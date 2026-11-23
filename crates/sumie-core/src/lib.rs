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

pub mod mouse;
mod selection;

pub use selection::{CellPosition, CellRange, Selection, SelectionPoint};

/// Key action before encoding against the active terminal modes.
#[derive(Clone, Copy)]
pub enum KeyAction {
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Copy, Default)]
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
    Consume,
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
        const KEY_C: u32 = 46;
        const KEY_F: u32 = 33;
        const KEY_V: u32 = 47;
        if let Some(index) = self.consumed.iter().position(|&held| held == evdev) {
            if !matches!(action, KeyAction::Repeat) {
                self.consumed.swap_remove(index);
            }
            if !matches!(action, KeyAction::Press) {
                return Some(Shortcut::Consume);
            }
        }
        if matches!(action, KeyAction::Press)
            && modifiers.control
            && modifiers.shift
            && !modifiers.alt
            && !modifiers.super_key
        {
            let key = match evdev {
                KEY_C => Some('c'),
                KEY_F => Some('f'),
                KEY_V => Some('v'),
                _ => symbol,
            };
            let shortcut = match key {
                Some('c' | 'C') => Shortcut::Copy,
                Some('f' | 'F') => Shortcut::Search,
                Some('v' | 'V') => Shortcut::Paste,
                _ => return None,
            };
            self.consumed.push(evdev);
            return Some(shortcut);
        }
        None
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
