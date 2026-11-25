use sumie_core::{KeyAction, PhysicalKey};

pub(crate) use sumie_core::{
    KeyModifiers as Modifiers,
    mouse::{Button as PointerButton, MousePhase as PointerPhase},
};

/// A layout symbol, independent of both physical position and composed input text.
#[derive(Clone, Copy)]
pub(crate) enum LogicalKey {
    Character(char),
    Escape,
    Enter,
    NumpadEnter,
    Backspace,
    Insert,
    Home,
    End,
    Left,
    Right,
    F3,
    /// No recognized logical key; the platform may still supply a character mapping.
    Other(Option<char>),
}

impl LogicalKey {
    pub(crate) fn character(self) -> Option<char> {
        match self {
            Self::Character(ch) => Some(ch),
            Self::Escape => Some('\x1b'),
            Self::Enter | Self::NumpadEnter => Some('\r'),
            Self::Backspace => Some('\x08'),
            Self::Other(ch) => ch,
            _ => None,
        }
    }
}

/// A pointer position in the coordinate space its platform reports. `view::geometry` maps
/// both spaces to the device pixels of the presented buffer.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Position {
    /// Device pixels of the buffer, already scaled by the platform (`AppKit` backing pixels).
    /// Never convert these through a rounded point extent.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only the macOS terminal driver reports backing pixels"
        )
    )]
    DevicePx(f64, f64),
    /// Surface-local logical coordinates; the compositor stretches the buffer over them.
    SurfaceLogical(f64, f64),
}

#[derive(Clone, Copy)]
pub(crate) struct PointerEvent {
    pub(crate) phase: PointerPhase,
    pub(crate) position: Position,
    pub(crate) modifiers: Modifiers,
}

/// Events are delivered in dispatch order. Do not log input text.
pub(crate) enum Event {
    Configured {
        activated: bool,
        suspended: bool,
    },
    Resized {
        logical: (u32, u32),
        device: (u32, u32),
        /// Scale in units of 1/120.
        scale: u32,
    },
    FrameReady,
    /// A submitted frame reached the screen at `at` on the compositor's clock.
    Presented {
        at: std::time::Duration,
        /// Predicted refresh interval; zero if unknown.
        refresh: std::time::Duration,
    },
    CloseRequested,
    ClipboardText {
        text: String,
        generation: u64,
    },
    Key {
        physical: PhysicalKey,
        logical: LogicalKey,
        utf8: Option<String>,
        modifiers: Modifiers,
        action: KeyAction,
    },
    ModifiersChanged(Modifiers),
    PointerEntered {
        position: Position,
        modifiers: Modifiers,
    },
    Pointer(PointerEvent),
    ImePreedit {
        text: String,
        generation: u64,
    },
    ImeCommit {
        text: String,
        generation: u64,
    },
}
