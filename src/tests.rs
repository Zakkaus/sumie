use std::ffi::{OsStr, OsString};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use sumie_core::{KeyAction, KeyModifiers, Pane, PaneCommand};
use sumie_pty::{Command, Size};
use sumie_wayland::Event;

use crate::{
    input,
    pane::{LocalPane, request_environment},
};

#[test]
fn local_pane_executes_committed_input_without_preedit() {
    let mut env = request_environment();
    env.insert("ENV".into(), "/dev/null".into());
    env.insert("PS1".into(), "".into());
    let args = ["-c", "stty -echo; printf 'READY\\n'; exec /bin/sh -i"].map(OsString::from);
    let command = Command::Argv {
        program: OsStr::new("/bin/sh"),
        args: &args,
    };
    let size = Size {
        cols: 40,
        rows: 10,
        width_px: 480,
        height_px: 240,
    };
    let mut pane = LocalPane::spawn(command, &env, size, [12, 24]).unwrap();
    wait_for_line(&mut pane, "READY");
    input::route(
        &mut pane,
        Event::ImePreedit {
            text: "not-a-command ".into(),
            cursor: None,
        },
    )
    .unwrap();
    pane.command(PaneCommand::Text(b"echo hi")).unwrap();
    pane.command(PaneCommand::Key {
        physical: 28,
        modifiers: KeyModifiers::default(),
        action: KeyAction::Press,
        text: "",
        unshifted: None,
    })
    .unwrap();
    assert!(pane.pty.flush_input().unwrap());
    wait_for_line(&mut pane, "hi");
    pane.command(PaneCommand::Close).unwrap();
    crate::sources::reap(&mut pane.pty).unwrap();
}

fn wait_for_line(pane: &mut LocalPane, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = Timespec::try_from(remaining).unwrap();
        let ready = poll(
            &mut [PollFd::from_borrowed_fd(
                pane.pty.output_fd(),
                PollFlags::IN,
            )],
            Some(&timeout),
        )
        .unwrap();
        assert_ne!(
            ready, 0,
            "PTY did not produce the expected line {expected:?}"
        );
        let (_, eof) = pane.drain_output().unwrap();
        let snapshot = pane.snapshot().unwrap();
        if snapshot.rows.iter().any(|row| {
            row.iter()
                .flat_map(|cell| cell.text.chars())
                .eq(expected.chars())
        }) {
            return;
        }
        assert!(
            !eof && Instant::now() < deadline,
            "PTY did not produce the expected line {expected:?}"
        );
    }
}
