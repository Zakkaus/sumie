//! Starts child processes so that none inherits a descriptor before it is close-on-exec.
//!
//! Darwin has no `pipe2`, `SOCK_CLOEXEC` or close-on-exec `posix_openpt`, so such a
//! descriptor exists briefly without `FD_CLOEXEC`; a `fork` on another thread in that window
//! copies it into a child, which keeps it across `exec`. Creating one holds this lock shared
//! and every spawn holds it exclusively, so a creation never overlaps a spawn, and two spawns
//! cannot copy each other's internal pipes either. Atomic creators (`open` with `O_CLOEXEC`,
//! `F_DUPFD_CLOEXEC`, `kqueue`, which XNU never copies into a child) need no lock. A spawn that
//! bypasses this module must name every descriptor its child inherits, as `posix_spawn` with
//! `POSIX_SPAWN_CLOEXEC_DEFAULT` does for the link opener.

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
    let _creating = creating();
    UnixStream::pair()
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use rustix::io::{FdFlags, fcntl_getfd};

    use super::*;

    #[test]
    fn a_spawn_waits_for_a_descriptor_being_created() {
        let creating = creating();
        let (sender, started) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut child = spawn(&mut Command::new("/usr/bin/true")).unwrap();
            sender.send(()).unwrap();
            child.wait().unwrap();
        });
        assert!(
            started.recv_timeout(Duration::from_millis(200)).is_err(),
            "a child started while a descriptor was not yet close-on-exec"
        );
        drop(creating);
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn socket_pair_ends_are_close_on_exec() {
        let (a, b) = socket_pair().unwrap();
        for end in [a.as_fd(), b.as_fd()] {
            assert!(fcntl_getfd(end).unwrap().contains(FdFlags::CLOEXEC));
        }
    }
}
