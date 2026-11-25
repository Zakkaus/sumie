//! Binds the shared window operations to the `AppKit` window, its readiness bridge and the
//! display-paced frame slot. Native events queue into one batch per run-loop turn.

use std::{collections::VecDeque, os::fd::BorrowedFd, time::Instant};

use sumie_core::{mouse::MousePhase, notify::Notification};
use sumie_macos::{
    Platform, Pointer,
    readiness::{Interest, Key, Readiness},
};

use super::{sources::Watches, surface::FrameSlot};
use crate::{
    Result,
    frame_window::FrameWindow,
    platform::{
        CaretBounds, CursorIcon, Event, ExitWatches, LogicalRect, PaneWatches, PointerEvent,
        Position, Ready, WindowOps,
    },
    view,
};

/// The frame slot's wake. Readiness timer IDs count up from 1, so this one never collides;
/// the driver's deadline uses the next one down.
const SLOT: Key = Key::new(u64::MAX, Interest::Timer);
/// Trackpad points per wheel notch, as Wayland counts logical units.
const POINTS_PER_NOTCH: f64 = 10.0;

/// What the window did since it opened, for the runner probe and the logs.
#[derive(Clone, Copy, Default)]
pub(super) struct Stats {
    /// Frame slots that left pending geometry for a later refresh: the GPU was busy.
    pub(super) deferred_configures: u64,
    pub(super) timeouts: u64,
}

pub(super) struct MacWindow {
    pub(super) watches: Watches,
    platform: Platform,
    readiness: Readiness,
    slot: FrameSlot,
    batch: VecDeque<Event>,
    generation: u64,
    focused: bool,
    occluded: bool,
    screen: Option<u32>,
    size: ([u32; 2], u32),
    title: Option<String>,
    text_cursor: Option<bool>,
    /// The runner probe names its window for the controller; terminal titles do not apply.
    pub(super) fixed_title: bool,
    pub(super) stats: Stats,
}

impl MacWindow {
    pub(super) fn new(platform: Platform, readiness: Readiness) -> Self {
        let mut window = Self {
            watches: Watches::new(readiness.clone()),
            slot: FrameSlot::new(platform.maximum_frames_per_second()),
            readiness,
            batch: VecDeque::new(),
            generation: 0,
            focused: platform.has_focus(),
            occluded: false,
            screen: platform.screen(),
            size: ([0; 2], 0),
            title: None,
            text_cursor: None,
            fixed_title: false,
            stats: Stats::default(),
            platform,
        };
        // The session starts at the first size, like a Wayland configure.
        window.batch.push_back(Event::Configured {
            activated: window.focused,
            suspended: false,
        });
        window.resized();
        window
    }

    pub(super) const fn platform(&self) -> &Platform {
        &self.platform
    }

    /// Drop the native window owner and keep the watches for children still closing.
    pub(super) fn into_watches(self) -> Watches {
        self.watches
    }

    /// Queue a native event for the next turn.
    pub(super) fn queue(&mut self, event: sumie_macos::Event) {
        use sumie_macos::Event as Native;
        match event {
            Native::Resized => self.resized(),
            Native::Moved => self.screen_changed(),
            Native::Focused(focused) => {
                self.focused = focused;
                self.configured();
            }
            Native::Occluded(occluded) if occluded != self.occluded => {
                self.occluded = occluded;
                self.end_backoff();
                self.configured();
            }
            Native::CloseRequested => self.batch.push_back(Event::CloseRequested),
            Native::KeyText(text) => self.commit_text(text),
            Native::PointerEntered(pointer) => self.batch.push_back(Event::PointerEntered {
                position: device_px(pointer.position),
                modifiers: pointer.modifiers,
            }),
            Native::Pointer(pointer) => {
                let event = self.pointer(pointer);
                self.batch.push_back(Event::Pointer(event));
            }
            _ => (),
        }
    }

    /// Text for the focused pane, stamped with the current input generation.
    pub(super) fn commit_text(&mut self, text: String) {
        self.batch.push_back(Event::ImeCommit {
            text,
            generation: self.generation,
        });
    }

    pub(super) fn deliver_events(
        &mut self,
        mut deliver: impl FnMut(Event, &mut Self) -> Result<bool>,
    ) -> Result<bool> {
        while let Some(event) = self.batch.pop_front() {
            let result = deliver(event, self);
            if !matches!(result, Ok(false)) {
                self.batch.clear();
                return result;
            }
        }
        Ok(false)
    }

    pub(super) const fn refresh(&self) -> std::time::Duration {
        self.slot.refresh()
    }

    fn resized(&mut self) {
        let device = self.platform.device_size();
        let scale = self.platform.scale();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "AppKit scale factors are small positive numbers"
        )]
        let scale120 = (scale * 120.0).round() as u32;
        if (device, scale120) == self.size || device.contains(&0) {
            return;
        }
        if self.size.1 != scale120 {
            self.slot
                .set_rate(self.platform.maximum_frames_per_second());
            self.end_backoff();
        }
        self.size = (device, scale120);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a point extent of a backing size that fits u32"
        )]
        let logical = device.map(|px| (f64::from(px) / scale).round() as u32);
        self.batch.push_back(Event::Resized {
            logical: logical.into(),
            device: device.into(),
            scale: scale120,
        });
        self.screen_changed();
    }

    fn screen_changed(&mut self) {
        let screen = self.platform.screen();
        if screen != self.screen {
            self.screen = screen;
            self.slot
                .set_rate(self.platform.maximum_frames_per_second());
            self.end_backoff();
        }
    }

    /// An occlusion, screen or Space change ends a drawable back-off; a Space switch reaches
    /// the window as an occlusion change.
    fn end_backoff(&mut self) {
        if self.slot.visibility_changed(Instant::now()) {
            self.readiness.remove(SLOT);
        }
    }

    fn configured(&mut self) {
        self.batch.push_back(Event::Configured {
            activated: self.focused,
            suspended: self.occluded,
        });
    }

    fn pointer(&self, pointer: Pointer) -> PointerEvent {
        let phase = match pointer.phase {
            MousePhase::Touch(pixels) => {
                MousePhase::Touch(pixels / self.platform.scale() * (120.0 / POINTS_PER_NOTCH))
            }
            phase => phase,
        };
        PointerEvent {
            phase,
            position: device_px(pointer.position),
            modifiers: pointer.modifiers,
        }
    }

    fn hold_slot(&self, until: Instant) {
        if let Err(error) = self.readiness.set_deadline(SLOT, until) {
            tracing::error!(%error, "could not arm the frame slot wake");
        }
    }
}

const fn device_px([x, y]: [f64; 2]) -> Position {
    Position::DevicePx(x, y)
}

impl FrameWindow for MacWindow {
    fn frame_slot_available(&self) -> bool {
        self.slot.available(Instant::now())
    }

    fn arm_frame_callback(&mut self) {
        self.platform.before_present();
        let wake = self.slot.present(Instant::now());
        self.hold_slot(wake);
    }
}

impl WindowOps for MacWindow {
    fn click_interval() -> Option<u32> {
        sumie_macos::double_click_interval()
    }

    fn hostname() -> Result<String> {
        Ok(nix::unistd::gethostname()?
            .into_string()
            .map_err(|_| "the hostname is not UTF-8")?)
    }

    fn logical_rect(bounds: CaretBounds) -> Option<LogicalRect> {
        view::surface_logical_rect(bounds)
    }

    fn recover_frame_slot(&mut self) {
        if self.slot.release(Instant::now()) {
            self.readiness.remove(SLOT);
        }
    }

    fn set_cursor_rectangle(&mut self, _: LogicalRect) {}

    fn set_pointer_icon(&mut self, icon: CursorIcon) {
        // AppKit rebuilds cursor rectangles on every call; the session calls once per frame.
        let text = matches!(icon, CursorIcon::Text);
        if self.text_cursor.replace(text) != Some(text) {
            self.platform.set_cursor_text(text);
        }
    }

    fn set_input_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    fn read_clipboard(&mut self, _: u64) {
        tracing::debug!("no pasteboard yet; dropped a paste request");
    }

    fn write_clipboard(&mut self, _: String) {
        tracing::debug!("no pasteboard yet; dropped a copy");
    }

    fn write_terminal_clipboard(&mut self, _: Option<Vec<u8>>) {}

    fn queue_title(&mut self, title: String) {
        if !self.fixed_title && self.title.as_ref() != Some(&title) {
            self.platform.set_title(&title);
            self.title = Some(title);
        }
    }

    fn notify(&mut self, _: Notification) {}

    fn set_background_effects(&mut self, _: bool, _: bool) -> bool {
        false
    }

    fn background_commit_needed(&self) -> bool {
        false
    }

    fn may_configure(&mut self, queue_empty: impl FnOnce() -> bool) -> bool {
        if queue_empty() {
            return true;
        }
        self.stats.deferred_configures += 1;
        let wake = self.slot.defer(Instant::now());
        self.hold_slot(wake);
        false
    }

    fn hold_after_timeout(&mut self) -> bool {
        self.stats.timeouts += 1;
        let wake = self.slot.timed_out(Instant::now());
        tracing::warn!(
            count = self.stats.timeouts,
            "Metal drawable acquire timed out; waiting before the next acquire"
        );
        self.hold_slot(wake);
        true
    }
}

impl ExitWatches for MacWindow {
    fn take_exit_ready(&mut self, id: u64) -> bool {
        self.watches.take_exit_ready(id)
    }

    fn unregister(&mut self, id: u64) {
        self.watches.unregister(id);
    }
}

impl PaneWatches for MacWindow {
    fn register_pane(
        &mut self,
        id: u64,
        output: BorrowedFd<'_>,
        exit: BorrowedFd<'_>,
    ) -> Result<()> {
        self.watches.register_pane(id, output, exit)
    }

    fn take_ready(&mut self, id: u64) -> Ready {
        self.watches.take_ready(id)
    }

    fn watch_input(&mut self, id: u64, fd: Option<BorrowedFd<'_>>) -> Result<()> {
        self.watches.watch_input(id, fd)
    }

    fn stop_output(&mut self, id: u64) {
        self.watches.stop_output(id);
    }

    fn watch_blink(&mut self, id: u64, enabled: bool) -> Result<()> {
        self.watches.watch_blink(id, enabled)
    }

    fn reset_blink(&mut self, id: u64) {
        self.watches.reset_blink(id);
    }

    fn cursor_hidden(&self, id: u64) -> bool {
        self.watches.cursor_hidden(id)
    }

    fn retire_pane(&mut self, id: u64) {
        self.watches.retire_pane(id);
    }
}
