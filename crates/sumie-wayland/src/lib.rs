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
pub use pointer::double_click_interval;
pub use presentation::PresentationFeedback;
pub use smithay_client_toolkit::seat::keyboard::Modifiers;
pub use smithay_client_toolkit::seat::pointer::CursorIcon;

/// Events in dispatch order. Text input is sensitive; do not log its contents.
/// Clipboard and IME text carry the generation of the recipient that requested them.
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
    ClipboardText {
        text: String,
        generation: u64,
    },
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
    ModifiersChanged(Modifiers),
    KeyboardFocus(bool),
    PointerEntered {
        position: (f64, f64),
        modifiers: Modifiers,
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
        generation: u64,
    },
    ImeCommit {
        text: String,
        generation: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerPhase {
    /// Button and Wayland event time in milliseconds.
    Press(PointerButton, u32),
    Release(PointerButton),
    Motion,
    Cancel,
    /// Signed vertical scroll, down positive, in units of 1/120 of a wheel notch.
    Wheel(f64),
    /// Finger movement on a touchpad, in the units of `Wheel`.
    Touch(f64),
    /// The fingers left the touchpad (`axis_stop`).
    TouchEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}
