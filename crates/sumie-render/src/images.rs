use std::{cmp::Reverse, num::NonZeroU64, ops::Range};

use sumie_core::graphics::{IMAGE_BUDGET, Image, Placement, texture_bytes};
use wgpu::util::StagingBelt;

use crate::{DrawList, PixelRect, atlas};

mod cache;

const BELOW_BACKGROUND: i32 = -(1 << 30);
pub(crate) const QUAD_SIZE: usize = size_of::<Quad>();

type Exclusions = [[f32; 4]; 2];

pub(crate) struct Images {
    format: wgpu::TextureFormat,
    pipeline: Option<BoundPipeline>,
    cache: cache::Cache<wgpu::BindGroup>,
    draws: Vec<Draw>,
}

pub(crate) struct Layers {
    pub below_background: u32,
    pub below_text: u32,
    pub end: u32,
    pub base: Option<PixelRect>,
}

struct Draw {
    index: usize,
    bytes: usize,
    quad: Quad,
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

    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        list: &DrawList<'_>,
        target: [u32; 2],
        capacity: u32,
    ) -> Option<Layers> {
        let clip = Clip::grid(list, target)?;
        if capacity == 0 {
            return None;
        }
        let dimension_limit = device.limits().max_texture_dimension_2d;
        let ndc_scale = [2.0 / f64::from(target[0]), -2.0 / f64::from(target[1])];
        self.draws.clear();
        self.draws
            .extend(list.images.iter().enumerate().filter_map(|(index, p)| {
                let bytes = texture_bytes(p.image.size, dimension_limit)?;
                if p.image.rgba.len() != bytes {
                    return None;
                }
                Some(Draw {
                    index,
                    bytes,
                    quad: Quad::clipped(p, list, clip, ndc_scale)?,
                })
            }));
        // Keep the foremost images when the visible working set exceeds the budget.
        self.draws.sort_unstable_by_key(|draw| {
            let p = &list.images[draw.index];
            Reverse((p.z, p.image_id, p.placement_id, draw.index))
        });
        let format = self.format;
        self.cache.begin_frame(
            self.draws
                .iter()
                .map(|draw| list.images[draw.index].image.generation),
        );
        let (cache, pipeline) = (&mut self.cache, &mut self.pipeline);
        let mut remaining = capacity;
        self.draws.retain(|draw| {
            if remaining == 0 {
                return false;
            }
            let image = &list.images[draw.index].image;
            let admitted = cache.acquire(image.generation, draw.bytes, || {
                pipeline
                    .get_or_insert_with(|| BoundPipeline::new(device, format))
                    .bind_texture(device, queue, image)
            });
            remaining -= u32::from(admitted);
            admitted
        });
        self.draws.reverse();
        let end = capacity - remaining;
        if end == 0 {
            return None;
        }
        let boundary =
            |z| u32::try_from(self.draws.partition_point(|d| list.images[d.index].z < z)).unwrap();
        let below_background = boundary(BELOW_BACKGROUND);
        Some(Layers {
            below_background,
            below_text: boundary(0),
            end,
            base: (below_background != 0).then(|| clip.pixel_rect()),
        })
    }

    pub(crate) fn stage_exclusions(
        &mut self,
        staging: &mut StagingBelt,
        encoder: &mut wgpu::CommandEncoder,
        exclusions: [Option<PixelRect>; 2],
    ) {
        let pipeline = self.pipeline.as_mut().expect("prepared image pipeline");
        if exclusions != pipeline.last_exclusions {
            let edges: Exclusions = exclusions.map(|rect| rect.map_or([0.0; 4], pixel_edges));
            staging
                .write_buffer(
                    encoder,
                    &pipeline.exclusions,
                    0,
                    NonZeroU64::new(size_of::<Exclusions>() as u64).unwrap(),
                )
                .copy_from_slice(bytemuck::bytes_of(&edges));
            pipeline.last_exclusions = exclusions;
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
        placements: &[Placement],
        range: Range<u32>,
        vertices: wgpu::BufferSlice<'_>,
    ) {
        if range.is_empty() {
            return;
        }
        let pipeline = self.pipeline.as_ref().expect("prepared image pipeline");
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(1, &pipeline.exclusions_bindings, &[]);
        pass.set_vertex_buffer(0, vertices);
        let generation = |draw: &Draw| placements[draw.index].image.generation;
        let mut start = range.start;
        let draws = &self.draws[range.start as usize..range.end as usize];
        for group in draws.chunk_by(|a, b| generation(a) == generation(b)) {
            let end = start + u32::try_from(group.len()).unwrap();
            pass.set_bind_group(0, self.cache.get(generation(&group[0])), &[]);
            pass.draw(0..4, start..end);
            start = end;
        }
    }
}

#[derive(Clone, Copy)]
struct Clip([[i64; 2]; 2]);

impl Clip {
    fn grid(list: &DrawList<'_>, target: [u32; 2]) -> Option<Self> {
        let bounds = std::array::from_fn(|axis| {
            let start = i64::from(list.origin[axis]);
            let extent = u64::from(list.grid_size[axis]) * u64::from(list.cell_size[axis]);
            let end = start.saturating_add(i64::try_from(extent).unwrap_or(i64::MAX));
            [start.max(0), end.min(i64::from(target[axis]))]
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

#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct Quad {
    dest: [f32; 4],
    source: [f32; 4],
    // A negative exclusive end marks an axis drawn at native size.
    crop: [i32; 4],
}

impl Quad {
    const ATTRS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Sint32x4];

    #[expect(
        clippy::cast_possible_truncation,
        reason = "GPU vertex coordinates are f32"
    )]
    fn clipped(p: &Placement, frame: &DrawList<'_>, clip: Clip, ndc: [f64; 2]) -> Option<Self> {
        let axis = |i: usize| {
            let source_size = p.source[2 + i];
            let source_start = p.source[i];
            let texture_size = p.image.size[i];
            if source_size == 0 || source_start > texture_size.checked_sub(source_size)? {
                return None;
            }
            let start = i64::from(p.cell[i]) * i64::from(frame.cell_size[i])
                + i64::from(frame.origin[i])
                + i64::from(p.offset[i]);
            let [lo, hi] = clip.0[i];
            let first = start.max(lo);
            let last = start.saturating_add(i64::from(p.size[i])).min(hi);
            if first >= last {
                return None;
            }
            let screen = |edge| {
                let pixel = f64::from(u32::try_from(edge).unwrap());
                (pixel * ndc[i] - ndc[i].signum()) as f32
            };
            let source = |edge| {
                let offset = f64::from(u32::try_from(edge - start).unwrap());
                (f64::from(source_start) + offset * f64::from(source_size) / f64::from(p.size[i]))
                    as f32
            };
            let end = i32::try_from(source_start + source_size).ok()?;
            Some((
                [screen(first), screen(last)],
                [source(first), source(last)],
                [
                    i32::try_from(source_start).ok()?,
                    if source_size == p.size[i] { -end } else { end },
                ],
            ))
        };
        let ([left, right], [x0, x1], [crop_left, crop_right]) = axis(0)?;
        let ([top, bottom], [y0, y1], [crop_top, crop_bottom]) = axis(1)?;
        Some(Self {
            dest: [left, top, right, bottom],
            source: [x0, y0, x1, y1],
            crop: [crop_left, crop_top, crop_right, crop_bottom],
        })
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
    last_exclusions: [Option<PixelRect>; 2],
}

impl BoundPipeline {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let pipeline = pipeline(device, format);
        let texture_layout = pipeline.get_bind_group_layout(0);
        let exclusions = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sumie image exclusions"),
            size: size_of::<Exclusions>() as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: false,
        });
        let exclusions_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sumie image exclusion bindings"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: exclusions.as_entire_binding(),
            }],
        });
        Self {
            pipeline,
            texture_layout,
            exclusions,
            exclusions_bindings,
            last_exclusions: [None; 2],
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

fn pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sumie image shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("images.wgsl").into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sumie image pipeline"),
        layout: None,
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
