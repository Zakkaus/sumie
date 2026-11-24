mod events;
mod watches;

pub(crate) use events::*;
pub(crate) use watches::{ExitWatches, PaneWatches, Ready, TimerSource};

pub(crate) enum CursorIcon {
    Default,
    Text,
    Pointer,
}

/// Clipped caret edges in the buffer used for this frame, not a pending configure.
pub(crate) struct CaretBounds {
    pub(crate) start: [i32; 2],
    pub(crate) end: [i64; 2],
    pub(crate) logical: [u32; 2],
    pub(crate) device: [u32; 2],
}

pub(crate) struct LogicalRect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

/// Main-thread window operations. User copy belongs to the event being delivered;
/// terminal clipboard requests use keyboard focus independently of input dispatch.
pub(crate) trait WindowOps: PaneWatches + crate::frame_window::FrameWindow {
    fn logical_rect(bounds: CaretBounds) -> Option<LogicalRect>;
    fn set_cursor_rectangle(&mut self, rect: LogicalRect);
    fn set_pointer_icon(&mut self, icon: CursorIcon);
    fn set_input_generation(&mut self, generation: u64);
    fn read_clipboard(&mut self, recipient: u64);
    fn write_clipboard(&mut self, text: String);
    fn write_terminal_clipboard(&mut self, bytes: Option<Vec<u8>>);
    fn queue_title(&mut self, title: String);
    fn set_background_effects(&mut self, transparent: bool, blur: bool) -> bool;
    fn background_commit_needed(&self) -> bool;
}
