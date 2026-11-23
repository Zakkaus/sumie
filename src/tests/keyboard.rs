use super::{
    KeyAction, LocalPane, Modifiers, Pane, PaneCommand, input, key_event, ready_pane,
    request_environment, wait_for_line,
};
use xkeysym::key;

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
    let enter = |action| key_event(28, key::Return, action, Modifiers::default());
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
    let ctrl = |action| key_event(29, key::Control_L, action, Modifiers::default());
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
    for (mode, physical, symbol) in [(b"\x1b[>0u", 29, key::Control_L), (b"\x1b[>3u", 30, key::a)] {
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
            ctrl: true,
            alt: true,
            shift,
            ..Modifiers::default()
        };
        for action in [KeyAction::Press, KeyAction::Repeat, KeyAction::Release] {
            let event = key_event(105, key::Left, action, modifiers);
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

fn close(mut pane: LocalPane) {
    pane.command(PaneCommand::Close).unwrap();
    crate::sources::reap(&mut pane.pty).unwrap();
}
