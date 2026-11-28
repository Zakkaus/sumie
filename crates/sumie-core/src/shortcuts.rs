//! The application shortcut table both platforms share. Matches a key before encoding and
//! reports the action; it neither runs actions nor touches a PTY. [`Platform`] picks the primary
//! modifier.

use unicode_script::{Script, UnicodeScript};

use crate::{
    KeyAction, KeyCode, KeyModifiers, PhysicalKey, Platform, TabAction,
    splits::{Axis, Direction},
};

mod bindings;
mod chord;

pub use bindings::{BINDINGS, Binding, binding_conflict};
pub use chord::Chord;

pub struct Shortcuts {
    platform: Platform,
    consumed: Vec<PhysicalKey>,
    bindings: Vec<(Chord, Shortcut)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Copy,
    Paste,
    Search,
    Settings,
    Tab(TabAction),
    Pane(PaneAction),
    Zoom(FontZoom),
    /// Scroll the history one page, up or down.
    ScrollPage {
        up: bool,
    },
    Consume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneAction {
    Split(Axis),
    Focus(Direction),
    Resize(Direction),
    Zoom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontZoom {
    Increase,
    Decrease,
    Reset,
}

impl Shortcuts {
    #[must_use]
    pub const fn new(platform: Platform) -> Self {
        Self {
            platform,
            consumed: Vec::new(),
            bindings: Vec::new(),
        }
    }

    /// Keep a modal key's repeats and release out of the PTY after the modal closes.
    pub fn capture(&mut self, physical: PhysicalKey) {
        if !self.consumed.contains(&physical) {
            self.consumed.push(physical);
        }
    }

    /// Whether a key held before Command repeats with Command down: on macOS that repeat
    /// reaches neither a PTY nor the search box, though the key's release still does.
    #[must_use]
    pub fn command_repeat(&self, modifiers: KeyModifiers, action: KeyAction) -> bool {
        self.platform == Platform::MacOS
            && modifiers.super_key
            && matches!(action, KeyAction::Repeat)
    }

    /// Match before encoding. A captured press owns its repeats and release even
    /// if the user releases a modifier first or the layout changes.
    /// Directional pane bindings are reserved only in tabs with splits, and Shift+PageUp and
    /// Shift+PageDown only on the primary screen; the alternate screen has no history.
    /// On macOS every Command chord is consumed, matched or not, and so are the Command keys
    /// and the repeats a key held before Command makes while Command is down.
    ///
    /// # Errors
    ///
    /// Returns the error of `primary_screen`, which runs only for those two keys.
    pub fn match_key<E>(
        &mut self,
        physical: PhysicalKey,
        symbol: Option<char>,
        modifiers: KeyModifiers,
        action: KeyAction,
        has_splits: bool,
        primary_screen: impl FnOnce() -> Result<bool, E>,
    ) -> Result<Option<Shortcut>, E> {
        if let Some(index) = self.consumed.iter().position(|&held| held == physical) {
            if !matches!(action, KeyAction::Repeat) {
                self.consumed.swap_remove(index);
            }
            if !matches!(action, KeyAction::Press) {
                return Ok(Some(Shortcut::Consume));
            }
        }
        if !matches!(action, KeyAction::Press) {
            return Ok(self
                .command_repeat(modifiers, action)
                .then_some(Shortcut::Consume));
        }
        let shortcut = self.configured_shortcut(physical, symbol, modifiers, has_splits);
        let shortcut = match shortcut {
            Some(Shortcut::ScrollPage { up }) if primary_screen()? => Shortcut::ScrollPage { up },
            Some(Shortcut::ScrollPage { .. }) | None => return Ok(None),
            Some(shortcut) => shortcut,
        };
        self.consumed.push(physical);
        Ok(Some(shortcut))
    }
}

fn default_shortcut(
    platform: Platform,
    physical: PhysicalKey,
    symbol: Option<char>,
    modifiers: KeyModifiers,
    has_splits: bool,
) -> Option<Shortcut> {
    match platform {
        Platform::Linux if modifiers.super_key => None,
        Platform::Linux => linux_shortcut(physical, symbol, modifiers, has_splits),
        Platform::MacOS if modifiers.super_key || command_key(physical) => Some(
            command_shortcut(physical, symbol, modifiers, has_splits).unwrap_or(Shortcut::Consume),
        ),
        Platform::MacOS => macos_shortcut(physical, modifiers, has_splits),
    }
}

fn linux_shortcut(
    physical: PhysicalKey,
    symbol: Option<char>,
    modifiers: KeyModifiers,
    has_splits: bool,
) -> Option<Shortcut> {
    match (modifiers.control, modifiers.alt, modifiers.shift) {
        (true, false, shift) => control_shortcut(physical, symbol, shift),
        (true, true, shift) if has_splits => directional_shortcut(physical, shift),
        (false, true, false) => alt_tab_shortcut(physical, symbol).map(Shortcut::Tab),
        (false, false, true) => page_shortcut(physical),
        _ => None,
    }
}

/// The chords macOS shares with Linux without Command: tab cycling and moving, pane focus
/// and resizing, and the history page keys. Option alone is never a shortcut, so right
/// Option keeps its characters.
fn macos_shortcut(
    physical: PhysicalKey,
    modifiers: KeyModifiers,
    has_splits: bool,
) -> Option<Shortcut> {
    match (modifiers.control, modifiers.alt, modifiers.shift) {
        (true, false, shift) => tab_cycle_shortcut(physical, shift).map(Shortcut::Tab),
        (true, true, shift) if has_splits => directional_shortcut(physical, shift),
        (false, false, true) => page_shortcut(physical),
        _ => None,
    }
}

const fn command_key(physical: PhysicalKey) -> bool {
    matches!(
        physical,
        PhysicalKey::Code(KeyCode::MetaLeft | KeyCode::MetaRight)
    )
}

/// The Linux Ctrl+Shift letters, zoom, tab digits and pane focus with Command alone, matched by
/// named position first and then by the layout's character.
fn command_shortcut(
    physical: PhysicalKey,
    symbol: Option<char>,
    modifiers: KeyModifiers,
    has_splits: bool,
) -> Option<Shortcut> {
    if modifiers.control || modifiers.alt {
        return None;
    }
    if !modifiers.shift
        && has_splits
        && let Some(focus) = directional_shortcut(physical, false)
    {
        return Some(focus);
    }
    let key = command_position(physical).or_else(|| symbol.map(|ch| ch.to_ascii_lowercase()))?;
    let shortcut = match (key, modifiers.shift) {
        ('=' | '+', _) => Shortcut::Zoom(FontZoom::Increase),
        ('-', false) => Shortcut::Zoom(FontZoom::Decrease),
        ('0', false) => Shortcut::Zoom(FontZoom::Reset),
        ('9', false) => Shortcut::Tab(TabAction::Last),
        (digit @ '1'..='8', false) => {
            Shortcut::Tab(TabAction::Select(digit.to_digit(10)? as usize - 1))
        }
        (',', false) => Shortcut::Settings,
        (letter, false) => letter_shortcut(letter)?,
        _ => return None,
    };
    Some(shortcut)
}

/// The character a named position stands for in the Command table.
const fn command_position(physical: PhysicalKey) -> Option<char> {
    use KeyCode as K;
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    Some(match code {
        K::Comma => ',',
        K::KeyC => 'c',
        K::KeyE => 'e',
        K::KeyF => 'f',
        K::KeyO => 'o',
        K::KeyT => 't',
        K::KeyV => 'v',
        K::KeyW => 'w',
        K::KeyZ => 'z',
        K::Equal | K::NumpadAdd => '=',
        K::Minus | K::NumpadSubtract => '-',
        K::Digit0 => '0',
        K::Digit1 => '1',
        K::Digit2 => '2',
        K::Digit3 => '3',
        K::Digit4 => '4',
        K::Digit5 => '5',
        K::Digit6 => '6',
        K::Digit7 => '7',
        K::Digit8 => '8',
        K::Digit9 => '9',
        _ => return None,
    })
}

/// The letter actions both tables share: Ctrl+Shift+<letter> on Linux, Command+<letter> on
/// macOS.
fn letter_shortcut(letter: char) -> Option<Shortcut> {
    Some(match letter {
        'c' => Shortcut::Copy,
        'e' => Shortcut::Pane(PaneAction::Split(Axis::Horizontal)),
        'f' => Shortcut::Search,
        'o' => Shortcut::Pane(PaneAction::Split(Axis::Vertical)),
        'v' => Shortcut::Paste,
        't' => Shortcut::Tab(TabAction::Open),
        'w' => Shortcut::Tab(TabAction::Close),
        'z' => Shortcut::Pane(PaneAction::Zoom),
        _ => return None,
    })
}

fn control_shortcut(physical: PhysicalKey, symbol: Option<char>, shift: bool) -> Option<Shortcut> {
    use KeyCode as K;
    use PhysicalKey::Code;

    match symbol {
        Some('=' | '+') => return Some(Shortcut::Zoom(FontZoom::Increase)),
        Some('-') => return Some(Shortcut::Zoom(FontZoom::Decrease)),
        Some('0') => return Some(Shortcut::Zoom(FontZoom::Reset)),
        _ => (),
    }
    if let Some(action) = tab_cycle_shortcut(physical, shift) {
        return Some(Shortcut::Tab(action));
    }
    if !shift && (physical == Code(K::Comma) || symbol == Some(',')) {
        return Some(Shortcut::Settings);
    }
    if !shift {
        return None;
    }
    // An unbound Latin key must not invoke a binding from its QWERTY position.
    let key = match symbol {
        Some(ch) if ch.is_ascii() || !ch.is_alphabetic() || ch.script() == Script::Latin => {
            Some(ch)
        }
        _ => match physical {
            Code(K::KeyC) => Some('c'),
            Code(K::KeyE) => Some('e'),
            Code(K::KeyF) => Some('f'),
            Code(K::KeyO) => Some('o'),
            Code(K::KeyV) => Some('v'),
            Code(K::KeyT) => Some('t'),
            Code(K::KeyW) => Some('w'),
            Code(K::KeyZ) => Some('z'),
            _ => None,
        },
    };
    letter_shortcut(key?.to_ascii_lowercase())
}

/// Ctrl+Tab and Ctrl+`PageDown` select the next tab, Ctrl+Shift+Tab and Ctrl+`PageUp` the
/// previous; Ctrl+Shift+`PageUp` and Ctrl+Shift+`PageDown` move the active tab.
fn tab_cycle_shortcut(physical: PhysicalKey, shift: bool) -> Option<TabAction> {
    use KeyCode as K;
    use PhysicalKey::Code;

    Some(match (physical, shift) {
        (Code(K::Tab | K::PageDown), false) => TabAction::Cycle(1),
        (Code(K::Tab), true) | (Code(K::PageUp), false) => TabAction::Cycle(-1),
        (Code(K::PageUp), true) => TabAction::MoveLeft,
        (Code(K::PageDown), true) => TabAction::MoveRight,
        _ => return None,
    })
}

fn page_shortcut(physical: PhysicalKey) -> Option<Shortcut> {
    match physical {
        PhysicalKey::Code(KeyCode::PageUp) => Some(Shortcut::ScrollPage { up: true }),
        PhysicalKey::Code(KeyCode::PageDown) => Some(Shortcut::ScrollPage { up: false }),
        _ => None,
    }
}

fn directional_shortcut(physical: PhysicalKey, resize: bool) -> Option<Shortcut> {
    let direction = match physical {
        PhysicalKey::Code(KeyCode::ArrowUp) => Direction::Up,
        PhysicalKey::Code(KeyCode::ArrowLeft) => Direction::Left,
        PhysicalKey::Code(KeyCode::ArrowRight) => Direction::Right,
        PhysicalKey::Code(KeyCode::ArrowDown) => Direction::Down,
        _ => return None,
    };
    Some(Shortcut::Pane(if resize {
        PaneAction::Resize(direction)
    } else {
        PaneAction::Focus(direction)
    }))
}

fn alt_tab_shortcut(physical: PhysicalKey, symbol: Option<char>) -> Option<TabAction> {
    let digit = match physical {
        PhysicalKey::Code(KeyCode::Digit1) => 1,
        PhysicalKey::Code(KeyCode::Digit2) => 2,
        PhysicalKey::Code(KeyCode::Digit3) => 3,
        PhysicalKey::Code(KeyCode::Digit4) => 4,
        PhysicalKey::Code(KeyCode::Digit5) => 5,
        PhysicalKey::Code(KeyCode::Digit6) => 6,
        PhysicalKey::Code(KeyCode::Digit7) => 7,
        PhysicalKey::Code(KeyCode::Digit8) => 8,
        PhysicalKey::Code(KeyCode::Digit9) => 9,
        _ => symbol?.to_digit(10)?,
    };
    match digit {
        1..=8 => Some(TabAction::Select((digit - 1) as usize)),
        9 => Some(TabAction::Last),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        KeyAction, KeyCode as K, KeyModifiers, PhysicalKey, Platform, Shortcut, Shortcuts,
    };

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
            let matched =
                shortcuts.match_key(physical, symbol, held, KeyAction::Press, true, primary);
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
}
