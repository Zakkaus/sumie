use std::sync::Once;

use thiserror::Error;
use wgpu::{CompositeAlphaMode, CurrentSurfaceTexture};

use crate::{AtlasUpload, DrawList, FrameDraw, PaneDraw, PixelRect, Renderer};

static TRANSPARENCY_WARNING: Once = Once::new();

#[derive(Debug, Error)]
pub enum SurfaceError {
    #[error("the adapter and surface have no compatible sRGB format")]
    UnsupportedFormat,
    #[error("the adapter and surface have no compatible composite alpha mode")]
    UnsupportedAlphaMode,
    #[error("the surface is outdated; reconfigure it before drawing")]
    Outdated,
    #[error("the surface was lost; recreate it before drawing")]
    Lost,
    #[error("wgpu rejected surface texture acquisition")]
    Validation,
    #[error("the surface does not support copying the frame for readback")]
    NoReadback,
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

/// A renderer and its caller-created surface. Drop on the render thread.
pub struct SurfaceRenderer<'window> {
    // Rust drops fields in declaration order; the surface outlives the renderer's resources.
    renderer: Renderer,
    config: wgpu::SurfaceConfiguration,
    opaque_alpha: CompositeAlphaMode,
    transparent_alpha: Option<CompositeAlphaMode>,
    active_alpha: Option<CompositeAlphaMode>,
    present_count: u64,
    surface: wgpu::Surface<'window>,
}

impl<'window> SurfaceRenderer<'window> {
    /// Take an existing surface, configure it for nonzero `size`, and use the given device and queue.
    ///
    /// # Errors
    /// Returns an error if the surface lacks an sRGB format or a compatible alpha mode.
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
        let transparent_alpha = [
            CompositeAlphaMode::PreMultiplied,
            CompositeAlphaMode::PostMultiplied,
        ]
        .into_iter()
        .find(|mode| caps.alpha_modes.contains(mode));
        let opaque_alpha = [CompositeAlphaMode::Opaque, CompositeAlphaMode::Inherit]
            .into_iter()
            .find(|mode| caps.alpha_modes.contains(mode))
            .or(transparent_alpha)
            .ok_or(SurfaceError::UnsupportedAlphaMode)?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: 0,
            height: 0,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: opaque_alpha,
            view_formats: Vec::new(),
        };
        let mut target = Self {
            renderer: Renderer::with_format(device, queue, format),
            config,
            opaque_alpha,
            transparent_alpha,
            active_alpha: None,
            present_count: 0,
            surface,
        };
        target.resize(size);
        Ok(target)
    }

    /// Request desktop compositing and return whether transparency is effective.
    /// Supply translucent backgrounds only when this returns `true`. A change may wait for the
    /// previous swapchain; call on the render thread with no acquired surface texture outstanding.
    pub fn set_transparent(&mut self, transparent: bool) -> bool {
        let requested_alpha = self.transparent_alpha.filter(|_| transparent);
        if transparent && requested_alpha.is_none() {
            TRANSPARENCY_WARNING.call_once(|| {
                eprintln!(
                    "sumie: this surface does not support transparency; using an opaque background"
                );
            });
        }
        if requested_alpha != self.active_alpha {
            self.active_alpha = requested_alpha;
            self.renderer.set_surface_alpha(requested_alpha);
        }
        let mode = requested_alpha.unwrap_or(self.opaque_alpha);
        if mode != self.config.alpha_mode {
            self.config.alpha_mode = mode;
            self.resize([self.config.width, self.config.height]);
        }
        requested_alpha.is_some()
    }

    /// Whether readback from the current translucent surface uses premultiplied sRGB.
    #[must_use]
    pub const fn is_premultiplied(&self) -> bool {
        matches!(self.active_alpha, Some(CompositeAlphaMode::PreMultiplied))
    }

    /// Set the minimum WCAG contrast ratio for text in subsequent draws.
    /// Defaults to `1.1`; `1.0` disables correction.
    pub fn set_minimum_contrast(&mut self, minimum_contrast: f32) {
        self.renderer.set_minimum_contrast(minimum_contrast);
    }

    /// Enable copying acquired frames before configuring the initial device size.
    ///
    /// # Errors
    /// Returns an error if the adapter cannot copy from the surface.
    pub fn enable_readback(&mut self, adapter: &wgpu::Adapter) -> Result<(), SurfaceError> {
        if !self
            .surface
            .get_capabilities(adapter)
            .usages
            .contains(wgpu::TextureUsages::COPY_SRC)
        {
            return Err(SurfaceError::NoReadback);
        }
        self.config.usage |= wgpu::TextureUsages::COPY_SRC;
        self.resize([self.config.width, self.config.height]);
        Ok(())
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

    /// Submit one frame and present once if a surface texture is acquired.
    /// Do not hold a terminal lock. On Wayland, request frame callbacks before calling this.
    ///
    /// # Errors
    /// Reports outdated, lost, or invalid surface texture acquisition.
    pub fn draw(&mut self, list: &DrawList<'_>) -> Result<SurfaceFrame, SurfaceError> {
        self.draw_with(list, wgpu::Color::TRANSPARENT, |_| {})
    }

    /// Submit a draw with a linear clear color. Run `before_present` only after acquisition and
    /// drawing succeed, just before the buffer commit. On Wayland, arm the frame callback there.
    /// The texture may be copied from the hook when readback has been enabled.
    /// The clear color uses straight alpha; the texture uses the negotiated composite alpha mode.
    ///
    /// # Errors
    /// Reports outdated, lost, or invalid surface texture acquisition.
    pub fn draw_with(
        &mut self,
        list: &DrawList<'_>,
        clear: wgpu::Color,
        before_present: impl FnOnce(&wgpu::Texture),
    ) -> Result<SurfaceFrame, SurfaceError> {
        self.with_surface(before_present, |renderer, view, size| {
            renderer.draw_view(list, view, [size.width, size.height], clear);
        })
    }

    /// Draw all panes and present once. Run `before_present` just before committing the buffer.
    /// On Wayland, arm the frame callback there. The hook may also copy a readback-enabled texture.
    /// The texture uses the negotiated composite alpha mode.
    /// Do not hold a terminal lock.
    ///
    /// # Errors
    /// Reports outdated, lost, or invalid surface texture acquisition.
    pub fn draw_frame_with<'a>(
        &mut self,
        frame: &FrameDraw<'a, impl Clone + Iterator<Item = PaneDraw<'a>>>,
        before_present: impl FnOnce(&wgpu::Texture),
    ) -> Result<SurfaceFrame, SurfaceError> {
        self.with_surface(before_present, |renderer, view, size| {
            renderer.draw_frame_view(frame, view, size);
        })
    }

    fn with_surface(
        &mut self,
        before_present: impl FnOnce(&wgpu::Texture),
        draw: impl FnOnce(&mut Renderer, &wgpu::TextureView, wgpu::Extent3d),
    ) -> Result<SurfaceFrame, SurfaceError> {
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
            draw(&mut self.renderer, &view, frame.texture.size());
        }
        before_present(&frame.texture);
        self.renderer.queue.present(frame);
        self.present_count += 1;
        Ok(SurfaceFrame::Presented { suboptimal })
    }

    /// Returns whether the given pane index had a viewer in the last successful presentation.
    /// The index is the pane's position in the frame's `panes` iterator.
    #[must_use]
    pub fn viewer_presented(&self, pane_index: usize) -> bool {
        self.renderer.viewer_presented(pane_index)
    }

    /// Hit layers from the last presentation, including non-thumbnail images that can cover
    /// a thumbnail. Items give the pane index, placement index, and clipped device bounds in
    /// draw order.
    pub fn presented_thumbnail_layers(&self) -> impl Iterator<Item = (usize, usize, PixelRect)> {
        self.renderer.thumbnail_layers()
    }

    /// Number of calls to wgpu's present for this surface over its lifetime.
    #[must_use]
    pub const fn present_count(&self) -> u64 {
        self.present_count
    }
}
