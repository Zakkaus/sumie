use sumie_render::{
    AtlasUpload, FrameDraw, PaneDraw, SurfaceError, SurfaceFrame, SurfaceRenderer, wgpu,
};

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

pub(crate) struct Gpu {
    target: SurfaceRenderer<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    _instance: wgpu::Instance,
}

impl Gpu {
    pub(crate) fn new(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        capture: bool,
        minimum_contrast: f32,
    ) -> Result<Self> {
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
        })
    }

    pub(crate) fn image_dimension_limit(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    pub(crate) fn presented_images(
        &self,
    ) -> impl Iterator<Item = (usize, usize, sumie_render::PixelRect)> + '_ {
        self.target.presented_image_layers()
    }

    pub(crate) fn viewer_presented(&self, pane: usize) -> bool {
        self.target.viewer_presented(pane)
    }

    pub(crate) fn draw<'a>(
        &mut self,
        window: &mut impl FrameWindow,
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
            window.arm_frame_callback();
        })?;
        Ok((result, readback))
    }

    pub(crate) fn upload_atlas(&mut self, atlas: &AtlasUpload<'_>) {
        self.target.upload_atlas(atlas);
    }

    pub(crate) fn set_minimum_contrast(&mut self, ratio: f32) {
        self.target.set_minimum_contrast(ratio);
    }

    pub(crate) fn set_transparent(&mut self, transparent: bool) -> bool {
        self.target.set_transparent(transparent)
    }

    pub(crate) const fn present_count(&self) -> u64 {
        self.target.present_count()
    }

    pub(crate) fn log_present(&self) {
        tracing::debug!(
            count = self.present_count(),
            image_textures = self.target.image_texture_count(),
            "submitted terminal frame"
        );
    }

    pub(crate) const fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub(crate) fn resize(&mut self, device: [u32; 2]) {
        self.target.resize(device);
    }
}
