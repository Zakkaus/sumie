use super::{
    KeyAction, KeyCode as K, KeyModifiers, PhysicalKey, Platform, Shortcut, Shortcuts, letter_chord,
};
use crate::TabAction;

#[test]
fn letter_chords_come_from_the_table() {
    let open = Shortcut::Tab(TabAction::Open);
    assert_eq!(
        letter_chord(Platform::Linux, open),
        Some(("Ctrl+Shift+", 'T'))
    );
    assert_eq!(letter_chord(Platform::MacOS, open), Some(("Cmd+", 'T')));
    assert_eq!(letter_chord(Platform::Linux, Shortcut::Consume), None);
}

#[test]
fn super_never_matches_a_shortcut() {
    let ctrl = KeyModifiers {
        control: true,
        ..KeyModifiers::default()
    };
    let ctrl_shift = KeyModifiers {
        shift: true,
        ..ctrl
    };
    let ctrl_alt = KeyModifiers { alt: true, ..ctrl };
    let alt = KeyModifiers {
        alt: true,
        ..KeyModifiers::default()
    };
    let shift = KeyModifiers {
        shift: true,
        ..KeyModifiers::default()
    };
    // Each chord matches without Super: by layout character, by the non-Latin
    // fallback, by alias, by tab digit, by split direction and by page key.
    for (code, symbol, modifiers) in [
        (K::KeyC, Some('C'), ctrl_shift),
        (K::KeyZ, Some('W'), ctrl_shift),
        (K::KeyC, Some('С'), ctrl_shift),
        (K::Tab, None, ctrl),
        (K::Equal, Some('='), ctrl),
        (K::Digit1, Some('1'), alt),
        (K::ArrowLeft, None, ctrl_alt),
        (K::PageUp, None, shift),
    ] {
        let physical = PhysicalKey::Code(code);
        let held = KeyModifiers {
            super_key: true,
            ..modifiers
        };
        let mut shortcuts = Shortcuts::new(Platform::Linux);
        let primary = || Ok::<_, ()>(true);
        let matched = shortcuts.match_key(physical, symbol, held, KeyAction::Press, true, primary);
        assert!(matches!(matched, Ok(None)), "Super matched {code:?}");
        let matched =
            shortcuts.match_key(physical, symbol, modifiers, KeyAction::Press, true, primary);
        assert!(matches!(matched, Ok(Some(_))), "{code:?} did not match");
    }
}

fn press(
    shortcuts: &mut Shortcuts,
    code: K,
    symbol: Option<char>,
    modifiers: KeyModifiers,
    has_splits: bool,
) -> Option<Shortcut> {
    let primary = || Ok::<_, ()>(true);
    let physical = PhysicalKey::Code(code);
    shortcuts
        .match_key(
            physical,
            symbol,
            modifiers,
            KeyAction::Press,
            has_splits,
            primary,
        )
        .unwrap()
}

fn label(shortcut: Option<Shortcut>) -> String {
    match shortcut {
        None => "none".into(),
        Some(Shortcut::Copy) => "copy".into(),
        Some(Shortcut::Paste) => "paste".into(),
        Some(Shortcut::Search) => "search".into(),
        Some(Shortcut::Settings) => "settings".into(),
        Some(Shortcut::Tab(action)) => format!("tab {action:?}"),
        Some(Shortcut::Pane(action)) => format!("pane {action:?}"),
        Some(Shortcut::Zoom(zoom)) => format!("zoom {zoom:?}"),
        Some(Shortcut::ScrollPage { up }) => format!("page {up}"),
        Some(Shortcut::Consume) => "consume".into(),
    }
}

#[test]
fn macos_binds_the_linux_actions_to_command_by_position_then_character() {
    let none = KeyModifiers::default();
    let cmd = KeyModifiers {
        super_key: true,
        ..none
    };
    let cmd_shift = KeyModifiers { shift: true, ..cmd };
    let ctrl = KeyModifiers {
        control: true,
        ..none
    };
    let ctrl_shift = KeyModifiers {
        shift: true,
        ..ctrl
    };
    let ctrl_alt = KeyModifiers { alt: true, ..ctrl };
    let left_alt = KeyModifiers { alt: true, ..none };
    let right_alt = KeyModifiers {
        right_alt: true,
        alt_consumed: true,
        ..left_alt
    };
    for (code, symbol, modifiers, splits, expected) in [
        (K::KeyC, Some('c'), cmd, false, "copy"),
        (K::KeyV, Some('v'), cmd, false, "paste"),
        (K::KeyT, Some('t'), cmd, false, "tab Open"),
        (K::KeyW, Some('w'), cmd, false, "tab Close"),
        (K::KeyE, Some('e'), cmd, false, "pane Split(Horizontal)"),
        (K::KeyO, Some('o'), cmd, false, "pane Split(Vertical)"),
        (K::KeyF, Some('f'), cmd, false, "search"),
        (K::Enter, None, cmd_shift, true, "pane Zoom"),
        // A named position wins over the layout: Dvorak types j on the C key.
        (K::KeyC, Some('j'), cmd, false, "copy"),
        // Non-Latin layouts keep the position.
        (K::KeyV, Some('м'), cmd, false, "paste"),
        // An unnamed position falls back to the layout's character.
        (K::Backquote, Some('c'), cmd, false, "copy"),
        (K::Equal, Some('='), cmd, false, "zoom Increase"),
        (K::Equal, Some('+'), cmd_shift, false, "zoom Increase"),
        (K::Minus, Some('-'), cmd, false, "zoom Decrease"),
        (K::Digit0, Some('0'), cmd, false, "zoom Reset"),
        (K::Digit1, Some('1'), cmd, false, "tab Select(0)"),
        (K::Digit9, Some('9'), cmd, false, "tab Last"),
        (K::ArrowLeft, None, cmd, true, "pane Focus(Left)"),
        (K::ArrowLeft, None, ctrl_alt, true, "pane Focus(Left)"),
        (K::Tab, None, ctrl, false, "tab Cycle(1)"),
        // An action that cannot run, or no action, still keeps Command out of the PTY.
        (K::ArrowLeft, None, cmd, false, "consume"),
        (K::KeyK, Some('k'), cmd, false, "consume"),
        // Cmd+Z is Undo on macOS, not pane zoom, at its position or by its character.
        (K::KeyZ, Some('z'), cmd, true, "consume"),
        (K::Backquote, Some('z'), cmd, true, "consume"),
        (K::KeyZ, Some('w'), cmd, false, "consume"),
        (K::Enter, None, cmd, true, "consume"),
        (K::KeyC, Some('C'), cmd_shift, false, "consume"),
        (K::MetaLeft, None, none, false, "consume"),
        // Without Command the Linux-only chords reach the program.
        (K::ArrowLeft, None, ctrl_alt, false, "none"),
        (K::KeyC, Some('C'), ctrl_shift, false, "none"),
        (K::Equal, Some('='), ctrl, false, "none"),
        (K::Digit1, Some('1'), left_alt, false, "none"),
        (K::Digit1, Some('¡'), right_alt, false, "none"),
    ] {
        let mut shortcuts = Shortcuts::new(Platform::MacOS);
        let got = label(press(&mut shortcuts, code, symbol, modifiers, splits));
        assert_eq!(got, expected, "{code:?} {symbol:?}");
    }
}

#[test]
fn a_matched_command_chord_consumes_its_press_repeat_and_release() {
    let command = KeyModifiers {
        super_key: true,
        ..KeyModifiers::default()
    };
    let primary = || Ok::<_, ()>(true);
    let key = PhysicalKey::Code(K::KeyC);
    let mut shortcuts = Shortcuts::new(Platform::MacOS);
    for (action, modifiers, expected) in [
        (KeyAction::Press, command, "copy"),
        (KeyAction::Repeat, command, "consume"),
        // Command lifted first: the release still belongs to the shortcut.
        (KeyAction::Release, KeyModifiers::default(), "consume"),
        (KeyAction::Press, KeyModifiers::default(), "none"),
    ] {
        let matched = shortcuts.match_key(key, Some('c'), modifiers, action, false, primary);
        assert_eq!(label(matched.unwrap()), expected);
    }
}

#[test]
fn a_key_held_into_command_repeats_only_without_it() {
    let command = KeyModifiers {
        super_key: true,
        ..KeyModifiers::default()
    };
    let primary = || Ok::<_, ()>(true);
    let key = PhysicalKey::Code(K::KeyA);
    for (step, (platform, action, modifiers, expected)) in [
        (
            Platform::MacOS,
            KeyAction::Press,
            KeyModifiers::default(),
            "none",
        ),
        (Platform::MacOS, KeyAction::Repeat, command, "consume"),
        (
            Platform::MacOS,
            KeyAction::Repeat,
            KeyModifiers::default(),
            "none",
        ),
        // The release still pairs with the press the program saw.
        (Platform::MacOS, KeyAction::Release, command, "none"),
        // Linux keeps Super out of the table, not out of the PTY.
        (Platform::Linux, KeyAction::Repeat, command, "none"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut shortcuts = Shortcuts::new(platform);
        let matched = shortcuts.match_key(key, Some('a'), modifiers, action, false, primary);
        assert_eq!(label(matched.unwrap()), expected, "step {step}");
    }
}
