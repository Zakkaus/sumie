use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
use nix::sys::time::TimeSpec;
use rustix::io::{self, Errno, retry_on_intr};
use rustix::process::{Pid, WaitOptions, waitpid};

pub(crate) struct ExitWatch {
    queue: Kqueue,
    retry_armed: bool,
}

impl ExitWatch {
    pub(crate) fn attach(pid: Pid) -> io::Result<Self> {
        let mut watch = Self {
            queue: Kqueue::new().map_err(errno)?,
            retry_armed: false,
        };
        let event = KEvent::new(
            usize::try_from(pid.as_raw_nonzero().get()).expect("positive child PID"),
            EventFilter::EVFILT_PROC,
            EvFlags::EV_ADD | EvFlags::EV_ONESHOT,
            FilterFlag::NOTE_EXIT,
            0,
            0,
        );
        match watch.register(event) {
            Ok(()) => (),
            // Darwin can remove a short-lived child from the process lookup before registration.
            Err(Errno::SRCH) => watch.arm_retry()?,
            Err(err) => return Err(err),
        }
        Ok(watch)
    }

    pub(crate) fn fd(&self) -> BorrowedFd<'_> {
        self.queue.as_fd()
    }

    pub(crate) fn try_reap(&mut self, pid: Pid) -> io::Result<Option<ExitStatus>> {
        let mut events = [KEvent::new(
            0,
            EventFilter::EVFILT_PROC,
            EvFlags::empty(),
            FilterFlag::empty(),
            0,
            0,
        )];
        let available = retry_on_intr(|| {
            self.queue
                .kevent(&[], &mut events, Some(*TimeSpec::new(0, 0).as_ref()))
                .map_err(errno)
        })?;
        if available == 0 {
            return Ok(None);
        }
        let status = retry_on_intr(|| waitpid(Some(pid), WaitOptions::NOHANG))?;
        if status.is_none() && !self.retry_armed {
            // NOTE_EXIT is delivered before XNU marks the child waitable.
            self.arm_retry()?;
        }
        Ok(status.map(|(_, status)| ExitStatus::from_raw(status.as_raw())))
    }

    fn arm_retry(&mut self) -> io::Result<()> {
        self.register(KEvent::new(
            0,
            EventFilter::EVFILT_TIMER,
            EvFlags::EV_ADD,
            FilterFlag::empty(),
            1,
            0,
        ))?;
        self.retry_armed = true;
        Ok(())
    }

    fn register(&self, event: KEvent) -> io::Result<()> {
        retry_on_intr(|| self.queue.kevent(&[event], &mut [], None).map_err(errno))?;
        Ok(())
    }
}

fn errno(err: nix::errno::Errno) -> Errno {
    Errno::from_raw_os_error(err as i32)
}
