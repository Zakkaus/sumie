#![cfg(target_os = "linux")]

//! Linux Wayland adapter. The caller owns the event loop, input policy, and renderer.

mod ime;
mod keyboard;
mod platform;
mod scale;

pub use ime::CursorRect;
pub use platform::{ConnectError, Platform, ShmPresentError, SourceHandle, WindowOwner};
pub use smithay_client_toolkit::seat::keyboard::Modifiers;

/// Events in dispatch order. Text input is sensitive; do not log its contents.
pub enum Event {
    Configured {
        logical: (u32, u32),
        activated: bool,
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
    CloseRequested,
    Key {
        /// Wayland/evdev code, without the XKB offset of 8.
        physical: u32,
        keysym: u32,
        utf8: Option<String>,
        modifiers: Modifiers,
        pressed: bool,
        repeat: bool,
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
