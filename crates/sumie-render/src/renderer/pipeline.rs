use std::num::NonZeroU64;

use crate::{AtlasRect, AtlasUpload, atlas, grid::Instance};

use super::{FRAME_SIZE, INSTANCE_SIZE};

pub(super) struct BoundAtlas {
    texture: [wgpu::Texture; 2],
    pub(super) bindings: wgpu::BindGroup,
    uploaded: [bool; 2],
}

impl BoundAtlas {
    pub(super) fn new(
        device: &wgpu::Device,
        pipeline: &wgpu::RenderPipeline,
        frame: &wgpu::Buffer,
        size: [[u32; 2]; 2],
    ) -> Self {
        let texture = [
            atlas::create(device, size[0], wgpu::TextureFormat::R8Unorm),
            atlas::create(device, size[1], wgpu::TextureFormat::Rgba8Unorm),
        ];
        Self {
            bindings: bindings(device, pipeline, &texture, frame),
            texture,
            uploaded: [false; 2],
        }
    }

    pub(super) fn rebind(
        &mut self,
        device: &wgpu::Device,
        pipeline: &wgpu::RenderPipeline,
        frame: &wgpu::Buffer,
    ) {
        self.bindings = bindings(device, pipeline, &self.texture, frame);
    }

    pub(super) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &wgpu::RenderPipeline,
        frame: &wgpu::Buffer,
        uploads: &[AtlasUpload<'_>; 2],
    ) {
        let mut resized = false;
        for ((texture, uploaded), upload) in
            self.texture.iter_mut().zip(&mut self.uploaded).zip(uploads)
        {
            let size = [upload.width, upload.height];
            if size != [texture.width(), texture.height()] {
                *texture = atlas::create(device, size, texture.format());
                *uploaded = false;
                resized = true;
            }
            if *uploaded {
                for &rect in upload.dirty {
                    atlas::write(queue, texture, upload, rect);
                }
            } else {
                atlas::write(
                    queue,
                    texture,
                    upload,
                    AtlasRect {
                        origin: [0, 0],
                        size,
                    },
                );
                *uploaded = true;
            }
        }
        if resized {
            self.rebind(device, pipeline, frame);
        }
    }
}

pub(super) fn frame_buffer(device: &wgpu::Device, capacity: u32) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sumie frame uniform and instances"),
        size: FRAME_SIZE + u64::from(capacity) * INSTANCE_SIZE,
        usage: wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::UNIFORM
            | wgpu::BufferUsages::VERTEX,
        mapped_at_creation: false,
    })
}

fn bindings(
    device: &wgpu::Device,
    pipeline: &wgpu::RenderPipeline,
    atlas: &[wgpu::Texture; 2],
    frame: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sumie frame bindings"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: frame,
                    offset: 0,
                    size: NonZeroU64::new(FRAME_SIZE),
                }),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(
                    &atlas[0].create_view(&wgpu::TextureViewDescriptor::default()),
                ),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(
                    &atlas[1].create_view(&wgpu::TextureViewDescriptor::default()),
                ),
            },
        ],
    })
}

pub(super) struct AlphaPipelines {
    pub(super) replace: wgpu::RenderPipeline,
    pub(super) tint: wgpu::RenderPipeline,
}

impl AlphaPipelines {
    pub(super) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        grid: &wgpu::RenderPipeline,
    ) -> Self {
        let module = shader(device);
        let layout = pipeline_layout(device, &grid.get_bind_group_layout(0));
        // A tint changes straight RGB without increasing the existing opacity.
        let tint = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::DstAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::REPLACE,
        };
        Self {
            replace: create_pipeline(
                device,
                format,
                &module,
                &layout,
                None,
                wgpu::ColorWrites::ALL,
            ),
            tint: create_pipeline(
                device,
                format,
                &module,
                &layout,
                Some(tint),
                wgpu::ColorWrites::COLOR,
            ),
        }
    }
}

pub(super) fn pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let texture = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sumie grid bindings layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(FRAME_SIZE),
                },
                count: None,
            },
            texture(1),
            texture(2),
        ],
    });
    create_pipeline(
        device,
        format,
        &shader(device),
        &pipeline_layout(device, &bindings),
        Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        wgpu::ColorWrites::ALL,
    )
}

fn shader(device: &wgpu::Device) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sumie grid shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../grid.wgsl").into()),
    })
}

fn pipeline_layout(
    device: &wgpu::Device,
    bindings: &wgpu::BindGroupLayout,
) -> wgpu::PipelineLayout {
    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("sumie grid pipeline layout"),
        bind_group_layouts: &[Some(bindings)],
        immediate_size: 0,
    })
}

fn create_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    blend: Option<wgpu::BlendState>,
    write_mask: wgpu::ColorWrites,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sumie grid pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vertex"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: INSTANCE_SIZE,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &Instance::ATTRS,
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fragment"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
