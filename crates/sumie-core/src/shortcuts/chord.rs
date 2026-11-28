use std::fmt;

use crate::{KeyCode as K, KeyModifiers, PhysicalKey, Platform};

const SHIFT: u8 = 1;
const ALT: u8 = 2;
const CTRL: u8 = 4;
const SUPER: u8 = 8;

// Kitty key numbers for non-printing keys. ASCII key numbers need no remapping.
const NAMED: &[(u32, &str, K)] = &[
    (9, "Tab", K::Tab),
    (13, "Enter", K::Enter),
    (32, "Space", K::Space),
    (127, "Backspace", K::Backspace),
    (57348, "Insert", K::Insert),
    (57349, "Delete", K::Delete),
    (57350, "Left", K::ArrowLeft),
    (57351, "Right", K::ArrowRight),
    (57352, "Up", K::ArrowUp),
    (57353, "Down", K::ArrowDown),
    (57354, "PageUp", K::PageUp),
    (57355, "PageDown", K::PageDown),
    (57356, "Home", K::Home),
    (57357, "End", K::End),
    (57364, "F1", K::F1),
    (57365, "F2", K::F2),
    (57366, "F3", K::F3),
    (57367, "F4", K::F4),
    (57368, "F5", K::F5),
    (57369, "F6", K::F6),
    (57370, "F7", K::F7),
    (57371, "F8", K::F8),
    (57372, "F9", K::F9),
    (57373, "F10", K::F10),
    (57374, "F11", K::F11),
    (57375, "F12", K::F12),
];

const ASCII: &[(u8, K)] = &[
    (b'a', K::KeyA),
    (b'b', K::KeyB),
    (b'c', K::KeyC),
    (b'd', K::KeyD),
    (b'e', K::KeyE),
    (b'f', K::KeyF),
    (b'g', K::KeyG),
    (b'h', K::KeyH),
    (b'i', K::KeyI),
    (b'j', K::KeyJ),
    (b'k', K::KeyK),
    (b'l', K::KeyL),
    (b'm', K::KeyM),
    (b'n', K::KeyN),
    (b'o', K::KeyO),
    (b'p', K::KeyP),
    (b'q', K::KeyQ),
    (b'r', K::KeyR),
    (b's', K::KeyS),
    (b't', K::KeyT),
    (b'u', K::KeyU),
    (b'v', K::KeyV),
    (b'w', K::KeyW),
    (b'x', K::KeyX),
    (b'y', K::KeyY),
    (b'z', K::KeyZ),
    (b'0', K::Digit0),
    (b'1', K::Digit1),
    (b'2', K::Digit2),
    (b'3', K::Digit3),
    (b'4', K::Digit4),
    (b'5', K::Digit5),
    (b'6', K::Digit6),
    (b'7', K::Digit7),
    (b'8', K::Digit8),
    (b'9', K::Digit9),
    (b'`', K::Backquote),
    (b'-', K::Minus),
    (b'=', K::Equal),
    (b'[', K::BracketLeft),
    (b']', K::BracketRight),
    (b'\\', K::Backslash),
    (b';', K::Semicolon),
    (b'\'', K::Quote),
    (b',', K::Comma),
    (b'.', K::Period),
    (b'/', K::Slash),
];
const SHIFTED: &[u8] = b"~!@#$%^&*()_+{}|:\"<>?";
const BASE: &[u8] = b"`1234567890-=[]\\;',./";

/// A single shortcut chord, independent of TOML and platform modifier names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    key: u32,
    mods: u8,
}

impl Chord {
    /// Decode a key and modifier mask in Kitty keyboard notation. Unmodified function keys
    /// are allowed; ordinary typing and Escape are not bindable.
    #[must_use]
    pub fn from_terminal(key: u32, mods: u8) -> Option<Self> {
        let mods = mods & 15;
        if let Ok(byte) = u8::try_from(key) {
            let mut byte = byte.to_ascii_lowercase();
            if mods & SHIFT != 0
                && let Some(i) = SHIFTED.iter().position(|&shifted| shifted == byte)
            {
                byte = BASE[i];
            }
            if byte.is_ascii_graphic() {
                return (mods & (CTRL | ALT | SUPER) != 0).then_some(Self {
                    key: u32::from(byte),
                    mods,
                });
            }
        }
        let named = NAMED.iter().any(|&(number, _, _)| number == key);
        (named && (mods != 0 || (57364..=57375).contains(&key))).then_some(Self { key, mods })
    }

    /// Parse a stored chord. `Super` is the portable spelling of the Command modifier.
    #[must_use]
    pub fn parse(mut text: &str) -> Option<Self> {
        let mut mods = 0;
        while let Some((name, rest)) = text.split_once('+') {
            let flag = match name {
                "Ctrl" => CTRL,
                "Alt" => ALT,
                "Shift" => SHIFT,
                "Super" => SUPER,
                _ => return None,
            };
            if mods & flag != 0 {
                return None;
            }
            mods |= flag;
            text = rest;
        }
        let key = match text.as_bytes() {
            &[byte] if byte.is_ascii_graphic() => u32::from(byte),
            b"Plus" => u32::from(b'+'),
            _ => NAMED.iter().find(|&&(_, name, _)| name == text)?.0,
        };
        Self::from_terminal(key, mods)
    }

    /// Format the chord for the selected platform rather than for storage.
    #[must_use]
    pub fn display(self, platform: Platform) -> String {
        let mut out = String::new();
        let _ = self.format(&mut out, Some(platform));
        out
    }

    fn format(self, out: &mut impl fmt::Write, platform: Option<Platform>) -> fmt::Result {
        let (alt, super_key, enter) = if platform == Some(Platform::MacOS) {
            ("Option", "Cmd", "Return")
        } else {
            ("Alt", "Super", "Enter")
        };
        for (bit, name) in [
            (CTRL, "Ctrl"),
            (ALT, alt),
            (SHIFT, "Shift"),
            (SUPER, super_key),
        ] {
            if self.mods & bit != 0 {
                write!(out, "{name}+")?;
            }
        }
        if let Some(&(_, name, _)) = NAMED.iter().find(|&&(key, _, _)| key == self.key) {
            out.write_str(if self.key == 13 { enter } else { name })
        } else if self.key == u32::from(b'+') {
            out.write_str(if platform.is_some() { "+" } else { "Plus" })
        } else {
            out.write_char(
                char::from_u32(self.key)
                    .expect("validated ASCII key")
                    .to_ascii_uppercase(),
            )
        }
    }

    pub(super) fn from_event(
        physical: PhysicalKey,
        symbol: Option<char>,
        mods: KeyModifiers,
    ) -> Option<Self> {
        let mask = u8::from(mods.shift)
            | (u8::from(mods.alt) << 1)
            | (u8::from(mods.control) << 2)
            | (u8::from(mods.super_key) << 3);
        let PhysicalKey::Code(code) = physical else {
            return Self::from_terminal(u32::from(symbol?), mask);
        };
        if let Some(&(key, _, _)) = NAMED.iter().find(|&&(_, _, pos)| pos == code) {
            return Self::from_terminal(key, mask);
        }
        let key = symbol
            .filter(char::is_ascii_graphic)
            .map(u32::from)
            .or_else(|| {
                ASCII
                    .iter()
                    .find(|&&(_, pos)| pos == code)
                    .map(|&(key, _)| u32::from(key))
            })?;
        Self::from_terminal(key, mask)
    }

    #[cfg(test)]
    pub(super) fn event(self) -> (PhysicalKey, Option<char>, KeyModifiers) {
        let mods = KeyModifiers {
            control: self.mods & CTRL != 0,
            alt: self.mods & ALT != 0,
            shift: self.mods & SHIFT != 0,
            super_key: self.mods & SUPER != 0,
            ..KeyModifiers::default()
        };
        if let Some(&(_, _, code)) = NAMED.iter().find(|&&(key, _, _)| key == self.key) {
            return (PhysicalKey::Code(code), None, mods);
        }
        let mut symbol = char::from_u32(self.key).expect("validated ASCII key");
        let position = ASCII
            .iter()
            .find(|&&(key, _)| char::from(key) == symbol)
            .map_or(PhysicalKey::Unidentified(0), |&(_, code)| {
                PhysicalKey::Code(code)
            });
        if mods.shift
            && let Some(i) = BASE.iter().position(|&byte| char::from(byte) == symbol)
        {
            symbol = char::from(SHIFTED[i]);
        }
        (position, Some(symbol), mods)
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(out, None)
    }
}
