use std::{num::NonZeroU64, ops::Range};

use sumie_core::graphics::{IMAGE_BUDGET, Image, Placement, texture_bytes};
use sumie_core::splits::Rect;
use wgpu::util::StagingBelt;

use crate::{DrawList, PaneDraw, PixelRect, atlas, grid::Instance};

mod cache;
mod quad;

use quad::Quad;

const BELOW_BACKGROUND: i32 = -(1 << 30);
pub(crate) const QUAD_SIZE: usize = size_of::<Quad>();
const FILL_SIZE: u64 = size_of::<Instance>() as u64;
const VIEWER_SIZE: u64 = QUAD_SIZE as u64 + FILL_SIZE;

type Exclusions = [[f32; 4]; 2];

pub(crate) struct Images {
    format: wgpu::TextureFormat,
    pipeline: Option<BoundPipeline>,
    cache: cache::Cache<wgpu::BindGroup>,
    draws: Vec<Draw>,
}

pub(crate) struct Layers {
    pub start: u32,
    pub below_background: u32,
    pub below_text: u32,
    pub terminal_end: u32,
    pub end: u32,
    pub base: Option<PixelRect>,
    pub has_thumbnail: bool,
}

pub(crate) struct Candidates {
    range: Range<usize>,
    base: Option<PixelRect>,
    viewer: bool,
}

#[derive(Clone, Copy)]
struct Draw {
    index: usize,
    generation: u64,
    bytes: usize,
    quad: Quad,
    bounds: PixelRect,
}

impl Draw {
    fn new(
        index: usize,
        p: &Placement,
        origin: [i64; 2],
        size: [u32; 2],
        clip: Clip,
        ndc: [f64; 2],
        dimension_limit: u32,
    ) -> Option<Self> {
        let bytes = texture_bytes(p.image.size, dimension_limit)?;
        if p.image.rgba.len() != bytes {
            return None;
        }
        let bounds = clip.intersect(origin, size)?;
        Some(Self {
            index,
            generation: p.image.generation,
            bytes,
            quad: Quad::clipped(p, origin, size, bounds, ndc)?,
            bounds: bounds.pixel_rect(),
        })
    }
}

impl Images {
    pub(crate) fn new(format: wgpu::TextureFormat) -> Self {
        Self {
            format,
            pipeline: None,
            cache: cache::Cache::new(IMAGE_BUDGET),
            draws: Vec::new(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.draws.clear();
    }

    pub(crate) fn gather(
        &mut self,
        pane: &PaneDraw<'_>,
        target: [u32; 2],
        scissor: Rect,
        dimension_limit: u32,
    ) -> Option<Candidates> {
        let list = &pane.draw;
        let grid = Clip::grid(list, scissor);
        let ndc = [2.0 / f64::from(target[0]), -2.0 / f64::from(target[1])];
        let start = self.draws.len();
        if let Some(clip) = grid {
            self.draws
                .extend(list.images.iter().enumerate().filter_map(|(index, p)| {
                    let origin = std::array::from_fn(|i| {
                        i64::from(p.cell[i]) * i64::from(list.cell_size[i])
                            + i64::from(list.origin[i])
                            + i64::from(p.offset[i])
                    });
                    let mut draw = Draw::new(index, p, origin, p.size, clip, ndc, dimension_limit)?;
                    if let Some(style) = pane.thumbnails.filter(|_| p.thumbnail) {
                        draw.quad
                            .decorate(origin, p.size, style, style.hovered == Some(index));
                    }
                    Some(draw)
                }));
            // Larger z values are foremost, and selected first if the frame fills the cache.
            self.draws[start..].sort_unstable_by_key(|draw| {
                let p = &list.images[draw.index];
                (p.z, p.image_id, p.placement_id, draw.index)
            });
        }
        let viewer = pane.viewer.and_then(|viewer| {
            Draw::new(
                list.images.len(),
                viewer.placement,
                viewer.dest.origin.map(i64::from),
                viewer.dest.size,
                Clip::scissor(scissor),
                ndc,
                dimension_limit,
            )
        });
        let has_viewer = viewer.is_some();
        self.draws.extend(viewer);
        let end = self.draws.len();
        (start != end).then(|| Candidates {
            range: start..end,
            base: grid.map(Clip::pixel_rect),
            viewer: has_viewer,
        })
    }

    pub(crate) fn begin_frame(&mut self) -> bool {
        if self.draws.is_empty() {
            return false;
        }
        self.cache
            .begin_frame(self.draws.iter().map(|draw| draw.generation));
        true
    }

    pub(crate) fn reserve_viewer(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        placement: &Placement,
        candidates: &mut Candidates,
        remaining: &mut u64,
    ) {
        if candidates.viewer {
            if *remaining >= VIEWER_SIZE
                && self.acquire(device, queue, candidates.range.end - 1, placement)
            {
                *remaining -= VIEWER_SIZE;
            } else {
                candidates.range.end -= 1;
                candidates.viewer = false;
            }
        }
    }

    fn acquire(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        index: usize,
        placement: &Placement,
    ) -> bool {
        let draw = &self.draws[index];
        self.cache.acquire(draw.generation, draw.bytes, || {
            self.pipeline
                .get_or_insert_with(|| BoundPipeline::new(device, self.format))
                .bind_texture(device, queue, &placement.image)
        })
    }

    pub(crate) fn admit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        list: &DrawList<'_>,
        candidates: &Candidates,
        remaining: &mut u64,
        start: u32,
    ) -> Option<Layers> {
        let viewer = candidates.viewer;
        let end = candidates.range.end;
        let terminal = end - usize::from(viewer);
        let mut first = terminal;
        let mut has_base = false;
        for index in (candidates.range.start..terminal).rev() {
            let placement = &list.images[self.draws[index].index];
            let needs_base = !has_base && placement.z < BELOW_BACKGROUND;
            // The base costs at most one fill; it may replace default backgrounds.
            let cost = QUAD_SIZE as u64 + u64::from(needs_base) * FILL_SIZE;
            if *remaining < cost {
                break;
            }
            if self.acquire(device, queue, index, placement) {
                *remaining -= cost;
                has_base |= needs_base;
                first -= 1;
                if first != index {
                    self.draws[first] = self.draws[index];
                }
            }
        }
        if first == end {
            return None;
        }
        let boundary = |z| {
            let count = self.draws[first..terminal].partition_point(|d| list.images[d.index].z < z);
            start + u32::try_from(count).unwrap()
        };
        let below_background = boundary(BELOW_BACKGROUND);
        let below_text = boundary(0);
        let has_thumbnail = !viewer
            && self.draws[first..terminal]
                .iter()
                .any(|d| list.images[d.index].thumbnail);
        let terminal_end = start + u32::try_from(terminal - first).unwrap();
        // Pack admitted placements without touching candidates for the following panes.
        if first != start as usize {
            self.draws.copy_within(first..end, start as usize);
        }
        Some(Layers {
            start,
            below_background,
            below_text,
            terminal_end,
            end: terminal_end + u32::from(viewer),
            base: candidates.base.filter(|_| has_base),
            has_thumbnail,
        })
    }

    pub(crate) fn bounds(
        &self,
        range: Range<u32>,
    ) -> impl Iterator<Item = (usize, PixelRect)> + '_ {
        self.draws[range.start as usize..range.end as usize]
            .iter()
            .map(|draw| (draw.index, draw.bounds))
    }

    pub(crate) fn finish_frame(&mut self, images: u32) {
        self.draws.truncate(images as usize);
    }

    pub(crate) fn stage_exclusions(
        &mut self,
        device: &wgpu::Device,
        staging: &mut StagingBelt,
        encoder: &mut wgpu::CommandEncoder,
        count: u32,
        exclusions: impl Iterator<Item = [Option<PixelRect>; 2]>,
    ) {
        if count == 1 {
            return;
        }
        let pipeline = self.pipeline.as_mut().expect("prepared image pipeline");
        let stride = pipeline.exclusion_stride;
        let bytes = u64::from(stride) * u64::from(count);
        pipeline.reserve_exclusions(device, bytes);
        // Slot zero always holds empty exclusions. Do not overwrite it on later frames.
        let mut out = staging.write_buffer(
            encoder,
            &pipeline.exclusions,
            u64::from(stride),
            NonZeroU64::new(bytes - u64::from(stride)).unwrap(),
        );
        for (slot, exclusions) in exclusions.enumerate() {
            let start = slot * stride as usize;
            let edges: Exclusions = exclusions.map(|rect| rect.map_or([0.0; 4], pixel_edges));
            out.slice(start..start + size_of::<Exclusions>())
                .copy_from_slice(bytemuck::bytes_of(&edges));
        }
    }

    pub(crate) fn write_instances(&self, bytes: wgpu::WriteOnly<'_, [u8]>) {
        for (out, draw) in bytes
            .into_chunks::<QUAD_SIZE>()
            .0
            .into_iter()
            .zip(&self.draws)
        {
            out.write(bytemuck::cast(draw.quad));
        }
    }

    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        exclusions: u32,
        range: Range<u32>,
        vertices: wgpu::BufferSlice<'_>,
    ) {
        if range.is_empty() {
            return;
        }
        let pipeline = self.pipeline.as_ref().expect("prepared image pipeline");
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(
            1,
            &pipeline.exclusions_bindings,
            &[exclusions * pipeline.exclusion_stride],
        );
        pass.set_vertex_buffer(0, vertices);
        let mut start = range.start;
        let draws = &self.draws[range.start as usize..range.end as usize];
        for group in draws.chunk_by(|a, b| a.generation == b.generation) {
            let end = start + u32::try_from(group.len()).unwrap();
            pass.set_bind_group(0, self.cache.get(group[0].generation), &[]);
            pass.draw(0..4, start..end);
            start = end;
        }
    }
}

#[derive(Clone, Copy)]
struct Clip([[i64; 2]; 2]);

impl Clip {
    fn scissor(rect: Rect) -> Self {
        Self([
            [i64::from(rect.x), i64::from(rect.x + rect.width)],
            [i64::from(rect.y), i64::from(rect.y + rect.height)],
        ])
    }

    fn intersect(self, start: [i64; 2], size: [u32; 2]) -> Option<Self> {
        let bounds = std::array::from_fn(|i| {
            let [lo, hi] = self.0[i];
            [
                start[i].max(lo),
                start[i].saturating_add(i64::from(size[i])).min(hi),
            ]
        });
        bounds
            .iter()
            .all(|[lo, hi]| lo < hi)
            .then_some(Self(bounds))
    }

    fn grid(list: &DrawList<'_>, scissor: Rect) -> Option<Self> {
        let lo = [scissor.x, scissor.y];
        let hi = [scissor.x + scissor.width, scissor.y + scissor.height];
        let bounds = std::array::from_fn(|axis| {
            let start = i64::from(list.origin[axis]);
            let extent = u64::from(list.grid_size[axis]) * u64::from(list.cell_size[axis]);
            let end = start.saturating_add(i64::try_from(extent).unwrap_or(i64::MAX));
            [start.max(i64::from(lo[axis])), end.min(i64::from(hi[axis]))]
        });
        bounds
            .iter()
            .all(|[start, end]| start < end)
            .then_some(Self(bounds))
    }

    fn pixel_rect(self) -> PixelRect {
        let [[left, right], [top, bottom]] = self.0;
        PixelRect {
            origin: [i32::try_from(left).unwrap(), i32::try_from(top).unwrap()],
            size: [
                u32::try_from(right - left).unwrap(),
                u32::try_from(bottom - top).unwrap(),
            ],
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "GPU pixel rectangles are f32"
)]
fn pixel_edges(rect: PixelRect) -> [f32; 4] {
    let [x, y] = rect.origin.map(f64::from);
    let [w, h] = rect.size.map(f64::from);
    [x as f32, y as f32, (x + w) as f32, (y + h) as f32]
}

struct BoundPipeline {
    pipeline: wgpu::RenderPipeline,
    texture_layout: wgpu::BindGroupLayout,
    exclusions: wgpu::Buffer,
    exclusions_bindings: wgpu::BindGroup,
    exclusion_stride: u32,
}

impl BoundPipeline {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sumie image textures"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let exclusions_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sumie image exclusions"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(size_of::<Exclusions>() as u64),
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sumie image layouts"),
            bind_group_layouts: &[Some(&texture_layout), Some(&exclusions_layout)],
            immediate_size: 0,
        });
        let pipeline = pipeline(device, format, &layout);
        let alignment = device.limits().min_uniform_buffer_offset_alignment;
        let size = u32::try_from(size_of::<Exclusions>()).unwrap();
        let exclusion_stride = size.div_ceil(alignment) * alignment;
        let (exclusions, exclusions_bindings) =
            exclusions_buffer(device, &exclusions_layout, u64::from(exclusion_stride));
        Self {
            pipeline,
            texture_layout,
            exclusions,
            exclusions_bindings,
            exclusion_stride,
        }
    }

    fn reserve_exclusions(&mut self, device: &wgpu::Device, bytes: u64) {
        if bytes > self.exclusions.size() {
            (self.exclusions, self.exclusions_bindings) = exclusions_buffer(
                device,
                &self.pipeline.get_bind_group_layout(1),
                bytes
                    .next_power_of_two()
                    .min(device.limits().max_buffer_size),
            );
        }
    }

    fn bind_texture(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &Image,
    ) -> wgpu::BindGroup {
        let size = atlas::extent(image.size);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sumie terminal image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            &image.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.size[0] * 4),
                rows_per_image: None,
            },
            size,
        );
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sumie image texture bindings"),
            layout: &self.texture_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(
                    &texture.create_view(&wgpu::TextureViewDescriptor::default()),
                ),
            }],
        })
    }
}

fn exclusions_buffer(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    bytes: u64,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sumie image exclusions"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: false,
    });
    let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sumie image exclusion bindings"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &buffer,
                offset: 0,
                size: NonZeroU64::new(size_of::<Exclusions>() as u64),
            }),
        }],
    });
    (buffer, bindings)
}

fn pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    layout: &wgpu::PipelineLayout,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sumie image shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("images.wgsl").into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sumie image pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vertex"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: QUAD_SIZE as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &Quad::ATTRS,
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fragment"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
