use std::num::NonZeroU64;

use crate::{AtlasRect, AtlasUpload, atlas, grid::Instance};

use super::{FRAME_SIZE, INSTANCE_SIZE};

pub(super) struct BoundAtlas {
    texture: wgpu::Texture,
    pub(super) bindings: wgpu::BindGroup,
    uploaded: bool,
}

impl BoundAtlas {
    pub(super) fn new(
        device: &wgpu::Device,
        pipeline: &wgpu::RenderPipeline,
        frame: &wgpu::Buffer,
        size: [u32; 2],
    ) -> Self {
        let texture = atlas::create(device, size);
        Self {
            bindings: bindings(device, pipeline, &texture, frame),
            texture,
            uploaded: false,
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
        upload: &AtlasUpload<'_>,
    ) {
        let size = [upload.width, upload.height];
        if size != [self.texture.width(), self.texture.height()] {
            *self = Self::new(device, pipeline, frame, size);
        }
        if self.uploaded {
            for &rect in upload.dirty {
                atlas::write(queue, &self.texture, upload, rect);
            }
        } else {
            atlas::write(
                queue,
                &self.texture,
                upload,
                AtlasRect {
                    origin: [0, 0],
                    size,
                },
            );
            self.uploaded = true;
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
    atlas: &wgpu::Texture,
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
                    &atlas.create_view(&wgpu::TextureViewDescriptor::default()),
                ),
            },
        ],
    })
}

pub(super) fn pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sumie grid shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../grid.wgsl").into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sumie grid pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
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
