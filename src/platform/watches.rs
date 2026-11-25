use std::os::fd::BorrowedFd;
use std::time::Instant;

use crate::Result;

#[derive(Clone, Copy, Default)]
pub(crate) struct Ready {
    pub(crate) output: bool,
    pub(crate) input: bool,
    pub(crate) exit: bool,
}

/// A one-shot deadline wakes dispatch; only a frame slot permits drawing.
#[cfg_attr(
    all(target_os = "macos", not(test)),
    expect(dead_code, reason = "the macOS terminal driver is not wired yet")
)]
pub(crate) trait TimerSource {
    type Token: Copy;

    fn register(&self, at: Instant) -> Result<Self::Token>;
    fn cancel(&self, token: Self::Token);
}

/// Exit-only operations remain available after the window has been dropped.
#[cfg_attr(
    all(target_os = "macos", not(test)),
    expect(dead_code, reason = "the macOS terminal driver is not wired yet")
)]
pub(crate) trait ExitWatches {
    fn take_exit_ready(&mut self, id: u64) -> bool;
    fn unregister(&mut self, id: u64);
}

/// The event-loop thread owns the watches, not the PTYs. Calls do not wait for I/O.
/// A failed registration leaves no watches for that ID.
/// Register before servicing a pane, sync input interest after dispatch, and keep
/// the exit watch through retirement until the child is reaped.
#[cfg_attr(
    all(target_os = "macos", not(test)),
    expect(dead_code, reason = "the macOS terminal driver is not wired yet")
)]
pub(crate) trait PaneWatches: ExitWatches {
    fn register_pane(
        &mut self,
        id: u64,
        output: BorrowedFd<'_>,
        exit: BorrowedFd<'_>,
    ) -> Result<()>;
    fn take_ready(&mut self, id: u64) -> Ready;
    fn watch_input(&mut self, id: u64, fd: Option<BorrowedFd<'_>>) -> Result<()>;
    fn stop_output(&mut self, id: u64);
    fn watch_blink(&mut self, id: u64, enabled: bool) -> Result<()>;
    fn reset_blink(&mut self, id: u64);
    fn cursor_hidden(&self, id: u64) -> bool;
    fn retire_pane(&mut self, id: u64);
}
