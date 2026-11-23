use std::ffi::{OsStr, OsString};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use sumie_core::mouse::{Button, MouseEvent, MousePhase};
use sumie_core::{CellPosition, KeyAction, KeyModifiers, Pane, PaneCommand, TabAction};
use sumie_pty::{Command, Size, SpawnOptions};
use sumie_wayland::{Event, Modifiers};

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
        &mut input::Routing::default(),
        false,
        Event::ImePreedit {
            text: "not-a-command ".into(),
            cursor: None,
            generation: 0,
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
    let mut routing = input::Routing::default();
    for (physical, shifted, base, text) in [
        (33, xkeysym::key::F, xkeysym::key::f, "F"),
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
            let effect = input::route(
                &mut pane,
                &mut routing,
                false,
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
            match (physical, pressed, repeat) {
                (33, true, false) => {
                    assert_eq!(effect, input::Effect::Search(input::SearchAction::Open));
                }
                (47, true, false) => assert_eq!(effect, input::Effect::ReadClipboard),
                _ => (),
            }
            assert!(
                pane.pty.input_fd().is_none(),
                "matched key queued PTY input: {physical}, pressed={pressed}, repeat={repeat}"
            );
        }
    }
    tab_shortcuts_never_reach_the_pty(&mut pane, &mut routing);
    search_modal_routes(&mut pane, &mut routing);
    pane.command(PaneCommand::Close).unwrap();
    crate::sources::reap(&mut pane.pty).unwrap();
}

fn tab_shortcuts_never_reach_the_pty(pane: &mut LocalPane, routing: &mut input::Routing) {
    use KeyAction::{Press, Release, Repeat};
    use input::Effect;
    use xkeysym::key;

    let plain = Modifiers::default();
    let ctrl = Modifiers {
        ctrl: true,
        ..plain
    };
    let ctrl_shift = Modifiers {
        shift: true,
        ..ctrl
    };
    let alt = Modifiers { alt: true, ..plain };
    let chords = [
        (20, key::Cyrillic_TE, ctrl_shift, TabAction::Open),
        (17, key::W, ctrl_shift, TabAction::Close),
        (15, key::Tab, ctrl, TabAction::Next),
        (15, key::ISO_Left_Tab, ctrl_shift, TabAction::Previous),
        (109, key::Page_Down, ctrl, TabAction::Next),
        (104, key::Page_Up, ctrl, TabAction::Previous),
        (104, key::Page_Up, ctrl_shift, TabAction::MoveLeft),
        (109, key::Page_Down, ctrl_shift, TabAction::MoveRight),
    ];
    let digits = (0_u8..9).map(|index| {
        let action = if index == 8 {
            TabAction::Last
        } else {
            TabAction::Select(usize::from(index))
        };
        (u32::from(index + 2), u32::from(b'1' + index), alt, action)
    });
    for (physical, symbol, modifiers, tab_action) in chords.into_iter().chain(digits) {
        for (action, modifiers, expected) in [
            (Press, modifiers, Effect::Tab(tab_action)),
            (Repeat, plain, Effect::None),
            (Release, plain, Effect::None),
        ] {
            let event = key_event(physical, symbol, action, modifiers);
            assert_eq!(input::route(pane, routing, false, event).unwrap(), expected);
            assert!(pane.pty.input_fd().is_none(), "matched tab key: {physical}");
        }
    }

    let press = key_event(15, key::ISO_Left_Tab, Press, ctrl_shift);
    assert_eq!(
        input::route(pane, routing, true, press).unwrap(),
        Effect::Tab(TabAction::Previous)
    );
    let release = key_event(15, key::Tab, Release, plain);
    assert_eq!(
        input::route(pane, routing, false, release).unwrap(),
        Effect::None
    );
    assert!(pane.pty.input_fd().is_none());
}

fn search_modal_routes(pane: &mut LocalPane, routing: &mut input::Routing) {
    use KeyAction::{Press, Release, Repeat};
    use input::{Effect, SearchAction};
    use xkeysym::key;

    assert_eq!(routing.change_recipient(), 1);
    for symbol in [key::Return, key::KP_Enter, key::F3] {
        for newer in [false, true] {
            let modifiers = Modifiers {
                shift: newer,
                ..Default::default()
            };
            let event = key_event(28, symbol, Press, modifiers);
            let effect = input::route(pane, routing, true, event).unwrap();
            assert_eq!(effect, Effect::Search(SearchAction::Navigate { newer }));
        }
    }

    let plain = Modifiers::default();
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        ..plain
    };
    let commit = |generation| Event::ImeCommit {
        text: "水".into(),
        generation,
    };
    let preedit = |generation| Event::ImePreedit {
        text: "ㄅ".into(),
        cursor: None,
        generation,
    };
    let paste = |generation| Event::ClipboardText {
        text: "paste".into(),
        generation,
    };
    let escape = |action| key_event(1, key::Escape, action, plain);
    let typed = |action| key_event(30, key::a, action, plain);
    let paste_key = |action, modifiers| key_event(47, key::v, action, modifiers);
    let insert = |text: &str| Effect::Search(SearchAction::Insert(text.into()));
    let mut searching = true;
    for (event, expected) in [
        (paste(0), Effect::None),
        (preedit(0), Effect::None),
        (commit(1), insert("水")),
        (preedit(1), Effect::Preedit("ㄅ".into())),
        (paste_key(Press, ctrl_shift), Effect::ReadClipboard),
        (paste_key(Repeat, plain), Effect::None),
        (paste_key(Release, plain), Effect::None),
        (paste(1), insert("paste")),
        (typed(Press), insert("a")),
        (typed(Repeat), insert("a")),
        (escape(Press), Effect::Search(SearchAction::Close)),
        (escape(Repeat), Effect::None),
        (escape(Release), Effect::None),
        (typed(Release), Effect::None),
        (commit(1), Effect::None),
        (paste(1), Effect::None),
        (
            key_event(33, key::F, Press, ctrl_shift),
            Effect::Search(SearchAction::Open),
        ),
        (preedit(1), Effect::None),
        (commit(1), Effect::None),
        (paste(1), Effect::None),
        (commit(3), insert("水")),
    ] {
        let effect = input::route(pane, routing, searching, event).unwrap();
        assert!(pane.pty.input_fd().is_none());
        assert_eq!(effect, expected);
        if let Effect::Search(SearchAction::Open | SearchAction::Close) = effect {
            searching = !searching;
            routing.change_recipient();
        }
    }
}

fn key_event(physical: u32, keysym: u32, action: KeyAction, modifiers: Modifiers) -> Event {
    let pressed = !matches!(action, KeyAction::Release);
    Event::Key {
        serial: 1,
        physical,
        keysym,
        utf8: pressed.then(|| "a".into()),
        modifiers,
        pressed,
        repeat: matches!(action, KeyAction::Repeat),
    }
}

#[test]
fn search_pointer_only_scrolls_history() {
    use MousePhase::{Cancel, Motion, Press, Release};
    use input::{Effect, SearchAction};

    let mut pane = ready_pane(
        "stty -echo; printf '\\033[?1003;1006h'; seq 1 20; printf 'READY\\n'; \
         press=$(printf '\\033[<2;1;1M'); release=$(printf '\\033[<2;1;1m'); \
         IFS= read -r reports; \
         if [ \"$reports\" = \"$press$release$press$release\" ]; then \
             printf '\\033[?1003l\\033[?1049;1007hALT\\n'; \
         fi; exec cat",
        &request_environment(),
    );
    let event = |phase| MouseEvent {
        phase,
        cell: CellPosition { column: 0, row: 0 },
        pixel: [0.0; 2],
        modifiers: KeyModifiers::default(),
    };
    let right = Button::Right;
    let left = Button::Left;
    let mut routing = input::Routing::default();
    let close_hit = || Some(input::SearchTarget::Close);
    let scenarios = [
        (true, Press(right, 0), false, Effect::None),
        (true, Motion, false, Effect::None),
        (true, Release(right), false, Effect::None),
        (true, Cancel, false, Effect::None),
        (false, Press(right, 0), true, Effect::None),
        (true, Motion, false, Effect::None),
        (false, Release(right), true, Effect::None),
        (false, Press(right, 0), true, Effect::None),
        (true, Release(right), true, Effect::None),
        (true, Press(right, 0), false, Effect::None),
        (false, Release(right), false, Effect::None),
        (true, Press(left, 0), false, Effect::None),
        (true, Motion, false, Effect::None),
        (
            true,
            Release(left),
            false,
            Effect::Search(SearchAction::Close),
        ),
    ];
    let mut searching = false;
    for (next, phase, reported, expected) in scenarios {
        if next != searching {
            pane.reset_pointer_gestures();
            searching = next;
        }
        assert_eq!(
            routing
                .pointer(&mut pane, searching, event(phase), close_hit)
                .unwrap(),
            expected
        );
        assert_eq!(pane.pty.input_fd().is_some(), reported);
        assert!(pane.pty.flush_input().unwrap());
    }

    let wheel = event(MousePhase::Wheel(-120.0));
    assert!(pane.terminal.at_bottom().unwrap());
    assert_eq!(
        routing.pointer(&mut pane, true, wheel, || None).unwrap(),
        Effect::ViewportChanged
    );
    assert!(!pane.terminal.at_bottom().unwrap());
    assert!(pane.pty.input_fd().is_none());

    pane.command(PaneCommand::Text(b"\n")).unwrap();
    assert!(pane.pty.flush_input().unwrap());
    wait_for_line(&mut pane, "ALT");
    routing.pointer(&mut pane, true, wheel, || None).unwrap();
    assert!(pane.pty.input_fd().is_none());
    routing.pointer(&mut pane, false, wheel, || None).unwrap();
    assert!(
        pane.pty.input_fd().is_some(),
        "1007 must encode arrows outside search"
    );
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
        SpawnOptions::new(command, env, size),
        [12, 24],
        &sumie_config::Config::default(),
    )
    .unwrap();
    wait_for_line(&mut pane, "READY");
    pane
}

fn wait_for_line(pane: &mut LocalPane, expected: &str) {
    // Shared CI runners can take several seconds to start a shell under parallel tests.
    let deadline = Instant::now() + Duration::from_secs(15);
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
