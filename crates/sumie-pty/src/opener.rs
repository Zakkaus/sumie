use std::io;
use std::process::ExitStatus;

use sumie_core::LinkUri;

/// Start the desktop opener in a separate process group. A detached worker
/// spawns and reaps it; the caller never waits for exec or exit. The opener
/// receives `/dev/null` for stdio and inherits no other descriptors.
///
/// # Errors
/// Returns an error if the worker thread cannot start.
pub fn open_url(uri: LinkUri) -> io::Result<()> {
    std::thread::Builder::new()
        .name("sumie-link-opener".into())
        .spawn(move || {
            #[cfg(target_os = "linux")]
            let result = launch("/proc/self/exe", &["--internal-link-opener", uri.as_str()]);
            #[cfg(target_os = "macos")]
            let result = launch("/usr/bin/open", &[uri.as_str()]);
            if let Err(error) = result {
                tracing::warn!(%error, "desktop opener failed");
            }
        })?;
    Ok(())
}

fn launch(program: &str, args: &[&str]) -> io::Result<()> {
    let status = opener_status(program, args)?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(status.to_string()))
    }
}

#[cfg(target_os = "linux")]
fn opener_status(program: &str, args: &[&str]) -> io::Result<ExitStatus> {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .status()
}

#[cfg(target_os = "macos")]
fn opener_status(program: &str, args: &[&str]) -> io::Result<ExitStatus> {
    use std::ffi::CString;
    use std::os::unix::process::ExitStatusExt as _;

    use nix::fcntl::OFlag;
    use nix::spawn::{PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags, posix_spawn};
    use nix::sys::signal::{SigSet, Signal};
    use nix::sys::stat::Mode;
    use rustix::io::retry_on_intr;
    use rustix::process::{Pid, WaitOptions, waitpid};

    let mut actions = PosixSpawnFileActions::init()?;
    actions.add_open(0, c"/dev/null", OFlag::O_RDWR, Mode::empty())?;
    actions.add_dup2(0, 1)?;
    actions.add_dup2(0, 2)?;

    let mut attrs = PosixSpawnAttr::init()?;
    attrs.set_pgroup(nix::unistd::Pid::from_raw(0))?;
    let mut signals = SigSet::empty();
    signals.add(Signal::SIGPIPE);
    attrs.set_sigdefault(&signals)?;
    // Darwin inherits only the destinations named by the file actions above.
    attrs.set_flags(
        PosixSpawnFlags::POSIX_SPAWN_SETPGROUP
            | PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF
            | PosixSpawnFlags::from_bits_retain(nix::libc::POSIX_SPAWN_CLOEXEC_DEFAULT),
    )?;

    let argv = std::iter::once(program)
        .chain(args.iter().copied())
        .map(CString::new)
        .collect::<Result<Vec<_>, _>>()?;
    let env = std::env::vars_os()
        .map(|(key, value)| {
            let mut pair = Vec::with_capacity(key.len() + value.len() + 2);
            pair.extend_from_slice(key.as_encoded_bytes());
            pair.push(b'=');
            pair.extend_from_slice(value.as_encoded_bytes());
            CString::new(pair)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let pid = posix_spawn(program, &actions, &attrs, &argv, &env)?;
    let pid = Pid::from_raw(pid.as_raw()).expect("posix_spawn returns a positive PID");
    let (_, status) = retry_on_intr(|| waitpid(Some(pid), WaitOptions::empty()))?
        .expect("blocking waitpid returns a status");
    Ok(ExitStatus::from_raw(status.as_raw()))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::fs::File;
    use std::os::fd::AsRawFd as _;
    use std::path::Path;

    use rustix::io::{FdFlags, fcntl_dupfd_cloexec, fcntl_getfd, fcntl_setfd};

    #[test]
    fn opener_does_not_inherit_an_ambient_descriptor() {
        let file = File::open("/dev/null").unwrap();
        let fd = fcntl_dupfd_cloexec(file, 64).unwrap();
        fcntl_setfd(&fd, FdFlags::empty()).unwrap();

        let path = format!("/dev/fd/{}", fd.as_raw_fd());
        assert!(Path::new(&path).exists());
        super::launch("/bin/test", &["!", "-e", &path]).unwrap();
        assert_eq!(fcntl_getfd(&fd).unwrap(), FdFlags::empty());
    }
}
