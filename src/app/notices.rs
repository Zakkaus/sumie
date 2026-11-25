//! What a pane's bell, desktop notifications and progress reports do in the window.

use std::time::{Duration, Instant};

use sumie_core::notify::{Gate, Notification, Progress};
use sumie_vt::Event as TerminalEvent;

use super::{PaneState, PaneTab, Session};
use crate::platform::WindowOps;

/// A report the program stopped refreshing is dropped after this long (Ghostty does the same).
const PROGRESS_TTL: Duration = Duration::from_secs(15);

/// Where the servicing pane is, and the window-wide notification state.
pub(super) struct Alerts<'a> {
    pub(super) gate: &'a mut Gate,
    pub(super) now: Instant,
    /// The pane is on screen in a focused window, so the user already sees what it prints.
    pub(super) visible: bool,
    /// The pane's tab is not the active one, so a bell marks it like activity.
    pub(super) background: bool,
}

impl Alerts<'_> {
    /// A visible pane never notifies, and its request does not spend the rate budget.
    pub(super) fn admit(&mut self, request: &Notification) -> bool {
        !self.visible && self.gate.admit(self.now, request)
    }
}

/// What servicing one pane changed.
#[derive(Default)]
pub(super) struct Serviced {
    pub(super) exited: bool,
    pub(super) activity: bool,
    pub(super) progress: bool,
}

impl PaneTab {
    /// The focused pane's report, else the first other pane's.
    pub(super) fn progress(&self) -> Option<Progress> {
        self.focused()
            .progress
            .or_else(|| self.panes.iter().find_map(|pane| pane.progress))
            .map(|(report, _)| report)
    }
}

impl PaneState {
    /// Apply what the program printed since the last service to the window and the tab.
    pub(super) fn handle_events(
        &mut self,
        window: &mut impl WindowOps,
        active: bool,
        labels_changed: &mut bool,
        alerts: &mut Alerts,
        serviced: &mut Serviced,
    ) {
        for event in self.pane.terminal.drain_events() {
            match event {
                TerminalEvent::Clipboard(bytes) if active => window.write_terminal_clipboard(bytes),
                TerminalEvent::Clipboard(_) => (),
                TerminalEvent::Title(title) => {
                    *labels_changed |= self.title != title;
                    self.title = title;
                }
                TerminalEvent::Bell => serviced.activity |= alerts.background,
                TerminalEvent::Notification(request) => {
                    if alerts.admit(&request) {
                        window.notify(request);
                    }
                }
                TerminalEvent::Progress(report) => {
                    let report = report.map(|report| (report, alerts.now));
                    serviced.progress |=
                        self.progress.map(|(old, _)| old) != report.map(|(new, _)| new);
                    self.progress = report;
                }
            }
        }
    }

    /// A pane whose child exited has no work to report, and a stale report goes away.
    pub(super) fn settle_progress(&mut self, serviced: &mut Serviced, now: Instant) {
        if serviced.exited {
            serviced.progress |= self.progress.take().is_some();
        } else {
            serviced.progress |= expire(&mut self.progress, now);
        }
    }
}

impl Session {
    /// When the earliest shown report goes stale; `None` while no pane shows one, so an idle
    /// window has no timer.
    pub(super) fn progress_deadline(&self) -> Option<Instant> {
        self.tabs
            .as_slice()
            .iter()
            .flat_map(|tab| &tab.panes)
            .filter_map(|pane| pane.progress)
            .map(|(_, at)| at + PROGRESS_TTL)
            .min()
    }
}

/// Drop `progress` when it is older than the lifetime; true when it was dropped.
fn expire(progress: &mut Option<(Progress, Instant)>, now: Instant) -> bool {
    let stale = progress.is_some_and(|(_, at)| now.saturating_duration_since(at) >= PROGRESS_TTL);
    if stale {
        *progress = None;
    }
    stale
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn progress_expires_fifteen_seconds_after_its_last_update() {
        let start = Instant::now();
        let report = Progress {
            state: sumie_core::notify::ProgressState::Set,
            percent: Some(40),
        };
        // (seconds since the update, expired)
        for (secs, expected) in [(0, false), (14, false), (15, true), (60, true)] {
            let mut progress = Some((report, start));
            let now = start + Duration::from_secs(secs);
            assert_eq!(expire(&mut progress, now), expected, "{secs} s");
            assert_eq!(progress.is_none(), expected, "{secs} s");
        }
        assert!(!expire(&mut None, start));
    }

    #[test]
    fn visible_panes_are_silent_and_keep_the_rate_budget() {
        let start = Instant::now();
        let mut gate = Gate::default();
        // (milliseconds, visible, title, admitted)
        let table = [
            (0, true, "seen", false),
            (100, false, "away", true),
            (200, false, "limited", false),
            (300, true, "seen again", false),
            (1100, false, "later", true),
        ];
        for (ms, visible, title, admitted) in table {
            let mut alerts = Alerts {
                gate: &mut gate,
                now: start + Duration::from_millis(ms),
                visible,
                background: false,
            };
            let request = Notification::from_bytes(title.as_bytes(), b"");
            assert_eq!(alerts.admit(&request), admitted, "{title}");
        }
    }
}
