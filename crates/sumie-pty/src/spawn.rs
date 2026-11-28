//! Starts child processes so that none inherits a descriptor before it is close-on-exec.
//!
//! A descriptor created without `O_CLOEXEC` exists briefly without `FD_CLOEXEC`; a `fork` on
//! another thread in that window copies it into a child, which keeps it across `exec`.
//! Creating one holds this lock shared and every spawn holds it exclusively, so a creation
//! never overlaps a spawn, and two spawns cannot copy each other's internal pipes either.
//! Linux needs the lock too: `pty-process` opens the PTY master without `O_CLOEXEC` on every
//! platform. Darwin also has no `pipe2` or `SOCK_CLOEXEC`, so its wake pipes and socket pairs
//! take it; Linux creates those atomically. Atomic creators (`open` with `O_CLOEXEC`,
//! `F_DUPFD_CLOEXEC`, `kqueue`, which XNU never copies into a child) need no lock. A spawn
//! that bypasses this module must name every descriptor its child inherits, as `posix_spawn`
//! with `POSIX_SPAWN_CLOEXEC_DEFAULT` does for the link opener.

use std::io;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command};
use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

static FORK: RwLock<()> = RwLock::new(());

/// Hold while creating a descriptor that is not close-on-exec from the start.
pub(crate) fn creating() -> RwLockReadGuard<'static, ()> {
    FORK.read().unwrap_or_else(PoisonError::into_inner)
}

/// Hold while a child process is created.
pub(crate) fn spawning() -> RwLockWriteGuard<'static, ()> {
    #[cfg(test)]
    tests::CONTENDED.with_borrow(|signal| {
        if let Some(signal) = signal
            && matches!(FORK.try_write(), Err(std::sync::TryLockError::WouldBlock))
        {
            signal.send(()).unwrap();
        }
    });
    FORK.write().unwrap_or_else(PoisonError::into_inner)
}

/// Start `command` while no descriptor is being made close-on-exec.
///
/// # Errors
/// Returns the spawn error.
pub fn spawn(command: &mut Command) -> io::Result<Child> {
    let _spawning = spawning();
    command.spawn()
}

/// A connected socket pair whose ends are close-on-exec before any child can start.
///
/// # Errors
/// Returns the socket error.
pub fn socket_pair() -> io::Result<(UnixStream, UnixStream)> {
    #[cfg(target_os = "macos")]
    let _creating = creating();
    UnixStream::pair()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs::File;
    use std::os::fd::{AsFd, AsRawFd};
    use std::sync::mpsc;
    use std::thread;

    use rustix::io::{FdFlags, fcntl_dupfd_cloexec, fcntl_getfd, fcntl_setfd};

    use super::*;

    thread_local! {
        pub(super) static CONTENDED: RefCell<Option<mpsc::Sender<()>>> = const { RefCell::new(None) };
    }

    #[test]
    fn a_spawn_waits_for_a_descriptor_being_created() {
        let creating = creating();
        let fd = fcntl_dupfd_cloexec(File::open("/dev/null").unwrap(), 64).unwrap();
        fcntl_setfd(&fd, FdFlags::empty()).unwrap();
        let path = format!("/dev/fd/{}", fd.as_raw_fd());
        assert!(std::path::Path::new(&path).exists());
        let (signal, contended) = mpsc::channel();
        let child = thread::spawn(move || {
            CONTENDED.set(Some(signal));
            spawn(Command::new("/bin/test").args(["!", "-e", &path]))
                .unwrap()
                .wait()
                .unwrap()
        });
        // Without the lock, the sender drops on thread exit, after the child checked the fd.
        let _ = contended.recv();
        fcntl_setfd(&fd, FdFlags::CLOEXEC).unwrap();
        drop(creating);
        assert!(child.join().unwrap().success(), "child inherited {fd:?}");
    }

    #[test]
    fn socket_pair_ends_are_close_on_exec() {
        let (a, b) = socket_pair().unwrap();
        for end in [a.as_fd(), b.as_fd()] {
            assert!(fcntl_getfd(end).unwrap().contains(FdFlags::CLOEXEC));
        }
    }
}
