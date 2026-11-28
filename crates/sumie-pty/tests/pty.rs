#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::fd::BorrowedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{Errno, retry_on_intr};
use rustix::process::{Signal, WaitOptions, waitpid};
use sumie_pty::{Command, Pty, Size, SpawnOptions};

const TIMEOUT: Duration = Duration::from_secs(3);
const SIZE: Size = Size {
    cols: 80,
    rows: 24,
    width_px: 800,
    height_px: 480,
};

#[test]
fn shell_echoes_queued_input_on_its_controlling_terminal() {
    let mut pty = shell("read x; echo got:$x >/dev/tty; exit 7", None);
    send(&mut pty, b"hello\n");
    assert_eq!(read_output(&pty, |_| false), b"hello\r\ngot:hello\r\n");
    assert_eq!(wait_for_exit(&mut pty, TIMEOUT).code(), Some(7));
}

#[test]
fn child_sees_new_terminal_size() {
    let mut pty = shell("/bin/stty size; read x; /bin/stty size", None);
    assert_eq!(
        read_output(&pty, |bytes| bytes.ends_with(b"\n")),
        b"24 80\r\n"
    );
    pty.resize(Size {
        rows: 41,
        cols: 103,
        width_px: 1030,
        height_px: 820,
    })
    .unwrap();
    send(&mut pty, b"\n");
    assert_eq!(read_output(&pty, |_| false), b"\r\n41 103\r\n");
    assert!(wait_for_exit(&mut pty, TIMEOUT).success());
}

#[test]
fn close_reaps_a_child_that_ignores_hangup() {
    let grace = Duration::from_millis(250);
    let mut pty = shell(
        "trap '' HUP; printf 'ready\\n'; exec /bin/sleep 60",
        Some(grace),
    );
    assert_eq!(
        read_output(&pty, |bytes| bytes.ends_with(b"\n")),
        b"ready\r\n"
    );
    let pid = pty.child_pid().unwrap();
    let start = Instant::now();
    pty.close().unwrap();
    let close_time = start.elapsed();
    let status = wait_for_exit(
        &mut pty,
        (grace + Duration::from_secs(1)).saturating_sub(close_time),
    );
    assert!(close_time < Duration::from_millis(100), "close blocked");
    assert_eq!(status.signal(), Some(Signal::KILL.as_raw()));
    assert_eq!(
        waitpid(Some(pid), WaitOptions::NOHANG).unwrap_err(),
        Errno::CHILD
    );
}

#[cfg(target_os = "linux")]
#[test]
fn child_exit_releases_watcher_with_or_without_status() {
    for externally_reaped in [false, true] {
        let mut pty = shell("exit 7", None);
        let until = Instant::now() + TIMEOUT;
        assert!(ready(pty.exit_fd().unwrap(), PollFlags::IN, until));
        if externally_reaped {
            let pid = pty.child_pid().unwrap();
            assert!(waitpid(Some(pid), WaitOptions::NOHANG).unwrap().is_some());
        }
        let code = pty
            .service_child()
            .unwrap()
            .and_then(|status| status.code());
        assert_eq!(code, (!externally_reaped).then_some(7));
        assert!(pty.exit_fd().is_none());
        assert!(pty.child_pid().is_none());
    }
}

#[test]
fn child_environment_is_exact() {
    let mut env = session_env();
    env.extend([
        ("TERM".into(), "caller-term".into()),
        ("COLORTERM".into(), "caller-color".into()),
        ("SUMIE_TEST".into(), "provided".into()),
    ]);
    let mut pty = Pty::spawn(SpawnOptions::new(
        Command::Argv {
            program: OsStr::new("/usr/bin/env"),
            args: &["-0".into()],
        },
        &env,
        SIZE,
    ))
    .unwrap();
    let output = read_output(&pty, |_| false);
    let mut actual: Vec<_> = output
        .strip_suffix(b"\0")
        .unwrap()
        .split(|byte| *byte == 0)
        .collect();
    actual.sort_unstable();
    let expected: Vec<_> = env
        .iter()
        .map(|(key, value)| {
            let mut line = key.as_bytes().to_vec();
            line.push(b'=');
            line.extend_from_slice(value.as_bytes());
            line
        })
        .collect();
    assert!(
        actual == expected,
        "child environment differs from the supplied map"
    );
    assert!(wait_for_exit(&mut pty, TIMEOUT).success());
}

fn session_env() -> BTreeMap<OsString, OsString> {
    let key = OsString::from("DBUS_SESSION_BUS_ADDRESS");
    std::env::var_os(&key).map_or_else(BTreeMap::new, |value| BTreeMap::from([(key, value)]))
}

fn shell(script: &str, grace: Option<Duration>) -> Pty {
    let env = session_env();
    let args = ["-c".into(), script.into()];
    let mut options = SpawnOptions::new(
        Command::Argv {
            program: OsStr::new("/bin/sh"),
            args: &args,
        },
        &env,
        SIZE,
    );
    if let Some(grace) = grace {
        options.close_grace = grace;
    }
    Pty::spawn(options).unwrap()
}

fn send(pty: &mut Pty, bytes: &[u8]) {
    pty.enqueue_input(bytes).unwrap();
    let until = Instant::now() + TIMEOUT;
    while let Some(fd) = pty.input_fd() {
        assert!(
            ready(fd, PollFlags::OUT, until),
            "input never became writable"
        );
        assert!(pty.flush_input().unwrap());
    }
}

fn read_output(pty: &Pty, done: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let mut output = Vec::new();
    let until = Instant::now() + TIMEOUT;
    loop {
        assert!(
            ready(pty.output_fd(), PollFlags::IN, until),
            "no output wake before timeout"
        );
        pty.clear_output_wake().unwrap();
        loop {
            match pty.try_recv_output() {
                Ok(chunk) => output.extend(chunk),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return output,
            }
        }
        if done(&output) {
            return output;
        }
    }
}

fn wait_for_exit(pty: &mut Pty, timeout: Duration) -> ExitStatus {
    let until = Instant::now() + timeout;
    loop {
        let wake_at = pty
            .close_deadline()
            .map_or(until, |deadline| deadline.min(until));
        let fd = pty.exit_fd().unwrap();
        assert!(
            ready(fd, PollFlags::IN, wake_at) || wake_at < until,
            "child did not exit before timeout"
        );
        if let Some(status) = pty.service_child().unwrap() {
            return status;
        }
    }
}

fn ready(fd: BorrowedFd<'_>, flags: PollFlags, until: Instant) -> bool {
    retry_on_intr(|| {
        let timeout = Timespec::try_from(until.saturating_duration_since(Instant::now())).unwrap();
        poll(&mut [PollFd::from_borrowed_fd(fd, flags)], Some(&timeout))
    })
    .unwrap()
        != 0
}
