/// Main-thread frame pacing. Notify the window only after acquiring and drawing a frame.
pub(crate) trait FrameWindow {
    fn frame_slot_available(&self) -> bool;
    fn arm_frame_callback(&mut self);
}
