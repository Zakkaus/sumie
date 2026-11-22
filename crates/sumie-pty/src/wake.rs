use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::io::{self, Errno, retry_on_intr};

pub(super) struct Wake {
    read: OwnedFd,
    #[cfg(target_os = "macos")]
    write: OwnedFd,
}

impl Wake {
    pub(super) fn new() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use rustix::event::{EventfdFlags, eventfd};

            let read = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?;
            Ok(Self { read })
        }
        #[cfg(target_os = "macos")]
        {
            use rustix::fs::{OFlags, fcntl_setfl};
            use rustix::io::{FdFlags, fcntl_setfd};

            let (read, write) = rustix::pipe::pipe()?;
            for fd in [&read, &write] {
                fcntl_setfd(fd, FdFlags::CLOEXEC)?;
                fcntl_setfl(fd, OFlags::NONBLOCK)?;
            }
            Ok(Self { read, write })
        }
    }

    pub(super) fn fd(&self) -> BorrowedFd<'_> {
        self.read.as_fd()
    }

    pub(super) fn notify(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        let (fd, bytes) = (&self.read, 1_u64.to_ne_bytes());
        #[cfg(target_os = "macos")]
        let (fd, bytes) = (&self.write, [1]);
        match retry_on_intr(|| io::write(fd, &bytes)) {
            Ok(_) | Err(Errno::AGAIN) => Ok(()),
            Err(err) => Err(err),
        }
    }

    pub(super) fn clear(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        let mut buf = [0; 8];
        #[cfg(target_os = "macos")]
        let mut buf = [0; 256];
        loop {
            match retry_on_intr(|| io::read(&self.read, &mut buf)) {
                Ok(_) if cfg!(target_os = "linux") => return Ok(()),
                Ok(0) | Err(Errno::AGAIN) => return Ok(()),
                Ok(_) => (),
                Err(err) => return Err(err),
            }
        }
    }
}
