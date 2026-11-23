#![cfg(target_os = "linux")]

//! Linux Wayland adapter. The caller owns the event loop, input policy, and renderer.

mod clipboard;
mod ime;
mod keyboard;
mod platform;
mod pointer;
mod presentation;
mod scale;

pub use ime::CursorRect;
pub use platform::{ConnectError, Platform, ShmPresentError, SourceHandle, WindowOwner};
pub use presentation::PresentationFeedback;
pub use smithay_client_toolkit::seat::keyboard::Modifiers;

/// Events in dispatch order. Text input is sensitive; do not log its contents.
pub enum Event {
    Configured {
        logical: (u32, u32),
        activated: bool,
        suspended: bool,
        maximized: bool,
        fullscreen: bool,
        server_decorated: bool,
    },
    Resized {
        logical: (u32, u32),
        device: (u32, u32),
        /// Scale in units of 1/120.
        scale: u32,
    },
    FrameReady,
    Presentation(PresentationFeedback),
    CloseRequested,
    ClipboardText(String),
    Key {
        /// Wayland/evdev code, without the XKB offset of 8.
        physical: u32,
        keysym: u32,
        utf8: Option<String>,
        modifiers: Modifiers,
        pressed: bool,
        repeat: bool,
        /// Wayland key serial, or zero for a client-generated repeat.
        serial: u32,
    },
    /// Pointer input from the active seat on the window surface.
    Pointer {
        phase: PointerPhase,
        /// Position in surface-local logical coordinates.
        position: (f64, f64),
        /// Wayland button serial on press and release; zero otherwise.
        serial: u32,
        modifiers: Modifiers,
    },
    /// Replace the composition overlay. An empty string clears it.
    ImePreedit {
        text: String,
        /// UTF-8 byte offsets; `None` hides the preedit cursor.
        cursor: Option<(u32, u32)>,
    },
    ImeCommit {
        text: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerPhase {
    Press(PointerButton),
    Release(PointerButton),
    Motion,
    Cancel,
    /// Signed vertical scroll, down positive, in units of 1/120 of a wheel notch.
    Wheel(f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}
