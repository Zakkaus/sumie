//! Binds the shared window operations to the `AppKit` window, its readiness bridge, the
//! display-paced frame slot and the pasteboard helpers. Native events and finished pastes
//! queue into one batch per run-loop turn; keys, modifiers, focus loss and input-method events
//! pass the composition as they are delivered, after every input generation the session set
//! for the events before them.

use std::{collections::VecDeque, os::fd::BorrowedFd, time::Instant};

use native::{device_px, key_event, paste_event, wheel_phase};
use sumie_core::notify::Notification;
#[cfg(feature = "macos-ci")]
use sumie_macos::Symbol;
use sumie_macos::{
    Platform, Pointer, PointerPhase,
    ime::{Composition, Ime, Input},
    readiness::{Interest, Key, Readiness},
};

use super::{
    pasteboard::{Pasteboard, Request},
    sources::Watches,
    surface::FrameSlot,
};
use crate::{
    Result,
    frame_window::FrameWindow,
    platform::{
        CaretBounds, CursorIcon, Event, ExitWatches, LogicalRect, PaneWatches, PointerEvent, Ready,
        WindowOps,
    },
    view,
};

mod native;

/// The frame slot's wake. Readiness timer IDs count up from 1, so this one never collides;
/// the driver's deadline uses the next one down.
const SLOT: Key = Key::new(u64::MAX, Interest::Timer);

/// What the window did since it opened, for the runner probe and the logs.
#[derive(Clone, Copy, Default)]
pub(super) struct Stats {
    /// Frame slots that left pending geometry for a later refresh: the GPU was busy.
    pub(super) deferred_configures: u64,
    pub(super) timeouts: u64,
}

/// A batch entry; the native ones become events when delivered.
enum Queued {
    Event(Event),
    Key(sumie_macos::Key),
    /// An input-method event and the input source selected when it arrived.
    Ime(Ime, Option<String>),
    /// Ends the composition ahead of the focus event.
    FocusLost,
}

#[derive(Default)]
struct ImeWindow {
    /// Whether input methods compose while the window is key.
    enabled: bool,
    /// The candidate window's caret, in points.
    area: Option<LogicalRect>,
    /// A caret of a frame not yet known to be on screen.
    pending: Option<LogicalRect>,
}

pub(super) struct MacWindow {
    pub(super) watches: Watches,
    pasteboard: Pasteboard,
    platform: Platform,
    readiness: Readiness,
    slot: FrameSlot,
    batch: VecDeque<Queued>,
    #[cfg(feature = "macos-ci")]
    generation: u64,
    text_recipient: u64,
    composition: Composition,
    ime: ImeWindow,
    focused: bool,
    occluded: bool,
    screen: Option<u32>,
    size: ([u32; 2], u32),
    title: Option<String>,
    text_cursor: Option<bool>,
    /// Set by the runner, which names its window for the controller: terminal titles do not
    /// apply.
    #[cfg(feature = "macos-ci")]
    pub(super) fixed_title: bool,
    pub(super) stats: Stats,
}

impl MacWindow {
    pub(super) fn new(platform: Platform, readiness: Readiness) -> Result<Self> {
        let watches = Watches::new(readiness.clone());
        let mut window = Self {
            pasteboard: Pasteboard::new(&watches)?,
            watches,
            slot: FrameSlot::new(platform.maximum_frames_per_second()),
            readiness,
            batch: VecDeque::new(),
            #[cfg(feature = "macos-ci")]
            generation: 0,
            text_recipient: 0,
            composition: Composition::default(),
            ime: ImeWindow::default(),
            focused: platform.has_focus(),
            occluded: false,
            screen: platform.screen(),
            size: ([0; 2], 0),
            title: None,
            text_cursor: None,
            #[cfg(feature = "macos-ci")]
            fixed_title: false,
            stats: Stats::default(),
            platform,
        };
        // The session starts at the first size, like a Wayland configure.
        window.push(Event::Configured {
            activated: window.focused,
            suspended: false,
        });
        window.resized();
        Ok(window)
    }

    pub(super) const fn platform(&self) -> &Platform {
        &self.platform
    }

    /// Drop the native window owner and keep the watches and pasteboard helpers for children
    /// still closing.
    pub(super) fn into_parts(self) -> (Watches, Pasteboard) {
        (self.watches, self.pasteboard)
    }

    /// Queue finished pastes, stamped with their request's recipient, for this turn.
    pub(super) fn service_pasteboard(&mut self) -> Result<()> {
        for pasted in self.pasteboard.service(&mut self.watches)? {
            self.push(paste_event(pasted, self.text_recipient));
        }
        Ok(())
    }

    /// Cancel pasteboard requests: draining drops paste results.
    pub(super) fn close_pasteboard(&mut self) -> Result<()> {
        self.pasteboard.close(&mut self.watches)
    }

    /// Queue a native event for the next turn.
    pub(super) fn queue(&mut self, event: sumie_macos::Event) {
        use sumie_macos::Event as Native;
        match event {
            Native::Resized => self.resized(),
            Native::Moved => self.screen_changed(),
            Native::Focused(focused) => {
                if focused != self.focused {
                    self.focused = focused;
                    // Focus loss cancels the composition; it composes again once key.
                    if !focused {
                        self.batch.push_back(Queued::FocusLost);
                    }
                    self.platform.set_ime_allowed(self.ime.enabled && focused);
                    self.push(Event::KeyboardFocus(focused));
                }
                self.configured();
            }
            Native::Occluded(occluded) if occluded != self.occluded => {
                self.occluded = occluded;
                self.end_backoff();
                self.configured();
                if !occluded {
                    self.push(Event::Exposed);
                }
            }
            Native::CloseRequested => self.push(Event::CloseRequested),
            Native::Key(key) => self.batch.push_back(Queued::Key(key)),
            Native::ModifiersChanged(modifiers) => self.push(Event::ModifiersChanged(modifiers)),
            Native::Ime(ime) => self.ime(ime),
            Native::PointerEntered(pointer) => self.push(Event::PointerEntered {
                position: device_px(pointer.position),
                modifiers: pointer.modifiers,
            }),
            Native::Pointer(pointer) => {
                let event = self.pointer(pointer);
                self.push(Event::Pointer(event));
            }
            _ => (),
        }
    }

    /// Let input methods compose while the window is key. The terminal driver turns them on;
    /// the runner keeps them off unless asked, so its key checks post below the user's input
    /// source.
    pub(super) fn set_ime(&mut self, enabled: bool) {
        self.ime.enabled = enabled;
        self.platform.set_ime_allowed(enabled && self.focused);
    }

    /// Queue an input-method event for the composition that owns it when delivered.
    pub(super) fn ime(&mut self, ime: Ime) {
        self.batch
            .push_back(Queued::Ime(ime, Platform::input_source()));
    }

    /// Text for the focused recipient: the runner's `type`, as if typed without an input method.
    #[cfg(feature = "macos-ci")]
    pub(super) fn commit_text(&mut self, text: String) {
        self.push(Event::ImeCommit {
            text,
            recipient: self.text_recipient,
        });
    }

    /// Cmd+F, then a preedit of `text`, as one batch: the runner's `ime search`.
    #[cfg(feature = "macos-ci")]
    pub(super) fn search_then_preedit(&mut self, text: String) {
        use sumie_core::{KeyAction, KeyCode, KeyModifiers, PhysicalKey};
        let modifiers = KeyModifiers {
            super_key: true,
            ..KeyModifiers::default()
        };
        for (action, typed) in [
            (KeyAction::Press, Some("f".into())),
            (KeyAction::Release, None),
        ] {
            self.batch.push_back(Queued::Key(sumie_macos::Key {
                physical: PhysicalKey::Code(KeyCode::KeyF),
                symbol: Symbol::Character('f'),
                text: typed,
                modifiers,
                action,
            }));
        }
        let cursor = Some((text.len(), text.len()));
        self.ime(Ime::Preedit { text, cursor });
    }

    #[cfg(feature = "macos-ci")]
    pub(super) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn deliver_events(
        &mut self,
        mut deliver: impl FnMut(Event, &mut Self) -> Result<bool>,
    ) -> Result<bool> {
        let mut events = Vec::new();
        while let Some(queued) = self.batch.pop_front() {
            self.convert(queued, &mut events);
            for event in events.drain(..) {
                let result = deliver(event, self);
                if !matches!(result, Ok(false)) {
                    self.batch.clear();
                    return result;
                }
            }
        }
        Ok(false)
    }

    /// Pass a batch entry through the composition under the current input generation.
    fn convert(&mut self, queued: Queued, events: &mut Vec<Event>) {
        let mut inputs = Vec::new();
        match queued {
            Queued::Event(event) => {
                if let Event::ModifiersChanged(modifiers) = event {
                    self.composition.set_modifiers(modifiers);
                }
                events.push(event);
            }
            Queued::Key(key) => {
                self.composition.key(&key);
                inputs.push(Input::Key(key));
            }
            Queued::Ime(ime, source) => self.composition.ime(ime, source, &mut inputs),
            Queued::FocusLost => inputs.extend(self.composition.cancel()),
        }
        events.extend(inputs.into_iter().map(|input| match input {
            Input::Key(key) => key_event(key),
            Input::Preedit {
                text,
                cursor,
                generation,
            } => Event::ImePreedit {
                text,
                cursor,
                generation,
            },
            Input::Commit { text, recipient } => Event::ImeCommit { text, recipient },
        }));
    }

    fn push(&mut self, event: Event) {
        self.batch.push_back(Queued::Event(event));
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
        self.push(Event::Resized {
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
        self.push(Event::Configured {
            activated: self.focused,
            suspended: self.occluded,
        });
    }

    fn pointer(&self, pointer: Pointer) -> PointerEvent {
        let phase = match pointer.phase {
            PointerPhase::Mouse(phase) => phase,
            PointerPhase::Scroll(scroll) => wheel_phase(scroll, self.platform.scale()),
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

    fn reduce_motion(&self) -> bool {
        Platform::reduce_motion()
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

    /// The candidate window moves with the frame that shows the caret there: one that timed
    /// out leaves it at the caret still on screen.
    fn set_cursor_rectangle(&mut self, rect: LogicalRect) {
        self.ime.pending = Some(rect);
    }

    fn cursor_rectangle_presented(&mut self) {
        if let Some(rect) = self.ime.pending.take()
            && self.ime.area.replace(rect) != Some(rect)
        {
            self.platform
                .set_ime_cursor_area([rect.x, rect.y], [rect.width, rect.height]);
        }
    }

    fn set_pointer_icon(&mut self, icon: CursorIcon) {
        // AppKit rebuilds cursor rectangles on every call; the session calls once per frame.
        let text = matches!(icon, CursorIcon::Text);
        if self.text_cursor.replace(text) != Some(text) {
            self.platform.set_cursor_text(text);
        }
    }

    /// Cancel an older composition natively without retargeting its queued commit.
    fn set_input_generation(&mut self, generation: u64, recipient: u64) {
        if self.composition.set_generation(generation, recipient)
            && self.ime.enabled
            && self.focused
        {
            self.platform.cancel_composition();
        }
        #[cfg(feature = "macos-ci")]
        {
            self.generation = generation;
        }
        self.text_recipient = recipient;
    }

    fn read_clipboard(&mut self, recipient: u64) {
        self.pasteboard
            .request(Request::Read { recipient }, &mut self.watches);
    }

    fn read_primary(&mut self, _: u64) {}

    fn write_clipboard(&mut self, text: String, _: bool) {
        self.pasteboard
            .request(Request::Write(Some(text)), &mut self.watches);
    }

    /// OSC 52: a terminal may write or clear the pasteboard only while the window is key.
    fn write_terminal_clipboard(&mut self, bytes: Option<Vec<u8>>) {
        if !self.focused {
            tracing::debug!("dropped a terminal clipboard write without keyboard focus");
            return;
        }
        let Ok(text) = bytes.map(String::from_utf8).transpose() else {
            tracing::warn!("dropped a terminal clipboard write that is not UTF-8");
            return;
        };
        self.pasteboard
            .request(Request::Write(text), &mut self.watches);
    }

    fn queue_title(&mut self, title: String) {
        #[cfg(feature = "macos-ci")]
        if self.fixed_title {
            return;
        }
        if self.title.as_ref() != Some(&title) {
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
