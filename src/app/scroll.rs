//! Smooth history scrolling: wheel and touchpad input become motion, and each frame turns
//! that motion into whole rows moved in the terminal plus a pixel offset below the row.

use std::time::{Duration, Instant};

use sumie_core::{mouse::MousePhase, scroll::Motion};

use super::{PaneState, Result, Session};

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
    /// Whole rows staged for the attempt; retained for retry if it does not present.
    rows: i32,
    /// The offset of the frame last presented, which is what the pointer sees.
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
        self.rows = 0;
    }

    /// Whether a frame is owed: the offset moved, or motion is still running.
    pub(super) fn pending(&self) -> bool {
        self.motion.active() || self.rows != 0 || self.offset() != self.shown
    }

    /// Another input moved the viewport; drop the offset and any motion.
    pub(super) fn reset(&mut self) {
        self.motion.stop();
        self.sub = 0.0;
        self.rows = 0;
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
    /// Take a wheel or touchpad event as smooth scrolling. Returns false when the program or
    /// the stepped route owns it.
    pub(super) fn smooth_wheel(&mut self, phase: MousePhase, scale: u32, now: Duration) -> bool {
        if !matches!(
            phase,
            MousePhase::Wheel(_) | MousePhase::Touch(_) | MousePhase::TouchEnd
        ) {
            return false;
        }
        if !self.pane.wheel_scrolls_history() {
            self.scroll.reset();
            return false;
        }
        let cell = f64::from(self.cell[1]);
        match phase {
            MousePhase::Wheel(value) => {
                let px = value / VALUE120_PER_NOTCH * ROWS_PER_NOTCH * cell;
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

    /// New output while pinned to the bottom ends any offset: the newest row stays whole.
    pub(super) fn follow_output(&mut self) -> Result<()> {
        if self.scroll.pending() && self.pane.terminal.at_bottom()? {
            self.scroll.reset();
        }
        Ok(())
    }

    /// Advance the motion by `dt` seconds. Returns whether whole rows moved.
    fn step_scroll(&mut self, dt: Option<f64>) -> Result<bool> {
        let moved = dt.map_or(0.0, |dt| self.scroll.motion.step(dt));
        if moved == 0.0 && self.scroll.rows == 0 {
            return Ok(false);
        }
        let cell = f64::from(self.cell[1]);
        let total = f64::from(self.scroll.rows).mul_add(cell, self.scroll.sub) + moved;
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
        #[expect(clippy::cast_possible_truncation, reason = "clamped to the history")]
        let rows = rows as i32;
        self.scroll.rows = rows;
        if rows == 0 {
            return Ok(false);
        }
        self.pane.terminal.scroll_rows(rows)?;
        self.snapshot_dirty = true;
        self.frame_dirty |= self.search.is_some() || self.selection.range().is_some();
        self.clear_selection();
        Ok(true)
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
    pub(super) fn step_scroll(&mut self, now: Instant) -> Result<()> {
        let tab = self.tabs.active_mut().expect("live window");
        let animating = tab.layout.panes.iter().any(|&(id, _)| {
            let scroll = &tab.get(id).scroll;
            scroll.motion.active() || scroll.rows != 0
        });
        if !animating {
            self.clock.restart();
            return Ok(());
        }
        let dt = self.frame_retry.is_none().then(|| self.clock.tick(now));
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

    pub(super) fn rollback_scroll(&mut self) -> Result<()> {
        let tab = self.tabs.active_mut().expect("live window");
        for i in 0..tab.layout.panes.len() {
            let pane = tab.get_mut(tab.layout.panes[i].0);
            if pane.scroll.rows != 0 {
                pane.pane.terminal.scroll_rows(-pane.scroll.rows)?;
                pane.snapshot_dirty = true;
            }
        }
        Ok(())
    }
}
