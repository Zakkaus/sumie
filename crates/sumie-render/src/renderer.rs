use std::{num::NonZeroU64, ops::Range};

use wgpu::util::StagingBelt;

use crate::grid::{GridLayers, Instance};
use crate::images::{Images, Layers};
use crate::{AtlasRect, AtlasUpload, DrawList, atlas};

const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const FRAME_SIZE: u64 = size_of::<Frame>() as u64;
const INSTANCE_SIZE: u64 = size_of::<Instance>() as u64;
const INITIAL_INSTANCES: u32 = 256;

#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct Frame {
    size: [u32; 2],
    minimum_contrast: f32,
    padding: u32,
}

struct LayeredFrame {
    grid: GridLayers,
    images: Layers,
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
    frame: wgpu::Buffer,
    frame_limit: u32,
    minimum_contrast: f32,
    pipeline: wgpu::RenderPipeline,
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
        let grid_atlas = BoundAtlas::new(device, &pipeline, &frame, [1, 1]);
        Self {
            grid_atlas,
            chrome_atlas: None,
            images: Images::new(format),
            staging: StagingBelt::new(device.clone(), frame.size()),
            frame,
            frame_limit,
            minimum_contrast: 1.1,
            pipeline,
            device: device.clone(),
            queue: queue.clone(),
        }
    }

    /// Set the minimum WCAG contrast ratio for text in subsequent draws.
    /// Defaults to `1.1`; `1.0` disables correction.
    pub fn set_minimum_contrast(&mut self, minimum_contrast: f32) {
        self.minimum_contrast = minimum_contrast;
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

    /// Queue atlas transfers for the next draw. Do not hold a terminal lock.
    /// The first upload and any change in dimensions use the full atlas.
    pub fn upload_atlas(&mut self, upload: &AtlasUpload<'_>) {
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

    pub(crate) fn draw_view(
        &mut self,
        list: &DrawList<'_>,
        view: &wgpu::TextureView,
        size: [u32; 2],
        clear: wgpu::Color,
    ) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sumie frame"),
            });
        let mut grid = list.grid_instances();
        let chrome = list.chrome.as_ref().map_or(0, |layer| {
            let size = [layer.atlas.width, layer.atlas.height];
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
        });
        let layered = if list.images.is_empty() {
            None
        } else {
            let available = self
                .frame_limit
                .saturating_sub(grid.saturating_add(chrome).saturating_add(1));
            self.images
                .prepare(&self.device, &self.queue, list, size, available)
                .map(|images| LayeredFrame {
                    grid: list.image_layers(images.base, grid),
                    images,
                })
        };
        if let Some(layers) = &layered {
            grid = layers.grid.end;
            self.images
                .stage_exclusions(&mut self.staging, &mut encoder, list.image_exclusions);
        }
        let count = grid.checked_add(chrome).expect("too many frame instances");
        if count != 0 {
            self.stage_instances(&mut encoder, list, size, count, layered.as_ref());
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sumie grid"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if let Some(layers) = &layered {
                self.draw_layered(&mut pass, list, layers, count);
            } else if count != 0 {
                self.draw_grid(&mut pass, 0..grid);
            }
            if chrome != 0 {
                if layered.is_some() {
                    pass.set_pipeline(&self.pipeline);
                }
                let atlas = self.chrome_atlas.as_ref().expect("uploaded chrome atlas");
                pass.set_bind_group(0, &atlas.bindings, &[]);
                let offset = FRAME_SIZE + u64::from(grid) * INSTANCE_SIZE;
                pass.set_vertex_buffer(0, self.frame.slice(offset..));
                pass.draw(0..4, 0..chrome);
            }
        }
        self.staging.finish_and_recall_on_submit(&encoder);
        self.queue.submit([encoder.finish()]);
    }

    fn draw_grid(&self, pass: &mut wgpu::RenderPass<'_>, instances: Range<u32>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.grid_atlas.bindings, &[]);
        pass.set_vertex_buffer(0, self.frame.slice(FRAME_SIZE..));
        pass.draw(0..4, instances);
    }

    fn draw_layered(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        list: &DrawList<'_>,
        layers: &LayeredFrame,
        count: u32,
    ) {
        let grid = &layers.grid;
        let images = &layers.images;
        let vertices = self
            .frame
            .slice(FRAME_SIZE + u64::from(count) * INSTANCE_SIZE..);
        let base = u32::from(grid.base.is_some());
        if base != 0 {
            self.draw_grid(pass, 0..base);
            self.images
                .draw(pass, list.images, 0..images.below_background, vertices);
        }
        if base != grid.backgrounds {
            self.draw_grid(pass, base..grid.backgrounds);
        }
        self.images.draw(
            pass,
            list.images,
            images.below_background..images.below_text,
            vertices,
        );
        if grid.backgrounds != grid.text {
            self.draw_grid(pass, grid.backgrounds..grid.text);
        }
        self.images
            .draw(pass, list.images, images.below_text..images.end, vertices);
        if grid.text != grid.end {
            self.draw_grid(pass, grid.text..grid.end);
        }
    }

    fn stage_instances(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        list: &DrawList<'_>,
        [width, height]: [u32; 2],
        count: u32,
        layered: Option<&LayeredFrame>,
    ) {
        let total = count + layered.map_or(0, |layers| layers.images.end);
        let bytes = FRAME_SIZE + u64::from(total) * INSTANCE_SIZE;
        if bytes > self.frame.size() {
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
        let mut staging =
            self.staging
                .write_buffer(encoder, &self.frame, 0, NonZeroU64::new(bytes).unwrap());
        let (mut header, instances) = staging.slice(..).split_at(size_of::<Frame>());
        header.copy_from_slice(bytemuck::bytes_of(&Frame {
            size: [width, height],
            minimum_contrast: self.minimum_contrast,
            padding: 0,
        }));
        if let Some(layers) = layered {
            let (grid, images) = instances.split_at(count as usize * size_of::<Instance>());
            list.write_layered(grid, &layers.grid);
            self.images.write_instances(images);
        } else {
            list.write_instances(instances);
        }
    }
}

struct BoundAtlas {
    texture: wgpu::Texture,
    bindings: wgpu::BindGroup,
    uploaded: bool,
}

impl BoundAtlas {
    fn new(
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

    fn rebind(
        &mut self,
        device: &wgpu::Device,
        pipeline: &wgpu::RenderPipeline,
        frame: &wgpu::Buffer,
    ) {
        self.bindings = bindings(device, pipeline, &self.texture, frame);
    }

    fn upload(
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

fn frame_buffer(device: &wgpu::Device, capacity: u32) -> wgpu::Buffer {
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

fn pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sumie grid shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("grid.wgsl").into()),
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
