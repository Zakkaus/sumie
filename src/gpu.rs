use std::time::Duration;

use sumie_render::{
    AtlasUpload, FrameDraw, PaneDraw, SurfaceError, SurfaceFrame, SurfaceRenderer, backends,
    instance_descriptor, wgpu,
};
use sumie_wayland::{CursorIcon, CursorRect, Event, Platform, SourceHandle};

use crate::{Result, capture::Readback, frame_window::FrameWindow};

pub(crate) fn background_alpha(opacity: f32) -> u8 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "validated opacity in 0..=1, rounded to an RGBA8 channel"
    )]
    {
        (opacity * 255.0).round() as u8
    }
}

impl FrameWindow for Platform {
    type Context = str;

    fn open(title: &str) -> Result<Self> {
        Ok(Self::connect(title)?)
    }

    fn frame_slot_available(&self) -> bool {
        self.can_present()
    }
    fn arm_frame_callback(&mut self) {
        assert!(self.begin_frame());
    }
}

pub(crate) struct Gpu {
    target: SurfaceRenderer<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    _instance: wgpu::Instance,
    window: Platform,
}

impl Gpu {
    pub(crate) fn new(capture: bool, minimum_contrast: f32) -> Result<Self> {
        let window = Platform::open("sumie")?;
        let instance = wgpu::Instance::new(instance_descriptor(
            wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
                window.display_handle_source(),
            )),
            backends(),
        ));
        let surface = instance.create_surface(window.window_owner())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                required_limits:
                    wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            }))?;
        let mut target = SurfaceRenderer::new(surface, &adapter, &device, &queue, [0, 0])?;
        target.set_minimum_contrast(minimum_contrast);
        if capture {
            target.enable_readback(&adapter)?;
        }
        Ok(Self {
            target,
            device,
            queue,
            _instance: instance,
            window,
        })
    }

    pub(crate) fn image_dimension_limit(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    pub(crate) fn presented_thumbnails(
        &self,
    ) -> impl Iterator<Item = (usize, usize, sumie_render::PixelRect)> + '_ {
        self.target.presented_thumbnail_layers()
    }

    pub(crate) fn viewer_presented(&self, pane: usize) -> bool {
        self.target.viewer_presented(pane)
    }

    pub(crate) fn loop_handle(&self) -> SourceHandle {
        self.window.loop_handle()
    }

    pub(crate) fn set_input_generation(&mut self, generation: u64) {
        self.window.set_input_generation(generation);
    }

    pub(crate) fn set_pointer_icon(&mut self, icon: CursorIcon) {
        self.window.set_pointer_icon(icon);
    }

    pub(crate) fn read_clipboard(&mut self, recipient: u64) {
        if let Err(error) = self
            .window
            .read_clipboard(sumie_pty::INPUT_LIMIT, recipient)
        {
            tracing::warn!(%error, "could not request clipboard text");
        }
    }

    pub(crate) fn write_clipboard(&mut self, serial: u32, text: String) {
        self.window.write_clipboard(serial, text);
    }

    pub(crate) fn write_terminal_clipboard(&mut self, bytes: Option<Vec<u8>>) {
        self.window.write_terminal_clipboard(bytes);
    }

    pub(crate) fn queue_title(&mut self, title: String) {
        self.window.queue_title(title);
    }

    pub(crate) fn dispatch(
        &mut self,
        received: &mut Vec<Event>,
        timeout: Option<Duration>,
    ) -> Result<()> {
        self.window.dispatch(timeout)?;
        received.extend(self.window.drain_events());
        Ok(())
    }

    pub(crate) fn frame_slot_available(&self) -> bool {
        self.window.frame_slot_available()
    }

    pub(crate) fn recover_frame_slot(&mut self) {
        self.window.recover_frame_slot();
    }

    pub(crate) fn draw<'a>(
        &mut self,
        list: &FrameDraw<'a, impl Clone + Iterator<Item = PaneDraw<'a>>>,
        capture: bool,
    ) -> Result<(SurfaceFrame, Option<Result<Readback>>), SurfaceError> {
        let mut readback = None;
        let premultiplied = capture && self.target.is_premultiplied();
        let result = self.target.draw_frame_with(list, |frame| {
            if capture {
                readback = Some(Readback::copy(
                    &self.device,
                    &self.queue,
                    frame,
                    premultiplied,
                ));
            }
            self.window.arm_frame_callback();
        })?;
        Ok((result, readback))
    }

    pub(crate) fn upload_atlas(&mut self, atlas: &AtlasUpload<'_>) {
        self.target.upload_atlas(atlas);
    }

    pub(crate) fn set_minimum_contrast(&mut self, ratio: f32) {
        self.target.set_minimum_contrast(ratio);
    }

    pub(crate) fn set_background(&mut self, settings: &sumie_config::Window) -> (u8, bool) {
        let alpha = background_alpha(settings.background_opacity());
        let transparent = self.target.set_transparent(alpha != u8::MAX);
        let changed = self
            .window
            .set_background_effects(transparent, transparent && settings.background_blur());
        (if transparent { alpha } else { u8::MAX }, changed)
    }

    pub(crate) fn background_commit_needed(&self) -> bool {
        self.window.background_commit_needed()
    }

    pub(crate) const fn present_count(&self) -> u64 {
        self.target.present_count()
    }

    pub(crate) const fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub(crate) fn set_cursor_rectangle(&mut self, rect: CursorRect) {
        self.window.set_cursor_rectangle(rect);
    }

    pub(crate) fn resize(&mut self, device: [u32; 2]) {
        self.target.resize(device);
    }
}
