//! The `wake` scenario: a write to a watched pipe reaches the handler while `AppKit` runs its
//! own loops. From `run_after_handler`, the main thread runs the default, event-tracking and
//! modal-panel modes for 300 ms each; another thread writes the pipe 100 ms into each.

use std::{
    cell::{Cell, RefCell},
    io::{self, ErrorKind, Read as _, Write as _},
    ops::ControlFlow,
    os::{fd::AsFd as _, unix::net::UnixStream},
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

use sumie_macos::{
    Platform, WindowContext,
    readiness::Readiness,
    runner::{self as native, RunLoopMode},
};

use crate::{
    Result,
    macos::sources::Watches,
    platform::{ExitWatches as _, PaneWatches as _},
};

const PANE: u64 = 1;
const INTERVAL: Duration = Duration::from_millis(300);
const WRITE_AT: Duration = Duration::from_millis(100);
const MODES: [RunLoopMode; 3] = [
    RunLoopMode::Default,
    RunLoopMode::EventTracking,
    RunLoopMode::ModalPanel,
];

type Arrival = (Instant, Option<RunLoopMode>);

struct Leg {
    mode: RunLoopMode,
    written: io::Result<Instant>,
    arrival: Option<Arrival>,
    end: Instant,
}

#[derive(Default)]
struct Probe {
    arrival: Cell<Option<Arrival>>,
    legs: RefCell<Vec<Leg>>,
}

pub(in crate::macos) struct Wake {
    watches: Watches,
    /// Nonblocking, like a PTY wake: a re-armed registration can report it after a drain.
    output: UnixStream,
    probe: Rc<Probe>,
    _window: Platform,
}

impl Wake {
    pub(in crate::macos) fn start(
        context: &WindowContext,
        readiness: &Readiness,
        window: Platform,
    ) -> Result<Self> {
        let (output, writer) = UnixStream::pair()?;
        output.set_nonblocking(true)?;
        let (exit, exit_writer) = UnixStream::pair()?;
        let mut watches = Watches::new(readiness.clone());
        watches.register_pane(PANE, output.as_fd(), exit.as_fd())?;
        let probe = Rc::new(Probe::default());
        let legs = Rc::clone(&probe);
        native::after_handler(context, move || {
            legs.run(&writer);
            // The pane's "child" exits; its exit report ends the scenario in the default mode.
            drop(exit_writer);
        });
        Ok(Self {
            watches,
            output,
            probe,
            _window: window,
        })
    }

    /// Handle `Event::Ready`. Breaks once the exit report arrives.
    pub(in crate::macos) fn ready(&mut self) -> Result<ControlFlow<()>> {
        self.watches.dispatch()?;
        let ready = self.watches.take_ready(PANE);
        if ready.output {
            let arrived = Instant::now();
            let mut drained = false;
            loop {
                match self.output.read(&mut [0; 16]) {
                    Ok(0) => break,
                    Ok(_) => drained = true,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error.into()),
                }
            }
            if drained && self.probe.arrival.get().is_none() {
                self.probe
                    .arrival
                    .set(Some((arrived, RunLoopMode::current())));
            }
        }
        if !ready.exit {
            return Ok(ControlFlow::Continue(()));
        }
        self.watches.unregister(PANE);
        for leg in self.probe.legs.borrow().iter() {
            leg.report()?;
        }
        println!("runner wake done");
        Ok(ControlFlow::Break(()))
    }
}

impl Probe {
    fn run(&self, writer: &UnixStream) {
        for mode in MODES {
            self.arrival.set(None);
            let start = Instant::now();
            let written = writer.try_clone().and_then(|mut writer| {
                thread::Builder::new()
                    .name("sumie-wake-writer".into())
                    .spawn(move || {
                        thread::sleep(WRITE_AT);
                        let at = Instant::now();
                        writer.write_all(b"w").map(|()| at)
                    })
            });
            mode.run_for(INTERVAL);
            let written = written.and_then(|thread| {
                thread
                    .join()
                    .unwrap_or_else(|_| Err(io::Error::other("writer panicked")))
            });
            self.legs.borrow_mut().push(Leg {
                mode,
                written,
                arrival: self.arrival.take(),
                end: start + INTERVAL,
            });
        }
    }
}

impl Leg {
    fn report(&self) -> Result<()> {
        let written = self.written.as_ref().map_err(ToString::to_string)?;
        let (latency, mode, inside) = match self.arrival {
            Some((at, mode)) => (
                at.saturating_duration_since(*written)
                    .as_nanos()
                    .to_string(),
                mode.map_or("other", RunLoopMode::label),
                at <= self.end,
            ),
            None => ("none".into(), "none", false),
        };
        println!(
            "runner wake {} {latency} {mode} {inside}",
            self.mode.label()
        );
        Ok(())
    }
}
