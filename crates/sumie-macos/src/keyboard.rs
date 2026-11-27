//! Converts winit key events. The physical position becomes the M1a [`PhysicalKey`]; the
//! layout's symbol and the typed text stay separate, so a press, its repeats and its release
//! share one identity even if the layout changes while the key is held.
//!
//! The window sets `OptionAsAlt::OnlyLeft`: left Option is Meta, so winit reports the text
//! without Option and Alt stays set for the encoder. Right Option is the layout's compose key:
//! its characters and dead keys stay, and Alt is marked as consumed by the text.

use sumie_core::{KeyAction, KeyCode as K, KeyModifiers, PhysicalKey};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{self, Key as Logical, KeyLocation, NamedKey, NativeKeyCode},
    platform::{
        modifier_supplement::KeyEventExtModifierSupplement as _,
        scancode::PhysicalKeyExtScancode as _,
    },
};

/// `AppKit` reports arrows and function keys as characters in this private-use range.
const FUNCTION_KEYS: std::ops::RangeInclusive<char> = '\u{f700}'..='\u{f8ff}';

/// The layout's symbol for a key, independent of its physical position and of the text it
/// typed. Matches the binary's logical keys one to one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symbol {
    Character(char),
    Escape,
    Enter,
    NumpadEnter,
    Backspace,
    Insert,
    Home,
    End,
    Left,
    Right,
    F3,
    /// No recognized symbol; the character, if any, is still passed to the terminal.
    Other(Option<char>),
}

/// One key press, repeat or release.
#[derive(Clone)]
pub struct Key {
    pub physical: PhysicalKey,
    pub symbol: Symbol,
    /// What the press typed with every modifier applied, like an XKB key's UTF-8; control
    /// characters included. Releases carry none.
    pub text: Option<String>,
    pub modifiers: KeyModifiers,
    pub action: KeyAction,
}

impl Key {
    /// Convert a key event under the modifiers winit reported last.
    pub(super) fn new(event: &KeyEvent, modifiers: KeyModifiers) -> Self {
        let action = match (event.state, event.repeat) {
            (ElementState::Released, _) => KeyAction::Release,
            (ElementState::Pressed, true) => KeyAction::Repeat,
            (ElementState::Pressed, false) => KeyAction::Press,
        };
        let text = event
            .text_with_all_modifiers()
            .filter(|_| event.state.is_pressed());
        let (text, modifiers) = typed(text, modifiers);
        Self {
            physical: physical_key(event.physical_key),
            symbol: symbol(&event.logical_key, event.location),
            text,
            modifiers,
            action,
        }
    }
}

/// The text a press typed, without `AppKit`'s function-key characters, and the modifiers
/// that apply to it.
fn typed(text: Option<&str>, mut modifiers: KeyModifiers) -> (Option<String>, KeyModifiers) {
    let text = text
        .map(|text| {
            text.chars()
                .filter(|ch| !FUNCTION_KEYS.contains(ch))
                .collect::<String>()
        })
        .filter(|text| !text.is_empty());
    // A character right Option typed is the input, not Alt plus a key: Alt stays reported but
    // consumed, so it neither prefixes ESC nor counts as Meta.
    modifiers.alt_consumed = modifiers.alt
        && modifiers.right_alt
        && text
            .as_deref()
            .is_some_and(|text| !text.chars().any(char::is_control));
    (text, modifiers)
}

fn symbol(key: &Logical, location: KeyLocation) -> Symbol {
    match key {
        Logical::Named(NamedKey::Escape) => Symbol::Escape,
        Logical::Named(NamedKey::Enter) if location == KeyLocation::Numpad => Symbol::NumpadEnter,
        Logical::Named(NamedKey::Enter) => Symbol::Enter,
        Logical::Named(NamedKey::Backspace) => Symbol::Backspace,
        Logical::Named(NamedKey::Insert) => Symbol::Insert,
        Logical::Named(NamedKey::Home) => Symbol::Home,
        Logical::Named(NamedKey::End) => Symbol::End,
        Logical::Named(NamedKey::ArrowLeft) => Symbol::Left,
        Logical::Named(NamedKey::ArrowRight) => Symbol::Right,
        Logical::Named(NamedKey::F3) => Symbol::F3,
        Logical::Named(NamedKey::Space) => Symbol::Character(' '),
        Logical::Named(NamedKey::Tab) => Symbol::Other(Some('\t')),
        Logical::Named(NamedKey::Delete) => Symbol::Other(Some('\x7f')),
        Logical::Character(text) => {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) if !FUNCTION_KEYS.contains(&ch) => Symbol::Character(ch),
                _ => Symbol::Other(None),
            }
        }
        _ => Symbol::Other(None),
    }
}

/// A position the M1a table names, else the `AppKit` virtual key code as an opaque identity.
fn physical_key(key: keyboard::PhysicalKey) -> PhysicalKey {
    match key {
        keyboard::PhysicalKey::Code(code) => key_code(code).map_or_else(
            // winit cannot name the code of every key it identifies (Fn); those share one.
            || PhysicalKey::Unidentified(key.to_scancode().unwrap_or(u32::MAX)),
            PhysicalKey::Code,
        ),
        keyboard::PhysicalKey::Unidentified(NativeKeyCode::MacOS(code)) => {
            PhysicalKey::Unidentified(code.into())
        }
        keyboard::PhysicalKey::Unidentified(_) => PhysicalKey::Unidentified(u32::MAX),
    }
}

fn key_code(code: keyboard::KeyCode) -> Option<K> {
    POSITIONS
        .iter()
        .find(|&&(winit, _)| winit == code)
        .map(|&(_, key)| key)
}

/// winit's positions that the M1a table names; the rest stay unidentified.
const POSITIONS: [(keyboard::KeyCode, K); 120] = {
    use keyboard::KeyCode as W;
    [
        (W::Backquote, K::Backquote),
        (W::Backslash, K::Backslash),
        (W::BracketLeft, K::BracketLeft),
        (W::BracketRight, K::BracketRight),
        (W::Comma, K::Comma),
        (W::Digit0, K::Digit0),
        (W::Digit1, K::Digit1),
        (W::Digit2, K::Digit2),
        (W::Digit3, K::Digit3),
        (W::Digit4, K::Digit4),
        (W::Digit5, K::Digit5),
        (W::Digit6, K::Digit6),
        (W::Digit7, K::Digit7),
        (W::Digit8, K::Digit8),
        (W::Digit9, K::Digit9),
        (W::Equal, K::Equal),
        (W::KeyA, K::KeyA),
        (W::KeyB, K::KeyB),
        (W::KeyC, K::KeyC),
        (W::KeyD, K::KeyD),
        (W::KeyE, K::KeyE),
        (W::KeyF, K::KeyF),
        (W::KeyG, K::KeyG),
        (W::KeyH, K::KeyH),
        (W::KeyI, K::KeyI),
        (W::KeyJ, K::KeyJ),
        (W::KeyK, K::KeyK),
        (W::KeyL, K::KeyL),
        (W::KeyM, K::KeyM),
        (W::KeyN, K::KeyN),
        (W::KeyO, K::KeyO),
        (W::KeyP, K::KeyP),
        (W::KeyQ, K::KeyQ),
        (W::KeyR, K::KeyR),
        (W::KeyS, K::KeyS),
        (W::KeyT, K::KeyT),
        (W::KeyU, K::KeyU),
        (W::KeyV, K::KeyV),
        (W::KeyW, K::KeyW),
        (W::KeyX, K::KeyX),
        (W::KeyY, K::KeyY),
        (W::KeyZ, K::KeyZ),
        (W::Minus, K::Minus),
        (W::Period, K::Period),
        (W::Quote, K::Quote),
        (W::Semicolon, K::Semicolon),
        (W::Slash, K::Slash),
        (W::AltLeft, K::AltLeft),
        (W::AltRight, K::AltRight),
        (W::Backspace, K::Backspace),
        (W::CapsLock, K::CapsLock),
        (W::ContextMenu, K::ContextMenu),
        (W::ControlLeft, K::ControlLeft),
        (W::ControlRight, K::ControlRight),
        (W::Enter, K::Enter),
        (W::SuperLeft, K::MetaLeft),
        (W::SuperRight, K::MetaRight),
        (W::ShiftLeft, K::ShiftLeft),
        (W::ShiftRight, K::ShiftRight),
        (W::Space, K::Space),
        (W::Tab, K::Tab),
        (W::Delete, K::Delete),
        (W::End, K::End),
        (W::Home, K::Home),
        (W::Insert, K::Insert),
        (W::PageDown, K::PageDown),
        (W::PageUp, K::PageUp),
        (W::ArrowDown, K::ArrowDown),
        (W::ArrowLeft, K::ArrowLeft),
        (W::ArrowRight, K::ArrowRight),
        (W::ArrowUp, K::ArrowUp),
        (W::NumLock, K::NumLock),
        (W::Numpad0, K::Numpad0),
        (W::Numpad1, K::Numpad1),
        (W::Numpad2, K::Numpad2),
        (W::Numpad3, K::Numpad3),
        (W::Numpad4, K::Numpad4),
        (W::Numpad5, K::Numpad5),
        (W::Numpad6, K::Numpad6),
        (W::Numpad7, K::Numpad7),
        (W::Numpad8, K::Numpad8),
        (W::Numpad9, K::Numpad9),
        (W::NumpadAdd, K::NumpadAdd),
        (W::NumpadDecimal, K::NumpadDecimal),
        (W::NumpadDivide, K::NumpadDivide),
        (W::NumpadEnter, K::NumpadEnter),
        (W::NumpadEqual, K::NumpadEqual),
        (W::NumpadMultiply, K::NumpadMultiply),
        (W::NumpadSubtract, K::NumpadSubtract),
        (W::Escape, K::Escape),
        (W::F1, K::F1),
        (W::F2, K::F2),
        (W::F3, K::F3),
        (W::F4, K::F4),
        (W::F5, K::F5),
        (W::F6, K::F6),
        (W::F7, K::F7),
        (W::F8, K::F8),
        (W::F9, K::F9),
        (W::F10, K::F10),
        (W::F11, K::F11),
        (W::F12, K::F12),
        (W::F13, K::F13),
        (W::F14, K::F14),
        (W::F15, K::F15),
        (W::F16, K::F16),
        (W::F17, K::F17),
        (W::F18, K::F18),
        (W::F19, K::F19),
        (W::F20, K::F20),
        (W::F21, K::F21),
        (W::F22, K::F22),
        (W::F23, K::F23),
        (W::F24, K::F24),
        (W::PrintScreen, K::PrintScreen),
        (W::ScrollLock, K::ScrollLock),
        (W::Pause, K::Pause),
        (W::Copy, K::Copy),
        (W::Cut, K::Cut),
        (W::Paste, K::Paste),
    ]
};

#[cfg(test)]
mod tests {
    use winit::keyboard::KeyCode as W;

    use super::*;

    #[test]
    fn named_positions_keep_their_m1a_codes_and_others_their_virtual_key_code() {
        for (winit, expected) in [
            (W::KeyA, PhysicalKey::Code(K::KeyA)),
            (W::SuperLeft, PhysicalKey::Code(K::MetaLeft)),
            (W::NumpadEnter, PhysicalKey::Code(K::NumpadEnter)),
            (W::IntlYen, PhysicalKey::Unidentified(0x5d)),
        ] {
            assert_eq!(physical_key(keyboard::PhysicalKey::Code(winit)), expected);
        }
        // winit leaves the ISO section key (kVK_ISO_Section) unidentified.
        for code in [0x0a, 0x6e] {
            let native = keyboard::PhysicalKey::Unidentified(NativeKeyCode::MacOS(code));
            assert_eq!(physical_key(native), PhysicalKey::Unidentified(code.into()));
        }
    }

    #[test]
    fn right_option_text_consumes_alt_but_left_option_and_function_keys_keep_it() {
        let left = KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        };
        let right = KeyModifiers {
            right_alt: true,
            ..left
        };
        let consumed = KeyModifiers {
            alt_consumed: true,
            ..right
        };
        let control = KeyModifiers {
            control: true,
            ..KeyModifiers::default()
        };
        for (text, modifiers, expected) in [
            // winit already typed left Option's key without it: Meta+a.
            (Some("a"), left, (Some("a"), left)),
            (Some("å"), right, (Some("å"), consumed)),
            // Option+Left: AppKit's arrow character is not text.
            (Some("\u{f702}"), right, (None, right)),
            (Some("\x7f"), right, (Some("\x7f"), right)),
            (Some("\x03"), control, (Some("\x03"), control)),
            // A dead key types nothing yet.
            (None, right, (None, right)),
        ] {
            let (text, modifiers) = typed(text, modifiers);
            assert_eq!(text.as_deref(), expected.0);
            assert!(modifiers == expected.1, "{text:?}");
        }
    }

    #[test]
    fn symbols_follow_the_layout_and_numpad_enter_stays_distinct() {
        let character = |text: &str| Logical::Character(text.into());
        for (key, location, expected) in [
            (
                character("q"),
                KeyLocation::Standard,
                Symbol::Character('q'),
            ),
            (
                character("ж"),
                KeyLocation::Standard,
                Symbol::Character('ж'),
            ),
            (
                character("\u{f702}"),
                KeyLocation::Standard,
                Symbol::Other(None),
            ),
            (
                Logical::Named(NamedKey::Enter),
                KeyLocation::Numpad,
                Symbol::NumpadEnter,
            ),
            (
                Logical::Named(NamedKey::Enter),
                KeyLocation::Standard,
                Symbol::Enter,
            ),
            (
                Logical::Named(NamedKey::Tab),
                KeyLocation::Standard,
                Symbol::Other(Some('\t')),
            ),
            (
                Logical::Named(NamedKey::Space),
                KeyLocation::Standard,
                Symbol::Character(' '),
            ),
            (
                Logical::Dead(Some('´')),
                KeyLocation::Standard,
                Symbol::Other(None),
            ),
        ] {
            assert_eq!(symbol(&key, location), expected);
        }
    }
}
