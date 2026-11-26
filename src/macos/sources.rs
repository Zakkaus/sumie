//! Binds the shared readiness traits to the `sumie-macos` bridge thread.
//!
//! The bridge reports a key once and leaves it disabled; `dispatch` folds the reports into
//! per-pane flags on the main thread, and taking a flag re-arms its descriptor. An exit watch
//! therefore keeps reporting while the child stays unreaped, until `unregister`.

use std::collections::{HashMap, hash_map::Entry};
use std::os::fd::BorrowedFd;
use std::time::{Duration, Instant};

use sumie_macos::readiness::{Interest, Key, Readiness};

use crate::{
    Result,
    platform::{ExitWatches, PaneWatches, Ready, TimerSource},
};

const BLINK_PHASE: Duration = Duration::from_millis(600);
/// The configuration child's result socket and exit watch; pane IDs are separate keys.
const CONFIG_RESULTS: Key = Key::new(0, Interest::Config);
const CONFIG_EXIT: Key = Key::new(1, Interest::Config);

#[derive(Clone, Copy)]
struct BlinkPhase {
    deadline: Instant,
    hidden: bool,
}

#[derive(Default)]
struct PaneState {
    ready: Ready,
    output: bool,
    input: bool,
    blink: Option<BlinkPhase>,
}

/// Main-thread registrations by pane ID. Dropping it removes every registration.
pub(crate) struct Watches {
    readiness: Readiness,
    panes: HashMap<u64, PaneState>,
    /// Registered configuration keys, and whether any reported since the last take.
    config: Vec<Key>,
    config_ready: bool,
    /// Registered pasteboard helper keys, and whether any reported since the last take.
    pasteboard: Vec<Key>,
    pasteboard_ready: bool,
}

/// One-shot deadlines that only wake dispatch, like the Linux calloop timer.
pub(crate) struct Timers(Readiness);

impl TimerSource for Timers {
    type Token = Key;

    fn register(&self, at: Instant) -> Result<Self::Token> {
        Ok(self.0.timer(at)?)
    }

    fn cancel(&self, token: Self::Token) {
        self.0.remove(token);
    }
}

impl Watches {
    pub(crate) fn new(readiness: Readiness) -> Self {
        Self {
            readiness,
            panes: HashMap::new(),
            config: Vec::new(),
            config_ready: false,
            pasteboard: Vec::new(),
            pasteboard_ready: false,
        }
    }

    pub(crate) fn animation_timer(&self) -> Timers {
        Timers(self.readiness.clone())
    }

    /// Watch the configuration child's result socket and its exit report.
    pub(crate) fn register_config(
        &mut self,
        results: BorrowedFd<'_>,
        exit: BorrowedFd<'_>,
    ) -> Result<()> {
        assert!(self.config.is_empty(), "config already registered");
        self.readiness.watch(CONFIG_RESULTS, results)?;
        self.readiness
            .watch(CONFIG_EXIT, exit)
            .inspect_err(|_| self.readiness.remove(CONFIG_RESULTS))?;
        self.config = vec![CONFIG_RESULTS, CONFIG_EXIT];
        Ok(())
    }

    /// Stop watching the result socket after its EOF; the exit watch stays until reaped.
    pub(crate) fn stop_config_results(&mut self) {
        if let Some(index) = self.config.iter().position(|key| *key == CONFIG_RESULTS) {
            self.config.swap_remove(index);
            self.readiness.remove(CONFIG_RESULTS);
        }
    }

    pub(crate) fn unregister_config(&mut self) {
        for key in self.config.drain(..) {
            self.readiness.remove(key);
        }
        self.config_ready = false;
    }

    /// Whether the child's socket or exit watch reported; taking re-arms both.
    pub(crate) fn take_config_ready(&mut self) -> Result<bool> {
        let ready = std::mem::take(&mut self.config_ready);
        if ready {
            for key in &self.config {
                self.readiness.rearm(*key)?;
            }
        }
        Ok(ready)
    }

    /// Watch a pasteboard helper's descriptor: its socket ([`Interest::Pasteboard`] or
    /// [`Interest::PasteboardWrite`]) or its exit report.
    pub(crate) fn watch_pasteboard(&mut self, key: Key, fd: BorrowedFd<'_>) -> Result<()> {
        self.readiness.watch(key, fd)?;
        self.pasteboard.push(key);
        Ok(())
    }

    /// Stop watching a pasteboard key; unknown keys are ignored.
    pub(crate) fn stop_pasteboard(&mut self, key: Key) {
        if let Some(index) = self.pasteboard.iter().position(|watched| *watched == key) {
            self.pasteboard.swap_remove(index);
            self.readiness.remove(key);
        }
    }

    /// Whether a pasteboard key reported; taking re-arms them all.
    pub(crate) fn take_pasteboard_ready(&mut self) -> Result<bool> {
        let ready = std::mem::take(&mut self.pasteboard_ready);
        if ready {
            for key in &self.pasteboard {
                self.readiness.rearm(*key)?;
            }
        }
        Ok(ready)
    }

    /// Fold the bridge's reports into pane state. Call on `Event::Ready`, before servicing.
    pub(crate) fn dispatch(&mut self) -> Result<()> {
        for key in self.readiness.take() {
            if key.interest == Interest::Config {
                self.config_ready |= self.config.contains(&key);
                continue;
            }
            if matches!(
                key.interest,
                Interest::Pasteboard | Interest::PasteboardWrite
            ) {
                self.pasteboard_ready |= self.pasteboard.contains(&key);
                continue;
            }
            let Some(pane) = self.panes.get_mut(&key.id) else {
                continue;
            };
            match key.interest {
                Interest::Output => pane.ready.output = true,
                Interest::Input => pane.ready.input = true,
                Interest::Exit => pane.ready.exit = true,
                Interest::Blink => {
                    let Some(current) = pane.blink else {
                        continue;
                    };
                    let now = Instant::now();
                    if current.deadline <= now {
                        pane.blink = Some(BlinkPhase {
                            deadline: now + BLINK_PHASE,
                            hidden: !current.hidden,
                        });
                    }
                    let deadline = pane.blink.expect("blink phase").deadline;
                    self.readiness.set_deadline(key, deadline)?;
                }
                Interest::Config
                | Interest::Pasteboard
                | Interest::PasteboardWrite
                | Interest::Timer => (),
            }
        }
        Ok(())
    }

    fn pane(&mut self, id: u64) -> &mut PaneState {
        self.panes.get_mut(&id).expect("registered pane")
    }

    fn stop_io(&mut self, id: u64) {
        let pane = self.panes.get_mut(&id).expect("registered pane");
        for (on, interest) in [
            (std::mem::take(&mut pane.output), Interest::Output),
            (std::mem::take(&mut pane.input), Interest::Input),
            (pane.blink.take().is_some(), Interest::Blink),
        ] {
            if on {
                self.readiness.remove(Key::new(id, interest));
            }
        }
    }
}

impl Drop for Watches {
    fn drop(&mut self) {
        self.unregister_config();
        for key in self.pasteboard.drain(..) {
            self.readiness.remove(key);
        }
        for id in self.panes.keys().copied().collect::<Vec<_>>() {
            self.unregister(id);
        }
    }
}

impl ExitWatches for Watches {
    fn take_exit_ready(&mut self, id: u64) -> bool {
        self.take_ready(id).exit
    }

    fn unregister(&mut self, id: u64) {
        self.stop_io(id);
        self.panes.remove(&id);
        self.readiness.remove(Key::new(id, Interest::Exit));
    }
}

impl PaneWatches for Watches {
    fn register_pane(
        &mut self,
        id: u64,
        output: BorrowedFd<'_>,
        exit: BorrowedFd<'_>,
    ) -> Result<()> {
        let Entry::Vacant(entry) = self.panes.entry(id) else {
            panic!("pane already registered");
        };
        let output_key = Key::new(id, Interest::Output);
        self.readiness.watch(output_key, output)?;
        self.readiness
            .watch(Key::new(id, Interest::Exit), exit)
            .inspect_err(|_| self.readiness.remove(output_key))?;
        entry.insert(PaneState {
            output: true,
            ..PaneState::default()
        });
        Ok(())
    }

    // Re-arming at take means a descriptor the caller has not drained yet reports once more,
    // which costs one extra wake per batch. The terminal driver (U8) drains fully, so this
    // stays bounded; a reader that caps its reads must move the re-arm after its drain.
    fn take_ready(&mut self, id: u64) -> Ready {
        let ready = std::mem::take(&mut self.pane(id).ready);
        for (taken, interest) in [
            (ready.output, Interest::Output),
            (ready.input, Interest::Input),
            (ready.exit, Interest::Exit),
        ] {
            if taken && let Err(error) = self.readiness.rearm(Key::new(id, interest)) {
                // The kqueue rejected an update to a live registration: nothing will report.
                panic!("could not re-arm {interest:?} for pane {id}: {error}");
            }
        }
        ready
    }

    fn watch_input(&mut self, id: u64, fd: Option<BorrowedFd<'_>>) -> Result<()> {
        let key = Key::new(id, Interest::Input);
        match (self.pane(id).input, fd) {
            (false, Some(fd)) => {
                self.readiness.watch(key, fd)?;
                self.pane(id).input = true;
            }
            (true, None) => {
                self.readiness.remove(key);
                let pane = self.pane(id);
                pane.input = false;
                pane.ready.input = false;
            }
            _ => (),
        }
        Ok(())
    }

    fn stop_output(&mut self, id: u64) {
        let pane = self.pane(id);
        if std::mem::take(&mut pane.output) {
            pane.ready.output = false;
            self.readiness.remove(Key::new(id, Interest::Output));
        }
    }

    fn watch_blink(&mut self, id: u64, enabled: bool) -> Result<()> {
        let key = Key::new(id, Interest::Blink);
        match (enabled, self.pane(id).blink) {
            (true, None) => {
                let deadline = Instant::now() + BLINK_PHASE;
                self.readiness.set_deadline(key, deadline)?;
                self.pane(id).blink = Some(BlinkPhase {
                    deadline,
                    hidden: false,
                });
            }
            (false, Some(_)) => {
                self.readiness.remove(key);
                self.pane(id).blink = None;
            }
            _ => (),
        }
        Ok(())
    }

    fn reset_blink(&mut self, id: u64) {
        let blink = &mut self.pane(id).blink;
        if blink.is_some() {
            // Keep the registered deadline; dispatch honors this later one.
            *blink = Some(BlinkPhase {
                deadline: Instant::now() + BLINK_PHASE,
                hidden: false,
            });
        }
    }

    fn cursor_hidden(&self, id: u64) -> bool {
        self.panes[&id].blink.is_some_and(|phase| phase.hidden)
    }

    fn retire_pane(&mut self, id: u64) {
        self.stop_io(id);
        let pane = self.pane(id);
        pane.ready = Ready {
            exit: pane.ready.exit,
            ..Ready::default()
        };
    }
}

#[cfg(test)]
mod tests;
