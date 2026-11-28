mod keyboard;

use std::ffi::{OsStr, OsString};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use sumie_core::mouse::{Button, MouseEvent, MousePhase};
use sumie_core::{
    CellPosition, KeyAction, KeyCode as K, KeyModifiers, Pane, PaneCommand, PhysicalKey,
};
use sumie_pty::{Command, Size, SpawnOptions};

use crate::{
    input,
    pane::{LocalPane, request_environment},
    platform::{Event, LogicalKey as L, Modifiers},
};

#[test]
fn local_pane_executes_committed_input_without_preedit() {
    let mut env = request_environment();
    env.insert("ENV".into(), "/dev/null".into());
    env.insert("PS1".into(), "".into());
    let mut pane = ready_pane("stty -echo; printf 'READY\\n'; exec /bin/sh -i", &env);
    input::route(
        (0, 0),
        &mut pane,
        &mut input::Routing::default(),
        false,
        Event::ImePreedit {
            text: "not-a-command ".into(),
            cursor: None,
            generation: 0,
        },
        input::Scope::Single,
    )
    .unwrap();
    pane.command(PaneCommand::Text(b"echo hi")).unwrap();
    pane.command(PaneCommand::Key {
        physical: PhysicalKey::Code(K::Enter),
        modifiers: KeyModifiers::default(),
        action: KeyAction::Press,
        text: "",
        unshifted: None,
    })
    .unwrap();
    assert!(pane.pty.flush_input().unwrap());
    wait_for_line(&mut pane, "hi");
    close(pane);
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
    close(pane);
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
    for (physical, shifted, base) in [
        (K::KeyE, 'F', 'f'), // Colemak
        (K::KeyC, 'С', 'с'), // Cyrillic Es
        (K::KeyV, 'М', 'м'), // Cyrillic Em
    ] {
        for (pressed, repeat) in [(true, false), (true, true), (false, false)] {
            let effect = input::route(
                (0, 0),
                &mut pane,
                &mut routing,
                false,
                Event::Key {
                    physical: PhysicalKey::Code(physical),
                    logical: L::Character(if pressed { shifted } else { base }),
                    utf8: pressed.then(|| String::from(shifted)),
                    modifiers: Modifiers {
                        control: pressed,
                        shift: pressed,
                        ..Default::default()
                    },
                    action: if repeat {
                        KeyAction::Repeat
                    } else if pressed {
                        KeyAction::Press
                    } else {
                        KeyAction::Release
                    },
                },
                input::Scope::Single,
            )
            .unwrap();
            match (physical, pressed, repeat) {
                (K::KeyE, true, false) => {
                    assert_eq!(effect, input::Effect::Search(input::SearchAction::Open));
                }
                (K::KeyC, true, false) => assert_eq!(effect, input::Effect::Copy),
                (K::KeyV, true, false) => assert_eq!(effect, input::Effect::ReadClipboard),
                _ => (),
            }
            assert!(
                pane.pty.input_fd().is_none(),
                "matched key queued PTY input: {physical:?}, pressed={pressed}, repeat={repeat}"
            );
        }
    }
    keyboard::window_shortcuts_never_reach_the_pty(&mut pane, &mut routing);
    search_modal_routes(&mut pane, &mut routing);
    close(pane);
}

fn search_event(
    pane: &mut LocalPane,
    routing: &mut input::Routing,
    searching: bool,
    event: Event,
) -> input::Effect {
    input::route(
        (0, routing.generation),
        pane,
        routing,
        searching,
        event,
        input::Scope::Single,
    )
    .unwrap()
}

fn search_modal_routes(pane: &mut LocalPane, routing: &mut input::Routing) {
    use KeyAction::{Press, Release, Repeat};
    use input::{Effect, SearchAction};

    assert_eq!(routing.change_recipient(), 1);
    for symbol in [L::Enter, L::NumpadEnter, L::F3] {
        for newer in [false, true] {
            let modifiers = Modifiers {
                shift: newer,
                ..Default::default()
            };
            let event = key_event(K::Enter, symbol, Press, modifiers);
            let effect = search_event(pane, routing, true, event);
            assert_eq!(effect, Effect::Search(SearchAction::Navigate { newer }));
        }
    }

    let plain = Modifiers::default();
    let ctrl_shift = Modifiers {
        control: true,
        shift: true,
        ..plain
    };
    let commit = |recipient| Event::ImeCommit {
        text: "水".into(),
        recipient,
    };
    let preedit = |generation| Event::ImePreedit {
        text: "ㄅ".into(),
        cursor: Some((0, 3)),
        generation,
    };
    let paste = |generation| Event::ClipboardText {
        text: "paste".into(),
        generation,
    };
    let escape = |action| key_event(K::Escape, L::Escape, action, plain);
    let typed = |action| key_event(K::KeyA, L::Character('a'), action, plain);
    let paste_key = |action, mods| key_event(K::KeyV, L::Character('М'), action, mods);
    let insert = |text: &str| Effect::Search(SearchAction::Insert(text.into()));
    let mut searching = true;
    for (event, expected) in [
        (paste(0), Effect::None),
        (preedit(0), Effect::None),
        (commit(1), insert("水")),
        (preedit(1), Effect::Preedit("ㄅ".into(), Some((0, 3)))),
        (paste_key(Press, ctrl_shift), Effect::ReadClipboard),
        (paste_key(Repeat, plain), Effect::None),
        (paste_key(Release, plain), Effect::None),
        (paste(1), insert("paste")),
        (
            key_event(K::KeyW, L::Character('É'), Press, ctrl_shift),
            Effect::None,
        ), // BÉPO
        (
            key_event(K::KeyW, L::Character('É'), Release, plain),
            Effect::None,
        ),
        (typed(Press), insert("a")),
        (typed(Repeat), insert("a")),
        (escape(Press), Effect::Search(SearchAction::Close)),
        (escape(Repeat), Effect::None),
        (escape(Release), Effect::None),
        (typed(Release), Effect::None),
        (commit(1), Effect::None),
        (paste(1), Effect::None),
        (
            key_event(K::KeyF, L::Character('F'), Press, ctrl_shift),
            Effect::Search(SearchAction::Open),
        ),
        (preedit(1), Effect::None),
        (commit(1), Effect::None),
        (paste(1), Effect::None),
        (commit(3), insert("水")),
    ] {
        let effect = search_event(pane, routing, searching, event);
        assert!(pane.pty.input_fd().is_none());
        assert_eq!(effect, expected);
        if let Effect::Search(SearchAction::Open | SearchAction::Close) = effect {
            searching = !searching;
            routing.change_recipient();
        }
    }
}

pub(crate) fn key_event(physical: K, logical: L, action: KeyAction, modifiers: Modifiers) -> Event {
    Event::Key {
        physical: PhysicalKey::Code(physical),
        logical,
        utf8: logical
            .character()
            .filter(|_| !matches!(action, KeyAction::Release))
            .map(String::from),
        modifiers,
        action,
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
        over_grid: true,
        modifiers: KeyModifiers::default(),
        link: false,
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
    close(pane);
}

#[test]
fn ctrl_click_a_link_bypasses_mouse_tracking() {
    use MousePhase::{Cancel, Motion, Press, Release};
    use input::Effect::{None as Ignore, OpenLink, Select};
    use sumie_core::mouse::{
        SelectStep::{Drag, DragFrom, Finish},
        SelectionUnit::Cell,
    };

    let mut pane = ready_pane(
        "printf '\\033[?1003;1006hREADY\\n'; exec cat",
        &request_environment(),
    );
    let mut routing = input::Routing::default();
    let mut send = |pane: &mut LocalPane, phase, column, x, link, expected| {
        let event = MouseEvent {
            phase,
            cell: CellPosition { column, row: 0 },
            pixel: [x, 0.0],
            over_grid: true,
            modifiers: KeyModifiers {
                control: matches!(phase, Press(..)),
                ..KeyModifiers::default()
            },
            link,
        };
        let effect = routing.pointer(pane, false, event, || None).unwrap();
        assert_eq!(effect, expected);
        assert!(pane.pty.input_fd().is_none());
    };
    let press = Press(Button::Left, 0);
    let release = Release(Button::Left);
    for (phase, column, x, link, expected) in [
        (press, 0, 0.0, true, Ignore),
        (Motion, 0, 0.0, true, Ignore),
        (release, 0, 0.0, true, OpenLink),
        (press, 0, 0.0, true, Ignore),
        (
            Motion,
            1,
            12.0,
            false,
            Select(DragFrom(CellPosition { row: 0, column: 0 })),
        ),
        (Motion, 0, 0.0, false, Select(Drag(Cell))),
        (
            release,
            0,
            0.0,
            true,
            Select(Finish {
                unit: Cell,
                copy: true,
            }),
        ),
        (press, 0, 0.0, true, Ignore),
        (Motion, 0, 1.0, false, Ignore),
        (release, 0, 0.0, true, Ignore),
        (press, 0, 0.0, true, Ignore),
        (release, 0, 0.0, false, Ignore),
        (press, 0, 0.0, true, Ignore),
        (Cancel, 0, 0.0, false, Ignore),
        (release, 0, 0.0, true, Ignore),
    ] {
        send(&mut pane, phase, column, x, link, expected);
    }
    send(&mut pane, press, 0, 0.0, true, Ignore);
    pane.leave_tab().unwrap();
    send(&mut pane, release, 0, 0.0, true, Ignore);
    close(pane);
}

#[test]
fn switching_tabs_finishes_a_reported_drag() {
    let mut pane = ready_pane(
        "stty -echo; printf '\\033[?1002;1006hREADY\\n'; IFS= read -r reports; \
         [ \"$reports\" = \"$(printf '\\033[<0;1;1M\\033[<0;1;1m')\" ] && echo RELEASED; exec cat",
        &request_environment(),
    );
    let mouse = |phase, column| {
        PaneCommand::Mouse(MouseEvent {
            phase,
            cell: CellPosition { column, row: 0 },
            pixel: [f32::from(column) * 12.0, 0.0],
            over_grid: true,
            modifiers: KeyModifiers::default(),
            link: false,
        })
    };
    pane.command(mouse(MousePhase::Press(Button::Left, 0), 0))
        .unwrap();
    pane.leave_tab().unwrap();
    assert!(pane.pty.flush_input().unwrap());
    pane.command(mouse(MousePhase::Motion, 1)).unwrap();
    assert!(pane.pty.input_fd().is_none());
    pane.command(PaneCommand::Text(b"\n")).unwrap();
    assert!(pane.pty.flush_input().unwrap());
    wait_for_line(&mut pane, "RELEASED");
    close(pane);
}

#[test]
fn settings_child_receives_config_argument() {
    let environment = crate::login::Environment::new(std::env::vars_os()).unwrap();
    let size = Size {
        cols: 40,
        rows: 10,
        width_px: 480,
        height_px: 240,
    };
    let mut pane = LocalPane::settings(
        OsStr::new("/bin/echo"),
        &environment,
        size,
        [12, 24],
        &sumie_config::Config::default(),
        std::path::Path::new("/"),
    )
    .unwrap();
    wait_for_line(&mut pane, "config");
    close(pane);
}

pub(crate) fn ready_pane(
    script: &str,
    env: &std::collections::BTreeMap<OsString, OsString>,
) -> LocalPane {
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

pub(crate) fn wait_for_line(pane: &mut LocalPane, expected: &str) {
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

pub(crate) fn close(mut pane: LocalPane) {
    pane.command(PaneCommand::Close).unwrap();
    let limit = Instant::now() + Duration::from_secs(6);
    while let Some(fd) = pane.pty.exit_fd() {
        let now = Instant::now();
        assert!(now < limit, "test child was not reaped");
        let until = pane.pty.close_deadline().unwrap_or(limit).min(limit);
        let timeout = Timespec::try_from(until.saturating_duration_since(now)).unwrap();
        poll(
            &mut [PollFd::from_borrowed_fd(fd, PollFlags::IN)],
            Some(&timeout),
        )
        .unwrap();
        pane.pty.service_child().unwrap();
    }
}
