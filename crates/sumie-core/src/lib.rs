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

pub mod graphics;
mod link;
pub mod mouse;
mod selection;
pub mod splits;
mod tabs;
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
    consumed: Vec<u32>,
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
    pub fn capture(&mut self, evdev: u32) {
        if !self.consumed.contains(&evdev) {
            self.consumed.push(evdev);
        }
    }

    /// Match before encoding. A captured press owns its repeats and release even
    /// if the user releases a modifier first or the layout changes.
    pub fn match_key(
        &mut self,
        evdev: u32,
        symbol: Option<char>,
        modifiers: KeyModifiers,
        action: KeyAction,
    ) -> Option<Shortcut> {
        if let Some(index) = self.consumed.iter().position(|&held| held == evdev) {
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
            (true, false, shift) => control_shortcut(evdev, symbol, shift),
            (true, true, shift) => directional_shortcut(evdev, shift),
            (false, true, false) => alt_tab_shortcut(evdev, symbol).map(Shortcut::Tab),
            _ => None,
        }?;
        self.consumed.push(evdev);
        Some(shortcut)
    }
}

fn control_shortcut(evdev: u32, symbol: Option<char>, shift: bool) -> Option<Shortcut> {
    const KEY_TAB: u32 = 15;
    const KEY_W: u32 = 17;
    const KEY_E: u32 = 18;
    const KEY_T: u32 = 20;
    const KEY_O: u32 = 24;
    const KEY_F: u32 = 33;
    const KEY_Z: u32 = 44;
    const KEY_C: u32 = 46;
    const KEY_V: u32 = 47;
    const KEY_PAGE_UP: u32 = 104;
    const KEY_PAGE_DOWN: u32 = 109;

    match symbol {
        Some('=' | '+') => return Some(Shortcut::Zoom(FontZoom::Increase)),
        Some('-') => return Some(Shortcut::Zoom(FontZoom::Decrease)),
        Some('0') => return Some(Shortcut::Zoom(FontZoom::Reset)),
        _ => (),
    }

    let action = match (evdev, shift) {
        (KEY_TAB | KEY_PAGE_DOWN, false) => TabAction::Cycle(1),
        (KEY_TAB, true) | (KEY_PAGE_UP, false) => TabAction::Cycle(-1),
        (KEY_PAGE_UP, true) => TabAction::MoveLeft,
        (KEY_PAGE_DOWN, true) => TabAction::MoveRight,
        (_, false) => return None,
        (_, true) => {
            let key = match evdev {
                KEY_C => Some('c'),
                KEY_E => Some('e'),
                KEY_F => Some('f'),
                KEY_O => Some('o'),
                KEY_V => Some('v'),
                KEY_T => Some('t'),
                KEY_W => Some('w'),
                KEY_Z => Some('z'),
                _ => symbol,
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

fn directional_shortcut(evdev: u32, resize: bool) -> Option<Shortcut> {
    const KEY_UP: u32 = 103;
    const KEY_LEFT: u32 = 105;
    const KEY_RIGHT: u32 = 106;
    const KEY_DOWN: u32 = 108;
    let direction = match evdev {
        KEY_UP => Direction::Up,
        KEY_LEFT => Direction::Left,
        KEY_RIGHT => Direction::Right,
        KEY_DOWN => Direction::Down,
        _ => return None,
    };
    Some(Shortcut::Pane(if resize {
        PaneAction::Resize(direction)
    } else {
        PaneAction::Focus(direction)
    }))
}

fn alt_tab_shortcut(evdev: u32, symbol: Option<char>) -> Option<TabAction> {
    const KEY_1: u32 = 2;
    const KEY_9: u32 = 10;
    let digit = match evdev {
        KEY_1..=KEY_9 => evdev - KEY_1 + 1,
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
        /// Physical key position in the sending platform's key code namespace.
        physical: u32,
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
