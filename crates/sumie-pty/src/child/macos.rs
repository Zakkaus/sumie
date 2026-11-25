use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
use nix::sys::time::TimeSpec;
use rustix::io::{self, Errno, retry_on_intr};
use rustix::process::{Pid, WaitOptions, WaitStatus, waitpid};

/// `fd()` is readable while an exit report waits; `try_reap` consumes one report.
/// Until the child is reaped, every consumed report is followed by another.
pub(crate) struct ExitWatch {
    queue: Kqueue,
}

/// How soon to look again after `NOTE_EXIT` arrived before the child became waitable.
const REWATCH_MS: isize = 1;

impl ExitWatch {
    pub(crate) fn attach(pid: Pid) -> io::Result<Self> {
        let watch = Self {
            queue: Kqueue::new().map_err(errno)?,
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
            Err(Errno::SRCH) => watch.rewatch()?,
            Err(err) => return Err(err),
        }
        Ok(watch)
    }

    pub(crate) fn fd(&self) -> BorrowedFd<'_> {
        self.queue.as_fd()
    }

    pub(crate) fn try_reap(&mut self, pid: Pid) -> io::Result<Option<ExitStatus>> {
        self.try_reap_with(|| retry_on_intr(|| waitpid(Some(pid), WaitOptions::NOHANG)))
    }

    fn try_reap_with(
        &mut self,
        wait: impl FnOnce() -> io::Result<Option<(Pid, WaitStatus)>>,
    ) -> io::Result<Option<ExitStatus>> {
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
        let status = wait()?;
        if status.is_none() {
            // NOTE_EXIT can arrive before XNU marks the child waitable. The one-shot proc
            // filter is gone and cannot be re-added to an exiting process, so look again soon.
            self.rewatch()?;
        }
        Ok(status.map(|(_, status)| ExitStatus::from_raw(status.as_raw())))
    }

    fn rewatch(&self) -> io::Result<()> {
        self.register(KEvent::new(
            0,
            EventFilter::EVFILT_TIMER,
            EvFlags::EV_ADD | EvFlags::EV_ONESHOT,
            FilterFlag::empty(),
            REWATCH_MS,
            0,
        ))
    }

    fn register(&self, event: KEvent) -> io::Result<()> {
        retry_on_intr(|| self.queue.kevent(&[event], &mut [], None).map_err(errno))?;
        Ok(())
    }
}

fn errno(err: nix::errno::Errno) -> Errno {
    Errno::from_raw_os_error(err as i32)
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    use super::*;

    fn reported(watch: &ExitWatch, within: Duration) -> bool {
        let timeout = Timespec::try_from(within).unwrap();
        let mut fds = [PollFd::from_borrowed_fd(watch.fd(), PollFlags::IN)];
        retry_on_intr(|| poll(&mut fds, Some(&timeout))).unwrap() == 1
    }

    #[test]
    fn early_note_exit_is_watched_until_reaped() {
        // The watch, not `std`, reaps the child.
        let id = Command::new("/usr/bin/true")
            .stdin(Stdio::null())
            .spawn()
            .map(|child| child.id())
            .unwrap();
        let pid = Pid::from_raw(id.cast_signed()).unwrap();
        let mut watch = ExitWatch::attach(pid).unwrap();
        assert!(reported(&watch, Duration::from_secs(5)), "no NOTE_EXIT");
        // The report arrives, but the child is not waitable yet.
        assert_eq!(watch.try_reap_with(|| Ok(None)).unwrap(), None);
        assert!(
            reported(&watch, Duration::from_secs(1)),
            "child not reaped: no exit report after an early NOTE_EXIT"
        );
        let status = watch
            .try_reap(pid)
            .unwrap()
            .expect("waitable after the re-watch");
        assert!(status.success());
    }
}
