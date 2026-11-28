use std::{
    os::fd::OwnedFd,
    process::{self, Child, Command, Stdio},
    time::{Duration, Instant},
};

use nix::{pty::openpty, sys::termios::tcgetattr};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::{FdFlags, fcntl_dupfd_cloexec, fcntl_setfd},
    process::{Pid, Signal, kill_process},
};

use super::{ENTER, LEAVE, Terminal, run};
use crate::model::Model;

const CASE: &str = "SUMIE_TEST_TERMINAL_EXIT";
const PANIC: &str = "settings terminal panic probe";

#[test]
#[ignore = "run by the PTY exit tests"]
fn child_session() {
    match std::env::var(CASE).as_deref() {
        Ok("panic") => {
            let _terminal = Terminal::enter().unwrap();
            panic!("{PANIC}");
        }
        Ok("signal") => {
            let path = std::env::temp_dir().join(format!("sumie-pty-{}.toml", process::id()));
            run(&mut Model::new(path)).unwrap();
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
fn restores_after_signal() {
    for signal in [Signal::INT, Signal::TERM, Signal::HUP] {
        let pty = pty();
        let original = tcgetattr(&pty.slave).unwrap();
        let mut child = spawn("signal", &pty.slave);
        let mut output = Vec::new();
        read_until(&pty.master, &mut output, ENTER);
        assert_ne!(tcgetattr(&pty.slave).unwrap(), original);
        kill_process(Pid::from_child(&child.0), signal).unwrap();
        read_until(&pty.master, &mut output, LEAVE);
        assert!(child.0.wait().unwrap().success());
        assert_eq!(tcgetattr(&pty.slave).unwrap(), original);
    }
}

#[test]
fn restores_keyboard_protocol_after_cancelling_recording() {
    let pty = pty();
    let mut child = spawn("signal", &pty.slave);
    let mut output = Vec::new();
    read_until(&pty.master, &mut output, ENTER);
    rustix::io::write(&pty.master, b"5\r").unwrap();
    read_until(&pty.master, &mut output, b"\x1b[=3u");
    rustix::io::write(&pty.master, b"\x1b[27u").unwrap();
    read_until(&pty.master, &mut output, b"\x1b[=0u");
    rustix::io::write(&pty.master, b"q").unwrap();
    read_until(&pty.master, &mut output, b"\x1b[<u");
    assert!(child.0.wait().unwrap().success());
}
