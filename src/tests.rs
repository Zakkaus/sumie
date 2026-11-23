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
    let mut pane = ready_pane("stty -echo; printf 'READY\\n'; exec /bin/sh -i", &env);
    input::route(
        &mut pane,
        &mut sumie_core::Shortcuts::default(),
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

#[test]
fn bracketed_paste_fits_or_leaves_the_queue_unchanged() {
    use sumie_pty::INPUT_LIMIT;
    let mut pane = ready_pane(
        "printf '\\033[?2004hREADY\\n'; exec cat",
        &request_environment(),
    );
    pane.command(PaneCommand::Text(b"x")).unwrap();
    // With one byte queued, the first framed paste exceeds the FIFO by one byte.
    let error = pane
        .command(PaneCommand::Paste("a".repeat(INPUT_LIMIT - 12)))
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::WouldBlock
    );
    pane.command(PaneCommand::Paste("b".repeat(INPUT_LIMIT - 13)))
        .unwrap();
    assert_eq!(
        pane.pty.enqueue_input(b"!").unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    pane.command(PaneCommand::Close).unwrap();
    crate::sources::reap(&mut pane.pty).unwrap();
}

#[test]
fn matched_shortcuts_never_reach_the_pty() {
    // A non-Latin layout supplies printable text even with Ctrl+Shift held.
    // Release the modifiers first so Kitty can also encode the key release.
    let mut pane = ready_pane(
        "printf '\\033[>11uREADY\\n'; exec cat",
        &request_environment(),
    );
    let mut shortcuts = sumie_core::Shortcuts::default();
    for (physical, shifted, base, text) in [
        (
            46,
            xkeysym::key::Cyrillic_ES,
            xkeysym::key::Cyrillic_es,
            "С",
        ),
        (
            47,
            xkeysym::key::Cyrillic_EM,
            xkeysym::key::Cyrillic_em,
            "М",
        ),
    ] {
        for (pressed, repeat) in [(true, false), (true, true), (false, false)] {
            input::route(
                &mut pane,
                &mut shortcuts,
                Event::Key {
                    physical,
                    keysym: if pressed { shifted } else { base },
                    utf8: pressed.then(|| text.into()),
                    modifiers: sumie_wayland::Modifiers {
                        ctrl: pressed,
                        shift: pressed,
                        ..Default::default()
                    },
                    pressed,
                    repeat,
                    serial: u32::from(!repeat),
                },
            )
            .unwrap();
            assert!(
                pane.pty.input_fd().is_none(),
                "matched key queued PTY input: {physical}, pressed={pressed}, repeat={repeat}"
            );
        }
    }
    pane.command(PaneCommand::Close).unwrap();
    crate::sources::reap(&mut pane.pty).unwrap();
}

fn ready_pane(script: &str, env: &std::collections::BTreeMap<OsString, OsString>) -> LocalPane {
    let args = ["-c", script].map(OsString::from);
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
    let mut pane = LocalPane::spawn(
        command,
        env,
        size,
        [12, 24],
        &sumie_config::Colors::default(),
    )
    .unwrap();
    wait_for_line(&mut pane, "READY");
    pane
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
