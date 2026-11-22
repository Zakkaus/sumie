use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use rustix::io::{self, retry_on_intr};
use rustix::process::{Pid, PidfdFlags, WaitId, WaitIdOptions, pidfd_open, waitid};

pub(crate) struct ExitWatch(OwnedFd);

impl ExitWatch {
    pub(crate) fn attach(pid: Pid) -> io::Result<Self> {
        Ok(Self(pidfd_open(pid, PidfdFlags::empty())?))
    }

    pub(crate) fn fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }

    pub(crate) fn try_reap(&self, _pid: Pid) -> io::Result<Option<ExitStatus>> {
        let status = retry_on_intr(|| {
            waitid(
                WaitId::PidFd(self.fd()),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
            )
        })?;
        Ok(status.map(|status| {
            let raw = if let Some(code) = status.exit_status() {
                code << 8
            } else {
                // Translate siginfo into the native wait status consumed by ExitStatus.
                let signal = status
                    .terminating_signal()
                    .expect("WEXITED reports only exits");
                signal | (i32::from(status.dumped()) << 7)
            };
            ExitStatus::from_raw(raw)
        }))
    }
}
