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

#[derive(Clone, Copy)]
pub(crate) struct PointerEvent {
    pub(crate) phase: PointerPhase,
    /// Surface-local logical coordinates.
    pub(crate) position: (f64, f64),
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
        position: (f64, f64),
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
