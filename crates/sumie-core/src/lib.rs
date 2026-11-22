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
    Text(&'a [u8]),
    Resize {
        cols: u16,
        rows: u16,
        /// Grid size and cell size, both in device pixels.
        px: [u16; 2],
        cell_px: [u32; 2],
    },
    Close,
}

/// Call from the owner thread. Neither operation waits for I/O or child exit.
pub trait Pane {
    type Snapshot;
    type Error;

    /// Submit one command. The implementation encodes key events before queuing input.
    ///
    /// # Errors
    /// Returns an error if encoding, queuing, resizing or initiating close fails.
    fn command(&mut self, command: PaneCommand<'_>) -> Result<(), Self::Error>;

    /// Return an owned snapshot for the consumer to draw without borrowing the pane.
    ///
    /// # Errors
    /// Returns an error if the pane cannot copy its current render state.
    fn snapshot(&mut self) -> Result<Self::Snapshot, Self::Error>;
}
