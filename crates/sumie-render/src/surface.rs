use thiserror::Error;
use wgpu::CurrentSurfaceTexture;

use crate::{AtlasUpload, DrawList, Renderer};

#[derive(Debug, Error)]
pub enum SurfaceError {
    #[error("the adapter and surface have no compatible sRGB format")]
    UnsupportedFormat,
    #[error("the surface is outdated; reconfigure it before drawing")]
    Outdated,
    #[error("the surface was lost; recreate it before drawing")]
    Lost,
    #[error("wgpu rejected surface texture acquisition")]
    Validation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceFrame {
    /// Presented once. If suboptimal, reconfigure before a subsequent draw.
    Presented {
        suboptimal: bool,
    },
    /// The device-pixel size has a zero dimension.
    Suspended,
    Timeout,
    Occluded,
}

/// A renderer and its caller-created surface. Drop on the render thread before the native window.
pub struct SurfaceRenderer<'window> {
    // Rust drops fields in declaration order; the surface outlives the renderer's resources.
    renderer: Renderer,
    config: wgpu::SurfaceConfiguration,
    present_count: u64,
    surface: wgpu::Surface<'window>,
}

impl<'window> SurfaceRenderer<'window> {
    /// Take an existing surface, configure it for nonzero `size`, and use the given device and queue.
    ///
    /// # Errors
    /// Returns [`SurfaceError::UnsupportedFormat`] if the surface has no sRGB format on `adapter`.
    pub fn new(
        surface: wgpu::Surface<'window>,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: [u32; 2],
    ) -> Result<Self, SurfaceError> {
        let caps = surface.get_capabilities(adapter);
        let format = caps
            .formats
            .into_iter()
            .find(wgpu::TextureFormat::is_srgb)
            .ok_or(SurfaceError::UnsupportedFormat)?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: 0,
            height: 0,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: Vec::new(),
        };
        let mut target = Self {
            renderer: Renderer::with_format(device, queue, format),
            config,
            present_count: 0,
            surface,
        };
        target.resize(size);
        Ok(target)
    }

    /// Resize or reconfigure in device pixels. wgpu may wait for the previous swapchain.
    /// Either dimension being zero suspends draws without configuring the surface.
    pub fn resize(&mut self, [width, height]: [u32; 2]) {
        self.config.width = width;
        self.config.height = height;
        if width != 0 && height != 0 {
            self.surface.configure(&self.renderer.device, &self.config);
        }
    }

    /// Queue atlas transfers for the next draw. Do not hold a terminal lock.
    pub fn upload_atlas(&mut self, upload: &AtlasUpload<'_>) {
        self.renderer.upload_atlas(upload);
    }

    /// Submit one render pass and present once if a surface texture is acquired.
    /// Do not hold a terminal lock. On Wayland, request frame callbacks before calling this.
    ///
    /// # Errors
    /// Reports outdated, lost, or invalid surface texture acquisition.
    pub fn draw(&mut self, list: &DrawList<'_>) -> Result<SurfaceFrame, SurfaceError> {
        if self.config.width == 0 || self.config.height == 0 {
            return Ok(SurfaceFrame::Suspended);
        }
        let (frame, suboptimal) = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) => (frame, false),
            CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            CurrentSurfaceTexture::Timeout => return Ok(SurfaceFrame::Timeout),
            CurrentSurfaceTexture::Occluded => return Ok(SurfaceFrame::Occluded),
            CurrentSurfaceTexture::Outdated => return Err(SurfaceError::Outdated),
            CurrentSurfaceTexture::Lost => return Err(SurfaceError::Lost),
            CurrentSurfaceTexture::Validation => return Err(SurfaceError::Validation),
        };
        {
            let view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            let size = [frame.texture.width(), frame.texture.height()];
            self.renderer.draw_view(list, &view, size);
        }
        self.renderer.queue.present(frame);
        self.present_count += 1;
        Ok(SurfaceFrame::Presented { suboptimal })
    }

    /// Number of calls to wgpu's present for this surface over its lifetime.
    #[must_use]
    pub const fn present_count(&self) -> u64 {
        self.present_count
    }
}
