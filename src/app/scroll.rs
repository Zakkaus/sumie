//! Smooth history scrolling: wheel and touchpad input become motion, and each frame turns
//! that motion into whole rows moved in the terminal plus a pixel offset below the row.

use std::time::{Duration, Instant};

use sumie_core::{mouse::MousePhase, scroll::Motion};

use super::{PaneState, PaneTab, Result, Session};

/// Rows one wheel notch scrolls, as in the stepped mode.
const ROWS_PER_NOTCH: f64 = 3.0;
const VALUE120_PER_NOTCH: f64 = 120.0;
/// One logical pixel of finger movement in `Touch` units.
const VALUE120_PER_LOGICAL_PX: f64 = 12.0;
const DEFAULT_REFRESH: f64 = 1.0 / 60.0;
/// A frame later than this is a stall, not motion to catch up on.
const MAX_STEP: f64 = 0.1;

#[derive(Default)]
pub(super) struct PaneScroll {
    motion: Motion,
    /// Pixels the content is scrolled up from its row boundary.
    sub: f64,
    /// The offset of the frame last built, which is what the pointer sees.
    shown: u32,
}

impl PaneScroll {
    pub(super) fn offset(&self) -> u32 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "below one row"
        )]
        {
            self.sub.round() as u32
        }
    }

    pub(super) const fn shown(&self) -> u32 {
        self.shown
    }

    pub(super) fn show(&mut self) {
        self.shown = self.offset();
    }

    /// Whether a frame is owed: the offset moved, or motion is still running.
    pub(super) fn pending(&self) -> bool {
        self.motion.active() || self.offset() != self.shown
    }

    /// Another input moved the viewport; drop the offset and any motion.
    pub(super) fn reset(&mut self) {
        self.motion.stop();
        self.sub = 0.0;
    }
}

impl PaneState {
    /// Take a wheel or touchpad event unless it belongs to the program or the stepped route.
    pub(super) fn smooth_wheel(
        &mut self,
        phase: MousePhase,
        scale: u32,
        now: Duration,
        reduce_motion: impl FnOnce() -> bool,
    ) -> bool {
        if !matches!(
            phase,
            MousePhase::Wheel(_) | MousePhase::Touch(_) | MousePhase::TouchEnd
        ) {
            return false;
        }
        if !self.pane.wheel_scrolls_history() || reduce_motion() {
            self.scroll.reset();
            return false;
        }
        match phase {
            MousePhase::Wheel(value) => {
                let px = value / VALUE120_PER_NOTCH * ROWS_PER_NOTCH * f64::from(self.cell[1]);
                self.scroll.motion.wheel(px);
            }
            MousePhase::Touch(value) => {
                let px = value / VALUE120_PER_LOGICAL_PX * f64::from(scale) / 120.0;
                self.scroll.motion.touch(px, now);
            }
            _ => self.scroll.motion.release(now),
        }
        true
    }
}

/// Frame time from presentation timestamps, so motion keeps its speed when a frame is late.
pub(super) struct FrameClock {
    origin: Instant,
    latest: Option<(Duration, Duration)>,
    previous: Option<Duration>,
    last_tick: Option<Instant>,
    refresh: f64,
}

impl FrameClock {
    pub(super) fn new() -> Self {
        Self {
            origin: Instant::now(),
            latest: None,
            previous: None,
            last_tick: None,
            refresh: DEFAULT_REFRESH,
        }
    }

    pub(super) fn elapsed(&self) -> Duration {
        self.origin.elapsed()
    }

    /// Feedback for a presented frame: its time and the predicted refresh interval.
    pub(super) fn presented(&mut self, at: Duration, refresh: Duration) {
        self.latest = Some((at, refresh));
    }

    /// Forget the previous frame when motion starts after a pause.
    pub(super) fn restart(&mut self) {
        self.latest = None;
        self.previous = None;
        self.last_tick = None;
    }

    /// Seconds to advance for the frame about to be drawn.
    pub(super) fn tick(&mut self, now: Instant) -> f64 {
        let fallback = self
            .last_tick
            .map_or(self.refresh, |last| now.duration_since(last).as_secs_f64());
        let dt = match (self.latest.take(), self.previous) {
            (Some((at, refresh)), previous) if previous.is_none_or(|previous| at > previous) => {
                if !refresh.is_zero() {
                    self.refresh = refresh.as_secs_f64();
                }
                self.previous = Some(at);
                previous.map_or(fallback, |previous| {
                    at.saturating_sub(previous).as_secs_f64()
                })
            }
            _ => fallback,
        };
        self.last_tick = Some(now);
        dt.clamp(0.001, MAX_STEP)
    }
}

impl PaneState {
    /// New output while pinned to the bottom ends any offset: the newest row stays whole.
    pub(super) fn follow_output(&mut self) -> Result<()> {
        if self.scroll.pending() && self.pane.terminal.at_bottom()? {
            self.scroll.reset();
        }
        Ok(())
    }

    /// Advance the motion by `dt` seconds. Returns whether whole rows moved.
    fn step_scroll(&mut self, dt: f64) -> Result<bool> {
        let moved = self.scroll.motion.step(dt);
        if moved == 0.0 {
            return Ok(false);
        }
        let cell = f64::from(self.cell[1]);
        let total = self.scroll.sub + moved;
        let mut rows = (total / cell).floor();
        let mut sub = total - rows * cell;
        let (above, below) = self.pane.terminal.scroll_extent()?;
        #[expect(clippy::cast_precision_loss, reason = "row counts are far below 2^53")]
        let (above, below) = (above as f64, below as f64);
        if rows < -above || rows > below || (rows >= below && sub > 0.0) {
            // The end of the history: stop there with a whole row showing.
            rows = rows.clamp(-above, below);
            sub = 0.0;
            self.scroll.motion.stop();
        }
        self.scroll.sub = sub;
        if rows == 0.0 {
            return Ok(false);
        }
        #[expect(clippy::cast_possible_truncation, reason = "clamped to the history")]
        self.pane.terminal.scroll_rows(rows as i32)?;
        self.snapshot_dirty = true;
        self.frame_dirty |= self.search.is_some() || self.selection.range().is_some();
        self.clear_selection();
        Ok(true)
    }
}

impl PaneTab {
    fn keep_scrolling(&mut self, reduce_motion: impl FnOnce() -> bool) -> bool {
        let animating = self
            .layout
            .panes
            .iter()
            .any(|&(id, _)| self.get(id).scroll.motion.active());
        if animating && reduce_motion() {
            for pane in &mut self.panes {
                pane.scroll.reset();
            }
            false
        } else {
            animating
        }
    }
}

impl Session {
    /// Switch smooth scrolling; turning it off drops every pane's offset so nothing stays shifted.
    pub(super) fn set_smooth_scroll(&mut self, on: bool) {
        if self.smooth_scroll && !on {
            for pane in self.all_panes_mut() {
                pane.scroll.reset();
                pane.frame_dirty = true;
            }
        }
        self.smooth_scroll = on;
    }

    /// Advance every visible pane's scroll motion for the frame about to be drawn.
    pub(super) fn step_scroll(
        &mut self,
        now: Instant,
        reduce_motion: impl FnOnce() -> bool,
    ) -> Result<()> {
        let tab = self.tabs.active_mut().expect("live window");
        if !tab.keep_scrolling(reduce_motion) {
            self.clock.restart();
            return Ok(());
        }
        let dt = self.clock.tick(now);
        let mut moved = false;
        for i in 0..tab.layout.panes.len() {
            moved |= tab.get_mut(tab.layout.panes[i].0).step_scroll(dt)?;
        }
        if moved {
            self.link_pointer.clear_link();
        }
        Ok(())
    }

    pub(super) fn scroll_pending(&self) -> bool {
        let tab = self.tabs.active().expect("live window");
        tab.layout
            .panes
            .iter()
            .any(|&(id, _)| tab.get(id).scroll.pending())
    }

    pub(super) fn show_scroll(&mut self) {
        let tab = self.tabs.active_mut().expect("live window");
        for i in 0..tab.layout.panes.len() {
            tab.get_mut(tab.layout.panes[i].0).scroll.show();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::app::{directory, links, tabs::SnapshotPace};
    use crate::input::{self, Effect};
    use crate::tests::{close, ready_pane};
    use sumie_core::{CellPosition, KeyModifiers, Selection, splits::Rect};

    fn pane_with_history() -> PaneState {
        let mut pane = ready_pane("printf 'READY\\n'; exec /bin/cat", &BTreeMap::new());
        for _ in 0..12 {
            pane.terminal.write(b"\r\n");
        }
        PaneState {
            id: 1,
            rect: Rect {
                x: 0,
                y: 0,
                width: 480,
                height: 240,
            },
            cell: [12, 24],
            viewport_px: [480, 240],
            exited: false,
            paste_recipient: None,
            search: None,
            viewer: None,
            selection: Selection::default(),
            selected_text: String::new(),
            pane,
            directory: directory::Directory::at_spawn(std::path::Path::new("/")),
            title: String::new(),
            snapshot: None,
            snapshot_dirty: false,
            snapshot_pace: SnapshotPace::default(),
            frame_dirty: false,
            activity_after: None,
            hover: links::Hover::default(),
            scroll: PaneScroll::default(),
            progress: None,
        }
    }

    #[test]
    fn reduced_motion_wheel_scrolls_three_rows_immediately() {
        let mut pane = pane_with_history();
        let wheel = MousePhase::Wheel(-120.0);
        if !pane.smooth_wheel(wheel, 120, Duration::ZERO, || true) {
            let mut routing = input::Routing::default();
            let event = input::mouse_event(
                wheel,
                KeyModifiers::default(),
                (CellPosition { column: 0, row: 0 }, [0.0; 2], true),
            );
            assert_eq!(
                routing
                    .pointer(&mut pane.pane, false, event, || None)
                    .unwrap(),
                Effect::ViewportChanged,
            );
        }
        assert_eq!(pane.pane.terminal.scroll_extent().unwrap().1, 3);
        assert!(!pane.scroll.pending());
        close(pane.pane);
    }

    #[test]
    fn enabling_reduced_motion_stops_an_in_flight_scroll() {
        let mut tab = PaneTab::new(pane_with_history());
        let pane = tab.focused_mut();
        assert!(pane.smooth_wheel(MousePhase::Wheel(-120.0), 120, Duration::ZERO, || false));
        pane.step_scroll(0.01).unwrap();
        pane.scroll.show();
        assert!(pane.scroll.shown() > 0);
        assert!(tab.keep_scrolling(|| false));
        assert!(!tab.keep_scrolling(|| true));
        let pane = tab.focused_mut();
        assert_eq!(pane.scroll.offset(), 0);
        pane.scroll.show();
        assert!(!pane.scroll.pending());
        close(tab.remove(1).pane);
    }
}
