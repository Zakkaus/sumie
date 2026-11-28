//! Process draining: from the first Close, Quit or fatal error to the exit, its deadlines,
//! and the watchdog that ends the process if a stage never finishes.
//!
//! The clock starts at the first entry into draining and never resets. An ordinary drain (the
//! red button, Quit, the last shell's `exit`, a fatal error) leaves closing children their
//! shell grace period before SIGKILL. A system quit (log out, restart, shut down) must not
//! hold up the system: children get at most [`SYSTEM_KILL`], and `AppKit` gets its reply by
//! [`SYSTEM_REPLY`] even if some were not reaped; launchd inherits them. At [`HARD`]
//! ([`SYSTEM_HARD`]) the watchdog calls `_exit`: 0 for a system quit, 70 (`EX_SOFTWARE`)
//! otherwise. It does not log, so blocked stderr cannot delay the exit.
//!
//! Stages and counts are logged; payloads never are.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, Thread},
    time::{Duration, Instant},
};

use winit::platform::macos::QuitReason;

use super::Result;

/// How long an ordinary drain may take before the watchdog exits.
pub const HARD: Duration = Duration::from_secs(10);
/// The latest SIGKILL for a closing child during a system quit.
pub const SYSTEM_KILL: Duration = Duration::from_secs(3);
/// When a system quit replies, whether or not every child was reaped.
pub const SYSTEM_REPLY: Duration = Duration::from_secs(4);
/// How long a system quit may take before the watchdog exits.
pub const SYSTEM_HARD: Duration = Duration::from_secs(6);
/// `EX_SOFTWARE` from `sysexits.h`: an ordinary drain stalled.
const STALLED: i32 = 70;
const _: () = assert!(
    SYSTEM_KILL.as_millis() < SYSTEM_REPLY.as_millis()
        && SYSTEM_REPLY.as_millis() < SYSTEM_HARD.as_millis()
        && SYSTEM_HARD.as_millis() < HARD.as_millis()
);

/// Why `AppKit` asked the application to quit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quit {
    /// The Quit menu item, Cmd+Q, the Dock, or a quit Apple event without a reason.
    Ordinary,
    /// A quit Apple event with a reason: log out, restart or shut down.
    System,
}

impl From<QuitReason> for Quit {
    fn from(reason: QuitReason) -> Self {
        match reason {
            QuitReason::Ordinary => Self::Ordinary,
            QuitReason::System => Self::System,
        }
    }
}

/// What draining waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Panes and the configuration child got their signals; `orderOut:` is queued.
    Hiding,
    /// The window is off screen; the GPU queue drains before the surface drops.
    Releasing,
    /// Closing children keep their exit watches until reaped.
    Reaping,
    /// Everything is reaped or given up; `AppKit` or the loop ends the process.
    Exiting,
}

impl Stage {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Hiding => "hiding",
            Self::Releasing => "releasing",
            Self::Reaping => "reaping",
            Self::Exiting => "exiting",
        }
    }
}

/// The process's drain; inactive until [`begin`](Self::begin). The watchdog outlives this
/// value, so it also bounds the teardown after the loop returns.
#[derive(Default)]
pub struct Termination(Option<Drain>);

struct Drain {
    quit: Quit,
    stage: Option<(Stage, usize)>,
    words: Arc<Words>,
    watchdog: Thread,
}

/// All the watchdog reads.
struct Words {
    started: Instant,
    hard_ms: AtomicU64,
    system: AtomicBool,
    cancelled: AtomicBool,
}

impl Termination {
    /// Enter draining, or upgrade an ordinary drain to a system quit. The clock and the
    /// deadlines of an earlier entry stay.
    ///
    /// # Errors
    /// Returns an error if the watchdog thread cannot start.
    pub fn begin(&mut self, quit: Quit) -> Result<()> {
        match &mut self.0 {
            None => {
                let words = Arc::new(Words {
                    started: Instant::now(),
                    hard_ms: AtomicU64::new(millis(hard(quit))),
                    system: AtomicBool::new(quit == Quit::System),
                    cancelled: AtomicBool::new(false),
                });
                let watched = Arc::clone(&words);
                let watchdog = thread::Builder::new()
                    .name("sumie-watchdog".into())
                    .spawn(move || watch(&watched))?
                    .thread()
                    .clone();
                tracing::info!(reason = ?quit, hard_ms = millis(hard(quit)), "draining");
                self.0 = Some(Drain {
                    quit,
                    stage: None,
                    words,
                    watchdog,
                });
            }
            Some(drain) if quit > drain.quit => {
                drain.quit = quit;
                // Before the deadline: a watchdog that sees the new deadline must exit 0.
                drain.words.system.store(true, Ordering::Release);
                drain
                    .words
                    .hard_ms
                    .store(millis(SYSTEM_HARD), Ordering::Release);
                drain.watchdog.unpark();
                tracing::info!(hard_ms = millis(SYSTEM_HARD), "draining for a system quit");
            }
            Some(_) => (),
        }
        Ok(())
    }

    #[must_use]
    pub const fn draining(&self) -> bool {
        self.0.is_some()
    }

    /// Record the open stage and how many children or steps it waits for.
    pub fn stage(&mut self, stage: Stage, open: usize) {
        let Some(drain) = &mut self.0 else {
            return;
        };
        if drain.stage == Some((stage, open)) {
            return;
        }
        drain.stage = Some((stage, open));
        tracing::info!(
            stage = stage.name(),
            open,
            elapsed_ms = millis(drain.words.started.elapsed()),
            "draining stage"
        );
    }

    /// When a system quit replies even with children left; `None` for an ordinary drain.
    #[must_use]
    pub fn reply_by(&self) -> Option<Instant> {
        self.0
            .as_ref()
            .filter(|drain| drain.quit == Quit::System)
            .map(|drain| drain.words.started + SYSTEM_REPLY)
    }

    /// Stop the watchdog without exiting: the runner reopens its window in the same process.
    #[cfg(feature = "macos-ci")]
    pub fn cancel(&mut self) {
        if let Some(drain) = self.0.take() {
            drain.words.cancelled.store(true, Ordering::Release);
            drain.watchdog.unpark();
        }
    }
}

const fn hard(quit: Quit) -> Duration {
    match quit {
        Quit::Ordinary => HARD,
        Quit::System => SYSTEM_HARD,
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Sleep until the hard deadline, which can only move closer, then end the process.
fn watch(words: &Words) {
    loop {
        if words.cancelled.load(Ordering::Acquire) {
            return;
        }
        let hard = words.started + Duration::from_millis(words.hard_ms.load(Ordering::Acquire));
        match hard.checked_duration_since(Instant::now()) {
            Some(left) if !left.is_zero() => thread::park_timeout(left),
            _ => break,
        }
    }
    if words.cancelled.load(Ordering::Acquire) {
        return;
    }
    let system = words.system.load(Ordering::Acquire);
    signal_hook::low_level::exit(if system { 0 } else { STALLED });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_system_quit_replies_four_seconds_after_the_first_entry() {
        let mut termination = Termination::default();
        assert_eq!(termination.reply_by(), None);
        termination.begin(Quit::Ordinary).unwrap();
        let started = termination.0.as_ref().unwrap().words.started;
        assert_eq!(termination.reply_by(), None);
        termination.begin(Quit::System).unwrap();
        termination.begin(Quit::Ordinary).unwrap();
        let drain = termination.0.as_ref().unwrap();
        assert_eq!(drain.words.started, started);
        assert_eq!(drain.quit, Quit::System);
        assert_eq!(termination.reply_by(), Some(started + SYSTEM_REPLY));
        assert_eq!(
            drain.words.hard_ms.load(Ordering::Acquire),
            millis(SYSTEM_HARD)
        );
        // The test process must outlive this drain.
        drain.words.cancelled.store(true, Ordering::Release);
        drain.watchdog.unpark();
    }
}
