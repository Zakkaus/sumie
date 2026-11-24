#![forbid(unsafe_code)]

//! Owns tabs, panes, selection, link and shortcut matching, commands, agent
//! state, and plain-data options. No platform crates, configuration, GPU,
//! FFI, unsafe code, or blocking I/O.
//!
//! Core does not expose wgpu to consumers:
//!
//! ```compile_fail
//! use sumie_core::wgpu;
//! ```

use unicode_script::{Script, UnicodeScript};

pub mod graphics;
mod keys;
mod link;
pub mod mouse;
mod selection;
pub mod splits;
mod tabs;
pub use keys::{KeyCode, PhysicalKey};
pub use link::LinkUri;
pub use selection::{CellPosition, CellRange, Selection, SelectionPoint};
use splits::{Axis, Direction};
pub use tabs::{TabAction, WindowTabs};

/// Key action before encoding against the active terminal modes.
#[derive(Clone, Copy)]
pub enum KeyAction {
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "modifiers are independent flags"
)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub super_key: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

#[derive(Default)]
pub struct Shortcuts {
    consumed: Vec<PhysicalKey>,
}

#[derive(Clone, Copy)]
pub enum Shortcut {
    Copy,
    Paste,
    Search,
    Tab(TabAction),
    Pane(PaneAction),
    Zoom(FontZoom),
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
    /// Keep a modal key's repeats and release out of the PTY after the modal closes.
    pub fn capture(&mut self, physical: PhysicalKey) {
        if !self.consumed.contains(&physical) {
            self.consumed.push(physical);
        }
    }

    /// Match before encoding. A captured press owns its repeats and release even
    /// if the user releases a modifier first or the layout changes.
    /// Directional pane bindings are reserved only in tabs with splits.
    pub fn match_key(
        &mut self,
        physical: PhysicalKey,
        symbol: Option<char>,
        modifiers: KeyModifiers,
        action: KeyAction,
        has_splits: bool,
    ) -> Option<Shortcut> {
        if let Some(index) = self.consumed.iter().position(|&held| held == physical) {
            if !matches!(action, KeyAction::Repeat) {
                self.consumed.swap_remove(index);
            }
            if !matches!(action, KeyAction::Press) {
                return Some(Shortcut::Consume);
            }
        }
        if !matches!(action, KeyAction::Press) || modifiers.super_key {
            return None;
        }
        let shortcut = match (modifiers.control, modifiers.alt, modifiers.shift) {
            (true, false, shift) => control_shortcut(physical, symbol, shift),
            (true, true, shift) if has_splits => directional_shortcut(physical, shift),
            (false, true, false) => alt_tab_shortcut(physical, symbol).map(Shortcut::Tab),
            _ => None,
        }?;
        self.consumed.push(physical);
        Some(shortcut)
    }
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

    let action = match (physical, shift) {
        (Code(K::Tab | K::PageDown), false) => TabAction::Cycle(1),
        (Code(K::Tab), true) | (Code(K::PageUp), false) => TabAction::Cycle(-1),
        (Code(K::PageUp), true) => TabAction::MoveLeft,
        (Code(K::PageDown), true) => TabAction::MoveRight,
        (_, false) => return None,
        (_, true) => {
            // An unbound Latin key must not invoke a binding from its QWERTY position.
            let key = match symbol {
                Some(ch)
                    if ch.is_ascii() || !ch.is_alphabetic() || ch.script() == Script::Latin =>
                {
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
            match key?.to_ascii_lowercase() {
                'c' => return Some(Shortcut::Copy),
                'e' => return Some(Shortcut::Pane(PaneAction::Split(Axis::Horizontal))),
                'f' => return Some(Shortcut::Search),
                'o' => return Some(Shortcut::Pane(PaneAction::Split(Axis::Vertical))),
                'v' => return Some(Shortcut::Paste),
                't' => TabAction::Open,
                'w' => TabAction::Close,
                'z' => return Some(Shortcut::Pane(PaneAction::Zoom)),
                _ => return None,
            }
        }
    };
    Some(Shortcut::Tab(action))
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

/// Commands accepted by the pane owner. The view never writes to a PTY directly.
pub enum PaneCommand<'a> {
    Key {
        physical: PhysicalKey,
        modifiers: KeyModifiers,
        action: KeyAction,
        /// Layout-dependent printable text, without control bytes.
        text: &'a str,
        unshifted: Option<char>,
    },
    Mouse(mouse::MouseEvent),
    Text(&'a [u8]),
    Paste(String),
    Copy(CellRange),
    Resize {
        cols: u16,
        rows: u16,
        /// Grid size and cell size, both in device pixels.
        px: [u16; 2],
        cell_px: [u32; 2],
    },
    Close,
}

#[derive(Debug)]
pub enum PaneEffect {
    None,
    Copied(String),
    Select(mouse::SelectStep),
    OpenLink,
    ViewportChanged,
    ReportedPress { viewport_changed: bool },
}

/// Call from the owner thread. Neither operation waits for I/O or child exit.
pub trait Pane {
    type Snapshot;
    type Error;

    /// Submit one command. Only `Copy` returns text; it never queues PTY input.
    ///
    /// # Errors
    /// Returns an error if encoding, queuing, resizing or initiating close fails.
    fn command(&mut self, command: PaneCommand<'_>) -> Result<PaneEffect, Self::Error>;

    /// Return an owned snapshot for the consumer to draw without borrowing the pane.
    ///
    /// # Errors
    /// Returns an error if the pane cannot copy its current render state.
    fn snapshot(&mut self) -> Result<Self::Snapshot, Self::Error>;
}
