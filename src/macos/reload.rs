//! Watches settings in a child process so that no read can block the main thread.
//!
//! The child is this executable in [`CHILD_ARG`] mode; its stdin is one end of a socket pair.
//! It loads the settings, writes each result as a frame (`reload::frame`), and waits for edits
//! with a 100 ms debounce. A FIFO or stalled include may block it in `load()` indefinitely.
//! The app reads frames without blocking from the readiness bridge, and cancels by closing its
//! end: the child exits on that EOF even while it is blocked, and SIGKILL follows at a deadline.
//! The app reaps it on exit readiness with `try_wait`; it never waits or joins.

use std::{
    io::{self, ErrorKind, Read as _, Write as _},
    os::{
        fd::{AsFd as _, OwnedFd},
        unix::net::UnixStream,
    },
    process::{self, Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use sumie_config::{Config, Error, Watch, WatchEvent, user_config_path};
use sumie_macos::{exit::ExitWatch, readiness::Key};

use super::sources::{Timers, Watches};
use crate::{
    Result,
    platform::TimerSource as _,
    reload::{
        self,
        frame::{self, Decoder, Frame, Kind},
    },
};

/// Selects the child mode on the command line.
pub(crate) const CHILD_ARG: &str = "--internal-config-watch";
const DEBOUNCE: Duration = Duration::from_millis(100);
/// A cancelled child exits on EOF at once unless it is stopped; then SIGKILL ends it.
const KILL_AFTER: Duration = Duration::from_millis(500);
/// One service call reads at most this much, so a flooding child cannot hold the main thread.
const DRAIN: usize = 2 * frame::MAX_BODY;

#[derive(Debug)]
pub(super) enum Update {
    Settings(Config),
    /// An invalid settings file, or a watcher fault; the active settings stay.
    Diagnostic(String),
}

/// The app's side of the watcher child. Call [`Reload::service`] on every `Event::Ready`.
pub(super) struct Reload {
    child: Child,
    exit: ExitWatch,
    /// Closed on cancellation or a fault; the child exits on its EOF.
    socket: Option<UnixStream>,
    decoder: Decoder,
    timers: Timers,
    kill: Option<(Instant, Key)>,
    reaped: bool,
}

/// The production child: this executable in watcher mode.
pub(super) fn command() -> io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg(CHILD_ARG);
    Ok(command)
}

impl Reload {
    /// Start the child and register its socket and exit watch. Does not wait for a result.
    pub(super) fn spawn(mut command: Command, watches: &mut Watches) -> Result<Self> {
        let (socket, remote) = sumie_pty::socket_pair()?;
        socket.set_nonblocking(true)?;
        let mut child =
            sumie_pty::spawn(command.stdin(OwnedFd::from(remote)).stdout(Stdio::null()))?;
        // The command holds the child's end; the child alone must keep it open.
        drop(command);
        let registered = (|| -> Result<ExitWatch> {
            let exit = ExitWatch::attach(&child)?;
            watches.register_config(socket.as_fd(), exit.as_fd())?;
            Ok(exit)
        })();
        let exit = match registered {
            Ok(exit) => exit,
            Err(error) => {
                let _ = child.kill();
                return Err(error);
            }
        };
        Ok(Self {
            child,
            exit,
            socket: Some(socket),
            decoder: Decoder::default(),
            timers: watches.animation_timer(),
            kill: None,
            reaped: false,
        })
    }

    /// Read complete results, reap an exited child and enforce the kill deadline. Never blocks.
    pub(super) fn service(&mut self, watches: &mut Watches) -> Result<Vec<Update>> {
        let mut updates = Vec::new();
        if self.reaped {
            return Ok(updates);
        }
        if watches.take_config_ready()? {
            let mut fault = self.read(&mut updates);
            if self.exit.try_reap(&mut self.child)?.is_some() {
                self.reaped = true;
                if fault.is_none() && self.socket.is_some() {
                    // Its writes are complete, but a copy of its end inherited by another
                    // process could hold off EOF, so do not wait for one.
                    let read = self.read(&mut updates);
                    fault = Some(read.unwrap_or_else(|| "the watcher exited".to_owned()));
                }
            }
            if let Some(fault) = fault {
                updates.push(Update::Diagnostic(format!(
                    "configuration watcher stopped: {fault}"
                )));
                self.close(watches)?;
            }
            if self.reaped {
                watches.unregister_config();
                if let Some((_, key)) = self.kill.take() {
                    self.timers.cancel(key);
                }
            }
        }
        if let Some((at, _)) = self.kill
            && at <= Instant::now()
        {
            self.kill = None;
            self.child.kill()?;
        }
        Ok(updates)
    }

    /// Cancel the child without waiting; keep calling `service` until [`Reload::reaped`].
    pub(super) fn close(&mut self, watches: &mut Watches) -> Result<()> {
        if self.socket.take().is_some() {
            // The bridge's duplicate goes too, so the child sees EOF.
            watches.stop_config_results();
            let at = Instant::now() + KILL_AFTER;
            self.kill = Some((at, self.timers.register(at)?));
        }
        Ok(())
    }

    pub(super) fn reaped(&self) -> bool {
        self.reaped
    }

    /// Drain the socket into `updates`; returns a fault that ends the watcher.
    fn read(&mut self, updates: &mut Vec<Update>) -> Option<String> {
        let socket = self.socket.as_mut()?;
        let mut buffer = [0; 16 * 1024];
        let mut frames = Vec::new();
        let mut budget = DRAIN;
        let fault = loop {
            if budget == 0 {
                // The bridge reports again while unread results remain.
                break None;
            }
            let limit = budget.min(buffer.len());
            match socket.read(&mut buffer[..limit]) {
                Ok(0) => {
                    break Some(self.decoder.finish().err().map_or_else(
                        || "the watcher exited".to_owned(),
                        |error| error.to_string(),
                    ));
                }
                Ok(read) => {
                    budget -= read;
                    if let Err(error) = self.decoder.feed(&buffer[..read], &mut frames) {
                        break Some(error.to_string());
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => (),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break None,
                Err(error) => break Some(error.to_string()),
            }
        };
        for frame in frames {
            match decode(frame) {
                Ok(update) => updates.push(update),
                Err(error) => return Some(error),
            }
        }
        fault
    }
}

impl Drop for Reload {
    fn drop(&mut self) {
        if let Some((_, key)) = self.kill.take() {
            self.timers.cancel(key);
        }
        // Never wait here: an unreaped child is killed and becomes the OS's to reap at exit.
        if !self.reaped && self.child.kill().is_ok() {
            let _ = self.child.try_wait();
        }
    }
}

fn decode(frame: Frame) -> Result<Update, String> {
    match frame.kind {
        Kind::Settings => Config::from_json(&frame.body)
            .map(Update::Settings)
            .map_err(|error| format!("malformed settings: {error}")),
        Kind::Invalid => String::from_utf8(frame.body)
            .map(Update::Diagnostic)
            .map_err(|error| format!("malformed diagnostic: {error}")),
        kind => Err(format!("unexpected frame kind {kind:?}")),
    }
}

/// Run the watcher child. Its stdin is the app's socket; it never touches `AppKit`.
pub(crate) fn watch_child() -> Result<()> {
    let socket = UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?);
    let mut watch = Watch::new(user_config_path(), &socket)?;
    // The app never writes, so a read returns only at its EOF, even while `load()` blocks.
    let cancel = socket.try_clone()?;
    thread::Builder::new()
        .name("sumie-config-cancel".into())
        .spawn(move || {
            let _ = (&cancel).read(&mut [0]);
            process::exit(0);
        })?;
    let result = (|| -> io::Result<()> {
        send(&socket, watch.load()?)?;
        let mut reload_at = None;
        loop {
            match watch.wait(reload_at)? {
                WatchEvent::Changed => reload_at = Some(Instant::now() + DEBOUNCE),
                WatchEvent::Control => return Ok(()),
                WatchEvent::Deadline => {
                    reload_at = None;
                    send(&socket, watch.load()?)?;
                }
            }
        }
    })();
    match result {
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}

fn send(mut socket: &UnixStream, result: Result<Config, Error>) -> io::Result<()> {
    let (kind, body) = match result {
        Ok(config) => (Kind::Settings, config.to_json()),
        Err(error) => (Kind::Invalid, reload::diagnostic(&error)),
    };
    let (kind, body) = if body.len() > frame::MAX_BODY {
        let limit = frame::MAX_BODY;
        let message = format!("sumie.toml:1:1: settings exceed {limit} bytes");
        (Kind::Invalid, message)
    } else {
        (kind, body)
    };
    socket.write_all(&frame::encode(kind, body.as_bytes()))
}

#[cfg(test)]
mod tests;
