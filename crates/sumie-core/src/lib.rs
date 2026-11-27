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
mod keys;
mod link;
pub mod mouse;
pub mod notify;
pub mod scroll;
mod selection;
mod shortcuts;
pub mod splits;
mod tabs;
pub use keys::{KeyCode, PhysicalKey};
pub use link::LinkUri;
pub use selection::{CellPosition, CellRange, Selection, SelectionPoint};
pub use shortcuts::{FontZoom, PaneAction, Shortcut, Shortcuts};
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
    /// Alt is the right-hand key. On macOS only left Option is Meta.
    pub right_alt: bool,
    /// The layout used Alt to type the key's text (right Option on macOS), so the encoder does
    /// not apply it again.
    pub alt_consumed: bool,
    pub super_key: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

/// The conventions the shortcut table and pointer gestures follow. The binary chooses one for
/// the platform it runs on; both stay testable on either host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// Ctrl+Shift is the primary modifier; Alt+digit selects tabs; a drag release copies.
    Linux,
    /// Command is the primary modifier and never reaches a PTY; Option is not a shortcut
    /// modifier; a drag release does not copy.
    MacOS,
}

impl Platform {
    /// Whether the modifiers turn a left press into a link gesture: Ctrl on Linux, Command on
    /// macOS.
    #[must_use]
    pub const fn link_modifier(self, modifiers: KeyModifiers) -> bool {
        match self {
            Self::Linux => modifiers.control,
            Self::MacOS => modifiers.super_key,
        }
    }

    /// Whether releasing a selection drag copies it: Linux mirrors the X11 habit, macOS copies
    /// only on Cmd+C.
    #[must_use]
    pub const fn copies_on_release(self) -> bool {
        matches!(self, Self::Linux)
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
    /// Scroll the history one page, up or down.
    ScrollPage {
        up: bool,
    },
    /// The window gained or lost the keyboard. Reaches the PTY only in mode 1004.
    Focus(bool),
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

    /// Whether the primary screen is active, where page keys scroll the history.
    ///
    /// # Errors
    /// Returns an error if the terminal cannot report its active screen.
    fn primary_screen(&self) -> Result<bool, Self::Error>;
}
