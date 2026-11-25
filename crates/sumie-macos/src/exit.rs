//! Reports a helper child's exit through a descriptor the readiness bridge can watch, the way
//! `sumie-pty` reports a shell's. The caller owns the child; nothing here waits or blocks.

use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::process::{Child, ExitStatus};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
use nix::sys::time::TimeSpec;

/// How soon to look again after `NOTE_EXIT` arrived before the child became waitable.
const REWATCH_MS: isize = 1;

/// `fd()` is readable while an exit report waits; `try_reap` consumes one report.
/// Until the child is reaped, every consumed report is followed by another.
pub struct ExitWatch {
    queue: Kqueue,
}

impl ExitWatch {
    /// Watch `child` for `NOTE_EXIT`.
    ///
    /// # Errors
    /// Returns kqueue creation and registration errors.
    pub fn attach(child: &Child) -> io::Result<Self> {
        let queue = Kqueue::new()?;
        fcntl(&queue, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        let watch = Self { queue };
        let event = KEvent::new(
            child.id() as usize,
            EventFilter::EVFILT_PROC,
            EvFlags::EV_ADD | EvFlags::EV_ONESHOT,
            FilterFlag::NOTE_EXIT,
            0,
            0,
        );
        match watch.register(event) {
            Ok(()) => (),
            // Darwin can remove a short-lived child from the process lookup before registration.
            Err(Errno::ESRCH) => watch.rewatch()?,
            Err(error) => return Err(error.into()),
        }
        Ok(watch)
    }

    /// Consume one exit report, if any, and reap `child` without blocking.
    ///
    /// # Errors
    /// Returns kqueue and wait errors.
    pub fn try_reap(&self, child: &mut Child) -> io::Result<Option<ExitStatus>> {
        let mut events = [KEvent::new(
            0,
            EventFilter::EVFILT_PROC,
            EvFlags::empty(),
            FilterFlag::empty(),
            0,
            0,
        )];
        let available = loop {
            match self
                .queue
                .kevent(&[], &mut events, Some(*TimeSpec::new(0, 0).as_ref()))
            {
                Err(Errno::EINTR) => (),
                result => break result?,
            }
        };
        if available == 0 {
            return Ok(None);
        }
        let status = child.try_wait()?;
        if status.is_none() {
            // The one-shot proc filter is gone and cannot be re-added to an exiting process.
            self.rewatch()?;
        }
        Ok(status)
    }

    fn rewatch(&self) -> io::Result<()> {
        Ok(self.register(KEvent::new(
            0,
            EventFilter::EVFILT_TIMER,
            EvFlags::EV_ADD | EvFlags::EV_ONESHOT,
            FilterFlag::empty(),
            REWATCH_MS,
            0,
        ))?)
    }

    fn register(&self, event: KEvent) -> Result<(), Errno> {
        loop {
            match self.queue.kevent(&[event], &mut [], None) {
                Err(Errno::EINTR) => (),
                result => return result.map(drop),
            }
        }
    }
}

impl AsFd for ExitWatch {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.queue.as_fd()
    }
}
