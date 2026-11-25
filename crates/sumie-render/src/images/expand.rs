//! Expands RGB, gray and gray-alpha images to RGBA on the GPU.
//!
//! The source bytes are copied unchanged into a scratch `R32Uint` texture, and a render pass
//! writes each RGBA texel. The main thread never touches individual pixels.

use std::num::NonZeroU64;

use sumie_core::graphics::Image;
use wgpu::util::DeviceExt;

/// Scratch rows are allocated in steps of this many rows to limit reallocation.
const ROW_STEP: u32 = 64;

pub(crate) struct Expander {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// Packed source words, kept between frames like the conversion buffer it replaces.
    scratch: Option<wgpu::Texture>,
    /// The last partial scratch row, padded to whole words.
    tail: Vec<u8>,
}

impl Expander {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sumie image expansion"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(16),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sumie image expansion"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sumie image expansion"),
            source: wgpu::ShaderSource::Wgsl(include_str!("expand.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sumie image expansion"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fragment"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(super::TEXTURE_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            layout,
            scratch: None,
            tail: Vec::new(),
        }
    }

    /// Copies `pixels` unchanged into the scratch texture and returns their first word.
    /// `row` is the first scratch row this command encoder has not used; queue writes land
    /// before the encoder runs, so each image in one submission takes its own rows.
    pub(super) fn stage(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        row: &mut u32,
        pixels: &[u8],
    ) -> u32 {
        // Packed sources have at most three channels, so one image always fits a square
        // scratch texture of the maximum dimension.
        let stride = device.limits().max_texture_dimension_2d;
        let row_bytes = stride as usize * 4;
        let rows = u32::try_from(pixels.len().div_ceil(row_bytes)).unwrap();
        let scratch = match &self.scratch {
            Some(scratch) if *row + rows <= scratch.height() => scratch,
            // Earlier images in this submission keep the old texture alive.
            _ => {
                let height = (*row + rows).next_multiple_of(ROW_STEP).min(stride);
                *row = 0;
                self.scratch.insert(scratch(device, [stride, height]))
            }
        };
        let base = *row;
        *row += rows;
        let full = pixels.len() / row_bytes;
        let (body, rest) = pixels.split_at(full * row_bytes);
        let copy = |origin_row: u32, data: &[u8], width: u32, height: u32| {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: scratch,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: origin_row,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: None,
                },
                crate::atlas::extent([width, height]),
            );
        };
        if full > 0 {
            copy(base, body, stride, u32::try_from(full).unwrap());
        }
        if !rest.is_empty() {
            self.tail.clear();
            self.tail.extend_from_slice(rest);
            self.tail.resize(rest.len().next_multiple_of(4), 0);
            let width = u32::try_from(self.tail.len() / 4).unwrap();
            copy(base + u32::try_from(full).unwrap(), &self.tail, width, 1);
        }
        base * stride
    }

    /// Records a pass that expands the staged words starting at `base` into `texture`.
    pub(super) fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        image: &Image,
        base: u32,
    ) {
        let scratch = self.scratch.as_ref().expect("staged before drawing");
        let channels = u32::try_from(image.format.channels()).unwrap();
        let source: Vec<u8> = [image.size[0], channels, base, scratch.width()]
            .iter()
            .flat_map(|word| word.to_ne_bytes())
            .collect();
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sumie image expansion source"),
            contents: &source,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sumie image expansion"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &scratch.create_view(&wgpu::TextureViewDescriptor::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Like a staged copy, this pass follows earlier uploads and precedes the frame's draw.
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("sumie image expansion"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn scratch(device: &wgpu::Device, size: [u32; 2]) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sumie image expansion scratch"),
        size: crate::atlas::extent(size),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Uint,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}
