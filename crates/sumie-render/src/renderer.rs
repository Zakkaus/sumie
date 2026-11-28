use std::num::NonZeroU64;

use sumie_core::graphics::IMAGE_BUDGET;
use sumie_core::splits::Rect;
use wgpu::util::StagingBelt;

use crate::grid::{GridLayers, Instance, write_fills};
use crate::images::{Candidates, ImageExpansion, Images, Layers, QUAD_SIZE};
use crate::{
    AtlasUpload, ChromeLayer, DrawList, Fill, FrameDraw, PaneDraw, PixelRect, Rgba, atlas,
};

mod panes;
mod pipeline;
mod presentation;

use pipeline::{AlphaPipelines, BoundAtlas, frame_buffer, pipeline};
use presentation::Presentation;

const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const FRAME_SIZE: u64 = size_of::<Frame>() as u64;
const INSTANCE_SIZE: u64 = size_of::<Instance>() as u64;
const IMAGE_INSTANCE_SIZE: u64 = QUAD_SIZE as u64;
const INITIAL_INSTANCES: u32 = 256;

#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct Frame {
    size: [u32; 2],
    minimum_contrast: f32,
    nominal_background: u32,
}

struct LayeredFrame {
    grid: GridLayers,
    images: Layers,
    exclusion: u32,
}

struct PaneBatch {
    rect: Rect,
    base: Option<Fill>,
    backdrop: Option<Fill>,
    backgrounds: u32,
    start: u32,
    count: u32,
    candidates: Option<Candidates>,
    layered: Option<LayeredFrame>,
}

#[derive(Default)]
struct Batches {
    grid: u32,
    fills: u32,
    chrome: u32,
    images: u32,
    exclusions: u32,
    translucent: bool,
}

impl Batches {
    fn image_offset(&self) -> u64 {
        FRAME_SIZE + u64::from(self.grid + self.fills + self.chrome) * INSTANCE_SIZE
    }
}

/// Persistent RGBA8 sRGB render attachment and copy source on the renderer's device.
pub struct OffscreenTarget {
    view: wgpu::TextureView,
    texture: wgpu::Texture,
}

impl OffscreenTarget {
    /// Borrow the copy source for GPU readback.
    #[must_use]
    pub const fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

/// GPU resources for one render target. The device and queue may be shared.
pub struct Renderer {
    grid_atlas: BoundAtlas,
    chrome_atlas: Option<BoundAtlas>,
    images: Images,
    panes: Vec<PaneBatch>,
    frame: wgpu::Buffer,
    frame_limit: u32,
    minimum_contrast: f32,
    pipeline: wgpu::RenderPipeline,
    alpha_pipelines: Option<AlphaPipelines>,
    presentation: Option<Presentation>,
    format: wgpu::TextureFormat,
    staging: StagingBelt,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
}

impl Renderer {
    /// Create an offscreen renderer using an existing device and its queue.
    #[must_use]
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::with_format(device, queue, OFFSCREEN_FORMAT)
    }

    pub(crate) fn with_format(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Self {
        let pipeline = pipeline(device, format);
        let max_buffer = device.limits().max_buffer_size;
        let frame_limit = u32::try_from(max_buffer.saturating_sub(FRAME_SIZE) / INSTANCE_SIZE)
            .unwrap_or(u32::MAX);
        let frame = frame_buffer(device, INITIAL_INSTANCES.min(frame_limit));
        let grid_atlas = BoundAtlas::new(device, &pipeline, &frame, [[1, 1]; 2]);
        Self {
            grid_atlas,
            chrome_atlas: None,
            images: Images::new(format, IMAGE_BUDGET),
            panes: Vec::new(),
            staging: StagingBelt::new(device.clone(), frame.size()),
            frame,
            frame_limit,
            minimum_contrast: 1.1,
            pipeline,
            alpha_pipelines: None,
            presentation: None,
            format,
            device: device.clone(),
            queue: queue.clone(),
        }
    }

    /// Set the minimum WCAG contrast ratio for text in subsequent draws.
    /// Defaults to `1.1`; `1.0` disables correction.
    pub fn set_minimum_contrast(&mut self, minimum_contrast: f32) {
        self.minimum_contrast = minimum_contrast;
    }

    /// Number of image textures in this renderer's cache, excluding other GPU resources.
    #[must_use]
    pub fn image_texture_count(&self) -> usize {
        self.images.texture_count()
    }

    /// Packed image uploads expanded on the GPU so far.
    #[must_use]
    pub fn image_expansion(&self) -> ImageExpansion {
        self.images.expansion()
    }

    pub(crate) fn set_surface_alpha(&mut self, mode: Option<wgpu::CompositeAlphaMode>) {
        self.presentation = mode.map(|mode| Presentation::new(&self.device, self.format, mode));
    }

    /// Create an offscreen target of the given nonzero device-pixel size.
    #[must_use]
    pub fn create_offscreen(&self, size: [u32; 2]) -> OffscreenTarget {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sumie offscreen"),
            size: atlas::extent(size),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        OffscreenTarget { view, texture }
    }

    /// Upload coverage and premultiplied color, in that order. Reallocation uploads a full plane.
    /// Do not hold a terminal lock.
    pub fn upload_atlas(&mut self, upload: &[AtlasUpload<'_>; 2]) {
        self.grid_atlas.upload(
            &self.device,
            &self.queue,
            &self.pipeline,
            &self.frame,
            upload,
        );
    }

    /// Submit one render pass to a target on this device. Do not hold a terminal lock.
    pub fn draw(&mut self, list: &DrawList<'_>, target: &OffscreenTarget) {
        self.draw_view(
            list,
            &target.view,
            [target.texture.width(), target.texture.height()],
            wgpu::Color::TRANSPARENT,
        );
    }

    /// Submit one render pass containing all panes and overlays. Do not hold a terminal lock.
    ///
    /// # Panics
    /// Panics if a grid does not supply exactly one background per cell or the frame exceeds
    /// the device's instance storage limit.
    pub fn draw_frame<'a>(
        &mut self,
        frame: &FrameDraw<'a, impl Clone + Iterator<Item = PaneDraw<'a>>>,
        target: &OffscreenTarget,
    ) {
        self.draw_frame_view(frame, &target.view, target.texture.size());
    }

    pub(crate) fn draw_frame_view<'a>(
        &mut self,
        frame: &FrameDraw<'a, impl Clone + Iterator<Item = PaneDraw<'a>>>,
        view: &wgpu::TextureView,
        size: wgpu::Extent3d,
    ) {
        self.draw_batches(
            frame,
            view,
            [size.width, size.height],
            clear_color(frame.background),
            true,
        );
    }

    pub(crate) fn draw_view(
        &mut self,
        list: &DrawList<'_>,
        view: &wgpu::TextureView,
        size: [u32; 2],
        mut clear: wgpu::Color,
    ) {
        if self.presentation.is_some() {
            clear.r *= clear.a;
            clear.g *= clear.a;
            clear.b *= clear.a;
        }
        let frame = FrameDraw {
            background: [0; 4],
            panes: std::iter::once(PaneDraw {
                rect: full_rect(size),
                draw: *list,
                viewer: None,
                thumbnails: None,
            }),
            fills: &[],
            chrome: list.chrome,
        };
        self.draw_batches(&frame, view, size, clear, false);
    }

    fn draw_batches<'a>(
        &mut self,
        frame: &FrameDraw<'a, impl Clone + Iterator<Item = PaneDraw<'a>>>,
        view: &wgpu::TextureView,
        size: [u32; 2],
        clear: wgpu::Color,
        draw_bases: bool,
    ) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sumie frame"),
            });
        let full = full_rect(size);
        let batches = self.prepare(frame, full, draw_bases, &mut encoder);
        if batches.translucent && self.alpha_pipelines.is_none() {
            self.alpha_pipelines = Some(AlphaPipelines::new(
                &self.device,
                self.format,
                &self.pipeline,
            ));
        }
        if let Some(presentation) = &mut self.presentation {
            presentation.resize(&self.device, size);
        }
        if batches.image_offset() > FRAME_SIZE {
            self.stage_instances(&mut encoder, frame, size, &batches);
        }
        let exclusions = frame
            .panes
            .clone()
            .zip(&self.panes)
            .filter_map(|(pane, batch)| {
                (batch.layered.as_ref()?.exclusion != 0).then_some(pane.draw.image_exclusions)
            });
        self.images.stage_exclusions(
            &self.device,
            &mut self.staging,
            &mut encoder,
            batches.exclusions,
            exclusions,
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sumie grid"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.presentation.as_ref().map_or(view, Presentation::view),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            self.draw_pass(&mut pass, &batches, full);
        }
        if let Some(presentation) = &self.presentation {
            presentation.encode(&mut encoder, view);
        }
        self.staging.finish_and_recall_on_submit(&encoder);
        self.queue.submit([encoder.finish()]);
    }

    fn bind_grid(&self, pass: &mut wgpu::RenderPass<'_>, pipeline: &wgpu::RenderPipeline) {
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.grid_atlas.bindings, &[]);
        pass.set_vertex_buffer(0, self.frame.slice(FRAME_SIZE..));
    }

    fn draw_pass(&self, pass: &mut wgpu::RenderPass<'_>, batches: &Batches, full: Rect) {
        let alpha = self
            .alpha_pipelines
            .as_ref()
            .filter(|_| batches.translucent);
        let mut last = full;
        let mut bound = false;
        for pane in &self.panes {
            if pane.count == 0 {
                continue;
            }
            let scissor = pane.rect;
            if scissor != last {
                pass.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
                last = scissor;
            }
            if let Some(layers) = &pane.layered {
                self.draw_layered(pass, pane, layers, batches.image_offset(), alpha);
                bound = false;
            } else {
                bound = self.draw_grid_pane(pass, pane, alpha, bound);
            }
        }
        if batches.fills == 0 && batches.chrome == 0 {
            return;
        }
        if last != full {
            pass.set_scissor_rect(full.x, full.y, full.width, full.height);
        }
        self.draw_overlays(pass, batches, alpha, bound);
    }

    fn draw_overlays(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        batches: &Batches,
        alpha: Option<&AlphaPipelines>,
        mut bound: bool,
    ) {
        let chrome_start = batches.grid + batches.fills;
        if batches.fills != 0 {
            if let Some(alpha) = alpha {
                self.bind_grid(pass, &alpha.tint);
                bound = false;
            } else if !bound {
                self.bind_grid(pass, &self.pipeline);
                bound = true;
            }
            pass.draw(0..4, batches.grid..chrome_start);
        }
        if batches.chrome != 0 {
            if !bound {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.frame.slice(FRAME_SIZE..));
            }
            let atlas = self.chrome_atlas.as_ref().expect("uploaded chrome atlas");
            pass.set_bind_group(0, &atlas.bindings, &[]);
            pass.draw(0..4, chrome_start..chrome_start + batches.chrome);
        }
    }

    fn prepare<'a>(
        &mut self,
        frame: &FrameDraw<'a, impl Clone + Iterator<Item = PaneDraw<'a>>>,
        full: Rect,
        draw_bases: bool,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Batches {
        let mut batches = Batches {
            fills: u32::try_from(frame.fills.len()).expect("too many fills"),
            chrome: self.prepare_chrome(frame.chrome.as_ref()),
            exclusions: 1,
            translucent: self.presentation.is_some() || (draw_bases && frame.background[3] != 255),
            ..Batches::default()
        };
        self.panes.clear();
        self.images.clear();
        let mut has_viewers = false;
        let dimension_limit = self.device.limits().max_texture_dimension_2d;
        for pane in frame.panes.clone() {
            let rect = clip_rect(pane.rect, full);
            let mut batch = PaneBatch {
                rect,
                base: None,
                backdrop: None,
                backgrounds: 0,
                start: batches.grid,
                count: 0,
                candidates: None,
                layered: None,
            };
            if rect.width != 0 && rect.height != 0 {
                if draw_bases {
                    batch.set_base(&pane, frame.background, batches.translucent);
                }
                batch.count = pane.draw.grid_instances() + u32::from(batch.base.is_some());
                if batches.translucent {
                    batch.backgrounds = u32::from(batch.base.is_some())
                        + u32::try_from(pane.draw.backgrounds.len()).expect("too many cells");
                }
                let open = pane.viewer.is_some();
                if !pane.draw.images.is_empty() || open {
                    batch.candidates =
                        self.images
                            .gather(&pane, [full.width, full.height], rect, dimension_limit);
                    has_viewers |= open;
                }
            }
            batches.grid = batches
                .grid
                .checked_add(batch.count)
                .expect("too many grid instances");
            self.panes.push(batch);
        }
        let grid_limit = batches
            .grid
            .checked_add(batches.fills)
            .and_then(|n| n.checked_add(batches.chrome))
            .expect("too many frame instances");
        if self.images.begin_frame() {
            let remaining = u64::from(self.frame_limit.saturating_sub(grid_limit)) * INSTANCE_SIZE;
            self.prepare_images(
                frame.panes.clone(),
                &mut batches,
                remaining,
                has_viewers,
                encoder,
            );
        }
        batches
    }

    fn prepare_chrome(&mut self, layer: Option<&ChromeLayer<'_>>) -> u32 {
        let Some(layer) = layer else { return 0 };
        let size = layer.atlas.map(|plane| [plane.width, plane.height]);
        let atlas = self.chrome_atlas.get_or_insert_with(|| {
            BoundAtlas::new(&self.device, &self.pipeline, &self.frame, size)
        });
        atlas.upload(
            &self.device,
            &self.queue,
            &self.pipeline,
            &self.frame,
            &layer.atlas,
        );
        layer.instance_count()
    }

    fn stage_instances<'a>(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &FrameDraw<'a, impl Clone + Iterator<Item = PaneDraw<'a>>>,
        size: [u32; 2],
        batches: &Batches,
    ) {
        let bytes = batches.image_offset() + u64::from(batches.images) * IMAGE_INSTANCE_SIZE;
        self.reserve_frame(bytes);
        let mut staging =
            self.staging
                .write_buffer(encoder, &self.frame, 0, NonZeroU64::new(bytes).unwrap());
        let (mut header, mut instances) = staging.slice(..).split_at(size_of::<Frame>());
        header.copy_from_slice(bytemuck::bytes_of(&Frame {
            size,
            minimum_contrast: self.minimum_contrast,
            nominal_background: u32::from(batches.translucent),
        }));
        let stride = size_of::<Instance>();
        for (pane, batch) in frame.panes.clone().zip(&self.panes) {
            let (mut bytes, rest) = instances.split_at(batch.count as usize * stride);
            instances = rest;
            if batch.count == 0 {
                continue;
            }
            if let Some(backdrop) = &batch.backdrop {
                let (rest, last) = bytes.split_at((batch.count as usize - 1) * stride);
                write_fills(last, std::slice::from_ref(backdrop));
                bytes = rest;
            }
            if let Some(base) = &batch.base {
                let (first, rest) = bytes.split_at(stride);
                write_fills(first, std::slice::from_ref(base));
                bytes = rest;
            }
            if let Some(layers) = &batch.layered {
                pane.draw.write_layered(
                    bytes,
                    &layers.grid,
                    batches.translucent,
                    batch.backdrop.is_some(),
                );
            } else {
                pane.draw.write_instances(bytes);
            }
        }
        let (fills, rest) = instances.split_at(batches.fills as usize * stride);
        write_fills(fills, frame.fills);
        let (chrome, images) = rest.split_at(batches.chrome as usize * stride);
        if let Some(layer) = &frame.chrome {
            layer.write_instances(chrome);
        }
        self.images.write_instances(images);
    }

    fn reserve_frame(&mut self, bytes: u64) {
        if bytes > self.frame.size() {
            let total = u32::try_from((bytes - FRAME_SIZE).div_ceil(INSTANCE_SIZE)).unwrap();
            let capacity = total
                .checked_next_power_of_two()
                .map_or(self.frame_limit, |rounded| rounded.min(self.frame_limit));
            assert!(total <= capacity, "too many frame instances");
            self.frame = frame_buffer(&self.device, capacity);
            self.grid_atlas
                .rebind(&self.device, &self.pipeline, &self.frame);
            if let Some(atlas) = &mut self.chrome_atlas {
                atlas.rebind(&self.device, &self.pipeline, &self.frame);
            }
        }
    }
}

fn full_rect([width, height]: [u32; 2]) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width,
        height,
    }
}

fn clip_rect(rect: Rect, full: Rect) -> Rect {
    Rect {
        width: rect.width.min(full.width.saturating_sub(rect.x)),
        height: rect.height.min(full.height.saturating_sub(rect.y)),
        ..rect
    }
}

fn pixel_rect(rect: Rect) -> PixelRect {
    PixelRect {
        origin: [
            i32::try_from(rect.x).unwrap(),
            i32::try_from(rect.y).unwrap(),
        ],
        size: [rect.width, rect.height],
    }
}

fn clear_color([r, g, b, a]: Rgba) -> wgpu::Color {
    let a = f64::from(a) / 255.0;
    let linear = |value| {
        let encoded = f64::from(value) / 255.0;
        a * if encoded <= 0.04045 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    };
    wgpu::Color {
        r: linear(r),
        g: linear(g),
        b: linear(b),
        a,
    }
}
