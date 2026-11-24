use sumie_core::TabAction;

use super::{
    K, KeyAction, L, LocalPane, Modifiers, Pane, PaneCommand, close, input, key_event, ready_pane,
    request_environment, wait_for_line,
};

#[test]
fn held_enter_never_reaches_the_next_pty() {
    let open = || {
        ready_pane(
            "printf '\\033[>11uREADY\\n'; exec cat",
            &request_environment(),
        )
    };
    let (mut first, mut next) = (open(), open());
    let mut routing = input::Routing::default();
    let enter = |action| key_event(K::Enter, L::Enter, action, Modifiers::default());
    input::route(
        (0, 0),
        &mut first,
        &mut routing,
        false,
        enter(KeyAction::Press),
        false,
    )
    .unwrap();
    assert!(first.pty.input_fd().is_some());
    let generation = routing.change_recipient();
    for action in [KeyAction::Repeat, KeyAction::Release] {
        input::route(
            (1, generation),
            &mut next,
            &mut routing,
            false,
            enter(action),
            false,
        )
        .unwrap();
        assert!(next.pty.input_fd().is_none());
    }
    close(first);
    close(next);
}

#[test]
fn held_keys_keep_their_release_but_not_their_repeat_across_focus() {
    let mut panes = [
        ready_pane(
            "stty -echo; printf '\\033[>11uREADY\\n'; IFS= read -r line; [ \"$line\" = \"$(printf '\\033[57442u\\033[57442;1:3u')\" ] && echo PAIRED; exec cat",
            &request_environment(),
        ),
        ready_pane(
            "printf '\\033[>11uREADY\\n'; exec cat",
            &request_environment(),
        ),
    ];
    let mut routing = input::Routing::default();
    let ctrl = |action| key_event(K::ControlLeft, L::Other(None), action, Modifiers::default());
    input::route(
        (0, 0),
        &mut panes[0],
        &mut routing,
        false,
        ctrl(KeyAction::Press),
        false,
    )
    .unwrap();
    for _ in 0..20 {
        panes[0].terminal.write(b"\r\n");
    }
    panes[0].terminal.scroll_rows(-3).unwrap();
    assert!(!panes[0].terminal.at_bottom().unwrap());
    let generation = routing.change_recipient();
    for action in [KeyAction::Repeat, KeyAction::Release] {
        let event = ctrl(action);
        let owner = routing.release_owner(&event).unwrap_or(1);
        let pane = if owner == 0 {
            &mut panes[0]
        } else {
            &mut panes[1]
        };
        input::route((owner, generation), pane, &mut routing, false, event, false).unwrap();
    }
    assert!(!panes[0].terminal.at_bottom().unwrap());
    assert!(panes[1].pty.input_fd().is_none());
    panes[0].command(PaneCommand::Text(b"\n")).unwrap();
    panes[0].pty.flush_input().unwrap();
    wait_for_line(&mut panes[0], "PAIRED");
    for pane in panes {
        close(pane);
    }
}

#[test]
fn rerouted_release_requires_a_reported_press() {
    let mut pane = ready_pane("printf 'READY\\n'; exec cat", &request_environment());
    for (mode, physical, symbol) in [
        (b"\x1b[>0u", K::ControlLeft, L::Other(None)),
        (b"\x1b[>3u", K::KeyA, L::Character('a')),
    ] {
        pane.terminal.write(mode);
        let event = |action| key_event(physical, symbol, action, Modifiers::default());
        let mut routing = input::Routing::default();
        input::route(
            (0, 0),
            &mut pane,
            &mut routing,
            false,
            event(KeyAction::Press),
            false,
        )
        .unwrap();
        pane.pty.flush_input().unwrap();
        let generation = routing.change_recipient();
        pane.terminal.write(b"\x1b[>11u");
        input::route(
            (0, generation),
            &mut pane,
            &mut routing,
            false,
            event(KeyAction::Release),
            false,
        )
        .unwrap();
        assert!(pane.pty.input_fd().is_none());
    }
    close(pane);
}

#[test]
fn unavailable_directional_chords_reach_the_pty() {
    let mut pane = ready_pane("printf 'READY\\n'; exec cat", &request_environment());
    let mut routing = input::Routing::default();
    for shift in [false, true] {
        let modifiers = Modifiers {
            control: true,
            alt: true,
            shift,
            ..Modifiers::default()
        };
        for action in [KeyAction::Press, KeyAction::Repeat, KeyAction::Release] {
            let event = key_event(K::ArrowLeft, L::Left, action, modifiers);
            let effect =
                input::route((0, 0), &mut pane, &mut routing, false, event, false).unwrap();
            assert_eq!(effect, input::Effect::None);
            if !matches!(action, KeyAction::Release) {
                assert!(pane.pty.input_fd().is_some());
            }
            pane.pty.flush_input().unwrap();
        }
    }
    close(pane);
}

pub(super) fn window_shortcuts_never_reach_the_pty(
    pane: &mut LocalPane,
    routing: &mut input::Routing,
) {
    use KeyAction::{Press, Release, Repeat};
    use input::Effect;
    use sumie_core::FontZoom;

    let plain = Modifiers::default();
    let ctrl = Modifiers {
        control: true,
        ..plain
    };
    let ctrl_shift = Modifiers {
        shift: true,
        ..ctrl
    };
    let alt = Modifiers { alt: true, ..plain };
    let chords = [
        (K::KeyT, L::Character('Т'), ctrl_shift, TabAction::Open),
        (K::KeyZ, L::Character('W'), ctrl_shift, TabAction::Close), // AZERTY
        (K::Tab, L::Other(Some('\t')), ctrl, TabAction::Cycle(1)),
        (K::Tab, L::Other(None), ctrl_shift, TabAction::Cycle(-1)),
        (K::PageDown, L::Other(None), ctrl, TabAction::Cycle(1)),
        (K::PageUp, L::Other(None), ctrl, TabAction::Cycle(-1)),
        (K::PageUp, L::Other(None), ctrl_shift, TabAction::MoveLeft),
        (
            K::PageDown,
            L::Other(None),
            ctrl_shift,
            TabAction::MoveRight,
        ),
    ];
    let zoom = [
        (
            K::Minus,
            L::Character('='),
            ctrl,
            Effect::Zoom(FontZoom::Increase),
        ),
        (
            K::Digit9,
            L::Character('+'),
            ctrl_shift,
            Effect::Zoom(FontZoom::Increase),
        ),
        (
            K::Digit6,
            L::Character('-'),
            ctrl,
            Effect::Zoom(FontZoom::Decrease),
        ),
        (
            K::Digit0,
            L::Character('0'),
            ctrl_shift,
            Effect::Zoom(FontZoom::Reset),
        ),
    ];
    let tabs =
        chords
            .into_iter()
            .chain(digit_chords(alt))
            .map(|(physical, symbol, modifiers, action)| {
                (physical, symbol, modifiers, Effect::Tab(action))
            });
    let panes = pane_chords(ctrl, ctrl_shift);
    for (physical, symbol, modifiers, effect) in tabs.chain(zoom).chain(panes) {
        for searching in [false, true] {
            for (action, modifiers, expected) in [
                (Press, modifiers, &effect),
                (Repeat, plain, &Effect::None),
                (Release, plain, &Effect::None),
            ] {
                let event = key_event(physical, symbol, action, modifiers);
                let searching = searching && !matches!(action, Release);
                assert_eq!(
                    &input::route((0, 0), pane, routing, searching, event, true).unwrap(),
                    expected
                );
                assert!(pane.pty.input_fd().is_none(), "matched key: {physical:?}");
            }
        }
    }
}

fn digit_chords(alt: Modifiers) -> impl Iterator<Item = (K, L, Modifiers, TabAction)> {
    let digits = [
        K::Digit1,
        K::Digit2,
        K::Digit3,
        K::Digit4,
        K::Digit5,
        K::Digit6,
        K::Digit7,
        K::Digit8,
        K::Digit9,
    ];
    (0_u8..).zip(digits).map(move |(index, physical)| {
        let action = if index == 8 {
            TabAction::Last
        } else {
            TabAction::Select(usize::from(index))
        };
        (
            physical,
            L::Character(char::from(b'1' + index)),
            alt,
            action,
        )
    })
}

fn pane_chords(
    ctrl: Modifiers,
    ctrl_shift: Modifiers,
) -> impl Iterator<Item = (K, L, Modifiers, input::Effect)> {
    use input::Effect::Pane;
    use sumie_core::{
        PaneAction::{Focus, Resize, Split, Zoom},
        splits::{
            Axis::{Horizontal, Vertical},
            Direction::{Down, Left, Right, Up},
        },
    };

    let focus = Modifiers { alt: true, ..ctrl };
    let resize = Modifiers {
        alt: true,
        ..ctrl_shift
    };
    [
        (
            K::KeyE,
            L::Character('E'),
            ctrl_shift,
            Pane(Split(Horizontal)),
        ),
        (
            K::KeyO,
            L::Character('O'),
            ctrl_shift,
            Pane(Split(Vertical)),
        ),
        (K::KeyW, L::Character('Z'), ctrl_shift, Pane(Zoom)), // AZERTY
        (K::ArrowLeft, L::Left, focus, Pane(Focus(Left))),
        (K::ArrowRight, L::Right, focus, Pane(Focus(Right))),
        (K::ArrowUp, L::Other(None), focus, Pane(Focus(Up))),
        (K::ArrowDown, L::Other(None), focus, Pane(Focus(Down))),
        (K::ArrowLeft, L::Left, resize, Pane(Resize(Left))),
        (K::ArrowRight, L::Right, resize, Pane(Resize(Right))),
        (K::ArrowUp, L::Other(None), resize, Pane(Resize(Up))),
        (K::ArrowDown, L::Other(None), resize, Pane(Resize(Down))),
    ]
    .into_iter()
}
