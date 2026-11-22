#![forbid(unsafe_code)]

//! Owns the shared Unix PTY, child processes, and reader threads for Linux and
//! macOS. Does not touch windows or render.

mod child;
mod reader;
mod wake;

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{self, IoSlice};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::process::ExitStatus;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::Pid;
use rustix::termios::{Winsize, tcsetwinsize};

use child::{Child, Cleanup, ExitWatch};
use reader::Reader;

const INPUT_LIMIT: usize = 1024 * 1024;
const MAX_CLOSE_GRACE: Duration = Duration::from_secs(5);

/// Child command. Login shells use the caller's `SHELL` and a login `argv[0]`.
#[derive(Clone, Copy, Debug)]
pub enum Command<'a> {
    LoginShell,
    Argv {
        program: &'a OsStr,
        args: &'a [OsString],
    },
}

/// Terminal dimensions in character cells and pixels.
#[derive(Clone, Copy, Debug)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub width_px: u16,
    pub height_px: u16,
}

impl From<Size> for Winsize {
    fn from(size: Size) -> Self {
        Self {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.width_px,
            ws_ypixel: size.height_px,
        }
    }
}

/// All spawn inputs come from the requester, not the process environment.
#[derive(Clone, Copy)]
pub struct SpawnOptions<'a> {
    pub command: Command<'a>,
    /// Complete child environment, including any `TERM` and `COLORTERM` values.
    pub env: &'a BTreeMap<OsString, OsString>,
    pub size: Size,
    /// Time between SIGHUP and SIGKILL. Must not exceed five seconds.
    pub close_grace: Duration,
}

impl<'a> SpawnOptions<'a> {
    #[must_use]
    pub const fn new(
        command: Command<'a>,
        env: &'a BTreeMap<OsString, OsString>,
        size: Size,
    ) -> Self {
        Self {
            command,
            env,
            size,
            close_grace: Duration::from_secs(3),
        }
    }

    fn command(self) -> Result<pty_process::blocking::Command, SpawnError> {
        use pty_process::blocking::Command as PtyCommand;

        let command = match self.command {
            Command::Argv { program, args } => PtyCommand::new(program).args(args),
            Command::LoginShell => {
                let shell = self.env.get(OsStr::new("SHELL")).ok_or(SpawnError::Shell)?;
                let name = Path::new(shell).file_name().ok_or(SpawnError::Shell)?;
                let mut arg0 = OsString::from("-");
                arg0.push(name);
                PtyCommand::new(shell).arg0(arg0)
            }
        };
        Ok(command.env_clear().envs(self.env))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("login shell requires a valid SHELL in the supplied environment")]
    Shell,
    #[error("close grace period exceeds five seconds")]
    CloseGrace,
    #[error("could not allocate a PTY or exec the child: {0}")]
    Spawn(#[from] pty_process::Error),
    #[error("could not set up PTY I/O or child monitoring: {0}")]
    Io(#[from] io::Error),
}

/// A live PTY and its child. Drive readiness and close deadlines from one event loop.
/// Do not wait on the child elsewhere or enable automatic SIGCHLD reaping.
pub struct Pty {
    master: Option<OwnedFd>,
    pending: VecDeque<u8>,
    reader: Reader,
    child: Option<Child>,
    close_grace: Duration,
}

impl Pty {
    /// Spawn the child with the PTY slave as its controlling terminal and stdio.
    /// This may wait for exec; live-pane operations do not wait for the child.
    ///
    /// # Errors
    /// Returns an error for invalid options or failed PTY, spawn, or watcher setup.
    pub fn spawn(options: SpawnOptions<'_>) -> Result<Self, SpawnError> {
        if options.close_grace > MAX_CLOSE_GRACE {
            return Err(SpawnError::CloseGrace);
        }
        let command = options.command()?;
        let (master, slave) = pty_process::blocking::open()?;
        let master: OwnedFd = master.into();
        setup_master(&master, options.size)?;
        let reader = Reader::start(master.try_clone()?)?;
        let mut pty = Self {
            master: Some(master),
            pending: VecDeque::new(),
            reader,
            child: None,
            close_grace: options.close_grace,
        };

        let pid = Pid::from_child(&command.spawn(slave)?);
        pty.child = match ExitWatch::attach(pid) {
            Ok(watch) => Some(Child::new(pid, watch)),
            Err(err) => {
                pty.reader.handoff(Cleanup::Unwatched(pid));
                return Err(SpawnError::Io(err.into()));
            }
        };
        Ok(pty)
    }

    /// The readable output wake descriptor. Unregister it on channel disconnection.
    #[must_use]
    pub fn output_fd(&self) -> BorrowedFd<'_> {
        self.reader.output_fd()
    }

    /// Reset the output wake before draining the channel, not after.
    ///
    /// # Errors
    /// Returns an error if reading the wake descriptor fails.
    pub fn clear_output_wake(&self) -> io::Result<()> {
        self.reader.clear_wake()
    }

    /// Take a chunk without waiting. `Disconnected` marks the end of output.
    ///
    /// # Errors
    /// Returns `Empty` if no chunk is available, or `Disconnected` at end of output.
    pub fn try_recv_output(&self) -> Result<Vec<u8>, TryRecvError> {
        self.reader.try_recv()
    }

    /// Enqueue a complete message without writing to or waiting for the PTY.
    ///
    /// # Errors
    /// Returns `WouldBlock` without accepting any bytes if the FIFO would exceed
    /// 1 MiB, or `BrokenPipe` after close.
    pub fn enqueue_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.master()?;
        if bytes.len() > INPUT_LIMIT - self.pending.len() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.pending.extend(bytes);
        Ok(())
    }

    /// Watch this descriptor for write readiness only while input is queued.
    #[must_use]
    pub fn input_fd(&self) -> Option<BorrowedFd<'_>> {
        self.master
            .as_ref()
            .filter(|_| !self.pending.is_empty())
            .map(AsFd::as_fd)
    }

    /// Attempt one nonblocking write, keeping the unwritten suffix in the FIFO.
    /// Returns `false` if the PTY is closed or its peer no longer accepts writes.
    ///
    /// # Errors
    /// Returns an error on other permanent write failures.
    pub fn flush_input(&mut self) -> io::Result<bool> {
        let Some(master) = &self.master else {
            return Ok(false);
        };
        if self.pending.is_empty() {
            return Ok(true);
        }
        let (head, tail) = self.pending.as_slices();
        match rustix::io::writev(master, &[IoSlice::new(head), IoSlice::new(tail)]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => drop(self.pending.drain(..written)),
            Err(Errno::AGAIN | Errno::INTR) => (),
            Err(Errno::IO | Errno::PIPE) => return Ok(false),
            Err(err) => return Err(err.into()),
        }
        Ok(true)
    }

    /// Apply cell and pixel dimensions to the PTY.
    ///
    /// # Errors
    /// Returns an error if `TIOCSWINSZ` fails or the master is closed.
    pub fn resize(&self, size: Size) -> io::Result<()> {
        Ok(tcsetwinsize(self.master()?, size.into())?)
    }

    /// The child PID while it is still owned by this handle.
    #[must_use]
    pub fn child_pid(&self) -> Option<Pid> {
        self.child.as_ref().map(Child::pid)
    }

    /// The readable pidfd on Linux or kqueue on macOS, until the child is reaped.
    #[must_use]
    pub fn exit_fd(&self) -> Option<BorrowedFd<'_>> {
        self.child.as_ref().map(Child::fd)
    }

    /// The event loop should call `service_child()` at this deadline, if present.
    #[must_use]
    pub fn close_deadline(&self) -> Option<Instant> {
        self.child.as_ref().and_then(Child::deadline)
    }

    /// On exit readiness or timer expiry, reap the child or escalate a pending close.
    /// A completed reap removes `exit_fd()`. An externally reaped child has no status.
    ///
    /// # Errors
    /// Returns an error if the wait, the exit event, or the signal fails.
    pub fn service_child(&mut self) -> io::Result<Option<ExitStatus>> {
        let Some(child) = &mut self.child else {
            return Ok(None);
        };
        let status = match child.service() {
            Ok(None) => return Ok(None),
            Ok(status) => status,
            Err(Errno::CHILD) => None,
            Err(err) => return Err(err.into()),
        };
        self.child = None;
        self.reader.release_cleanup();
        Ok(status)
    }

    /// Send SIGHUP, stop I/O, and schedule escalation. Does not wait or join.
    /// Retain the handle to receive the exit status, or drop it to detach cleanup.
    ///
    /// # Errors
    /// Returns an error if sending SIGHUP or waking the reader fails.
    pub fn close(&mut self) -> io::Result<()> {
        if self.master.is_none() {
            return Ok(());
        }
        let signal = self
            .child
            .as_mut()
            .map_or(Ok(()), |child| child.close(self.close_grace));
        self.master = None;
        self.pending = VecDeque::new();
        let stop = self.reader.stop();
        signal?;
        stop
    }

    fn master(&self) -> io::Result<&OwnedFd> {
        self.master
            .as_ref()
            .ok_or_else(|| io::ErrorKind::BrokenPipe.into())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Err(err) = self.close() {
            tracing::error!(%err, "PTY close failed during drop");
        }
        if let Some(child) = self.child.take() {
            self.reader.handoff(Cleanup::Watched(child));
        }
    }
}

fn setup_master(master: &OwnedFd, size: Size) -> io::Result<()> {
    fcntl_setfl(master, fcntl_getfl(master)? | OFlags::NONBLOCK)?;
    Ok(tcsetwinsize(master, size.into())?)
}
