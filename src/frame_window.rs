/// Main-thread frame pacing. Notify the window only after acquiring and drawing a frame.
pub(crate) trait FrameWindow: Sized {
    type Context: ?Sized;
    fn open(context: &Self::Context) -> crate::Result<Self>;
    fn frame_slot_available(&self) -> bool;
    fn arm_frame_callback(&mut self);
}
