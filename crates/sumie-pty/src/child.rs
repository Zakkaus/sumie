use std::os::fd::BorrowedFd;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{self, Errno, retry_on_intr};
use rustix::process::{Pid, Signal, WaitOptions, kill_process_group, waitpid};

#[cfg(target_os = "linux")]
#[path = "child/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "child/macos.rs"]
mod platform;

pub(super) use platform::ExitWatch;

pub(super) struct Child {
    pid: Pid,
    watch: ExitWatch,
    deadline: Option<Instant>,
}

impl Child {
    pub(super) fn new(pid: Pid, watch: ExitWatch) -> Self {
        Self {
            pid,
            watch,
            deadline: None,
        }
    }

    pub(super) fn pid(&self) -> Pid {
        self.pid
    }

    pub(super) fn fd(&self) -> BorrowedFd<'_> {
        self.watch.fd()
    }

    pub(super) fn close(&mut self, grace: Duration) -> io::Result<()> {
        self.deadline = Some(Instant::now() + grace);
        signal_group(self.pid, Signal::HUP)
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) fn service(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.watch.try_reap(self.pid)?;
        if status.is_none() {
            self.escalate()?;
        }
        Ok(status)
    }

    fn escalate(&mut self) -> io::Result<()> {
        if self
            .deadline
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            self.deadline = None;
            tracing::warn!(
                pid = self.pid.as_raw_nonzero().get(),
                "PTY child exceeded close deadline; sending SIGKILL"
            );
            signal_group(self.pid, Signal::KILL)?;
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        loop {
            let timeout = self.deadline.map(|deadline| {
                Timespec::try_from(deadline.saturating_duration_since(Instant::now()))
                    .expect("PTY close deadline is at most five seconds away")
            });
            match poll(
                &mut [PollFd::from_borrowed_fd(self.fd(), PollFlags::IN)],
                timeout.as_ref(),
            ) {
                Ok(_) => (),
                Err(Errno::INTR) => continue,
                Err(err) => return Err(err),
            }
            if self.watch.try_reap(self.pid)?.is_some() {
                return Ok(());
            }
            if let Err(err) = self.escalate() {
                tracing::error!(%err, "could not signal detached PTY child");
            }
        }
    }
}

pub(super) enum Cleanup {
    Watched(Child),
    Unwatched(Pid),
}

impl Cleanup {
    pub(super) fn finish(self) {
        let result = match self {
            Self::Watched(child) => child.finish(),
            Self::Unwatched(pid) => reap_after_setup_failure(pid),
        };
        if let Err(err) = result {
            tracing::error!(%err, "could not reap detached PTY child");
        }
    }
}

fn reap_after_setup_failure(pid: Pid) -> io::Result<()> {
    if let Err(err) = signal_group(pid, Signal::KILL) {
        tracing::error!(%err, "could not stop PTY child after exit watcher setup failed");
    }
    retry_on_intr(|| waitpid(Some(pid), WaitOptions::empty()))?;
    Ok(())
}

fn signal_group(pid: Pid, signal: Signal) -> io::Result<()> {
    // The group leader is still ours to reap, so its PID cannot be recycled here.
    match kill_process_group(pid, signal) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(err) => Err(err),
    }
}
