use std::{
    fs::File,
    io,
    os::fd::OwnedFd,
    process::{self, Child, Command, Stdio},
    time::{Duration, Instant},
};

use nix::{pty::openpty, sys::termios::tcgetattr};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::{Errno, FdFlags, fcntl_dupfd_cloexec, fcntl_setfd},
    process::{Pid, Signal, kill_process},
    stdio::dup2_stdout,
};

use super::{ENTER, LEAVE, Terminal, run};
use crate::{log::Log, model::Model};

const CASE: &str = "SUMIE_TEST_TERMINAL_EXIT";
const PANIC: &str = "settings terminal panic probe";
const ERROR: &str = "settings terminal entry failed";

#[test]
#[ignore = "run by the PTY exit tests"]
fn child_session() {
    match std::env::var(CASE).as_deref() {
        Ok("panic") => {
            let _terminal = Terminal::enter().unwrap();
            panic!("{PANIC}");
        }
        Ok("error") => {
            let stdout = fcntl_dupfd_cloexec(io::stdout(), 0).unwrap();
            dup2_stdout(File::open("/dev/null").unwrap()).unwrap();
            let result = Terminal::enter();
            dup2_stdout(stdout).unwrap();
            assert_eq!(
                result.err().and_then(|error| error.raw_os_error()),
                Some(Errno::BADF.raw_os_error()),
            );
            eprintln!("{ERROR}");
        }
        Ok("signal") => {
            let path = std::env::temp_dir().join(format!("sumie-pty-{}.toml", process::id()));
            let mut model = Model::new(path, Log::default());
            process::exit(i32::from(run(&mut model).unwrap()));
        }
        _ => panic!("missing PTY test case"),
    }
}

struct Session(Child);

impl Drop for Session {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn spawn(case: &str, slave: &OwnedFd) -> Session {
    let duplicate = || Stdio::from(fcntl_dupfd_cloexec(slave, 0).unwrap());
    Session(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "terminal::tests::child_session",
                "--ignored",
                "--nocapture",
            ])
            .env(CASE, case)
            .stdin(duplicate())
            .stdout(duplicate())
            .stderr(duplicate())
            .spawn()
            .unwrap(),
    )
}

fn read_until(master: &OwnedFd, output: &mut Vec<u8>, marker: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = [0; 4096];
    while !output.windows(marker.len()).any(|bytes| bytes == marker) {
        let timeout =
            Timespec::try_from(deadline.saturating_duration_since(Instant::now())).unwrap();
        let mut fds = [PollFd::new(master, PollFlags::IN)];
        assert!(
            poll(&mut fds, Some(&timeout)).unwrap() > 0,
            "missing {:?} in {:?}",
            String::from_utf8_lossy(marker),
            String::from_utf8_lossy(output),
        );
        let n = rustix::io::read(master, &mut buf).unwrap();
        assert_ne!(n, 0, "PTY closed before marker");
        output.extend_from_slice(&buf[..n]);
    }
}

fn pty() -> nix::pty::OpenptyResult {
    let pty = openpty(None, None).unwrap();
    fcntl_setfd(&pty.master, FdFlags::CLOEXEC).unwrap();
    fcntl_setfd(&pty.slave, FdFlags::CLOEXEC).unwrap();
    pty
}

#[test]
fn restores_after_error() {
    let pty = pty();
    let original = tcgetattr(&pty.slave).unwrap();
    let mut child = spawn("error", &pty.slave);
    read_until(&pty.master, &mut Vec::new(), ERROR.as_bytes());
    assert!(child.0.wait().unwrap().success());
    assert_eq!(tcgetattr(&pty.slave).unwrap(), original);
}

#[test]
fn restores_before_panic_diagnostic() {
    let pty = pty();
    let original = tcgetattr(&pty.slave).unwrap();
    let mut child = spawn("panic", &pty.slave);
    let mut output = Vec::new();
    read_until(&pty.master, &mut output, PANIC.as_bytes());
    assert!(!child.0.wait().unwrap().success());
    let diagnostic_at = output
        .windows(PANIC.len())
        .position(|bytes| bytes == PANIC.as_bytes())
        .unwrap();
    assert!(
        output[..diagnostic_at]
            .windows(LEAVE.len())
            .any(|bytes| bytes == LEAVE)
    );
    assert_eq!(tcgetattr(&pty.slave).unwrap(), original);
}

#[test]
fn restores_and_reports_signal_status() {
    for (signal, expected) in [(Signal::INT, 130), (Signal::TERM, 143), (Signal::HUP, 129)] {
        let pty = pty();
        let original = tcgetattr(&pty.slave).unwrap();
        let mut child = spawn("signal", &pty.slave);
        let mut output = Vec::new();
        read_until(&pty.master, &mut output, ENTER);
        assert_ne!(tcgetattr(&pty.slave).unwrap(), original);
        kill_process(Pid::from_child(&child.0), signal).unwrap();
        read_until(&pty.master, &mut output, LEAVE);
        assert_eq!(child.0.wait().unwrap().code(), Some(expected));
        assert_eq!(tcgetattr(&pty.slave).unwrap(), original);
    }
}
