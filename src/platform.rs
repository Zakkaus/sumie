mod events;
mod watches;

pub(crate) use events::*;
pub(crate) use watches::{ExitWatches, PaneWatches, Ready, TimerSource};

/// The shortcut and gesture conventions of the platform this binary runs on.
pub(crate) const POLICY: sumie_core::Platform = if cfg!(target_os = "macos") {
    sumie_core::Platform::MacOS
} else {
    sumie_core::Platform::Linux
};

pub(crate) enum CursorIcon {
    Default,
    Text,
    Pointer,
}

/// Clipped caret edges in the buffer used for this frame, not a pending configure.
#[derive(Clone, Copy)]
pub(crate) struct CaretBounds {
    pub(crate) start: [i32; 2],
    pub(crate) end: [i64; 2],
    pub(crate) logical: [u32; 2],
    pub(crate) device: [u32; 2],
}

#[cfg_attr(
    all(target_os = "macos", not(test)),
    expect(dead_code, reason = "macOS has no input methods to place yet")
)]
pub(crate) struct LogicalRect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

/// Main-thread window operations. User copy belongs to the event being delivered;
/// terminal clipboard requests use keyboard focus independently of input dispatch.
pub(crate) trait WindowOps: PaneWatches + crate::frame_window::FrameWindow {
    /// The desktop's double-click interval in milliseconds, if it publishes one.
    fn click_interval() -> Option<u32>;
    fn hostname() -> crate::Result<String>;
    /// Map caret edges to the coordinates the platform's input method expects.
    fn logical_rect(bounds: CaretBounds) -> Option<LogicalRect>;
    /// Release a frame slot stalled by a hidden or inactive window.
    fn recover_frame_slot(&mut self);
    fn set_cursor_rectangle(&mut self, rect: LogicalRect);
    fn set_pointer_icon(&mut self, icon: CursorIcon);
    fn set_input_generation(&mut self, generation: u64);
    fn read_clipboard(&mut self, recipient: u64);
    fn write_clipboard(&mut self, text: String);
    fn write_terminal_clipboard(&mut self, bytes: Option<Vec<u8>>);
    fn queue_title(&mut self, title: String);
    /// Queue a desktop notification without waiting. A platform without a service drops it.
    fn notify(&mut self, request: sumie_core::notify::Notification);
    fn set_background_effects(&mut self, transparent: bool, blur: bool) -> bool;
    fn background_commit_needed(&self) -> bool;
    /// Whether pending geometry may configure the surface at this frame slot.
    /// `queue_empty` polls the GPU. A platform that declines frees the slot again later.
    fn may_configure(&mut self, queue_empty: impl FnOnce() -> bool) -> bool {
        let _ = queue_empty;
        true
    }
    /// After an acquire timed out: whether the platform holds the frame slot until it
    /// may retry. Otherwise the session retries after a short delay.
    fn hold_after_timeout(&mut self) -> bool {
        false
    }
}
