use crate::atlas;

/// A reusable linear-light composition attachment and its compositor encoding pass.
pub(super) struct Presentation {
    pipeline: wgpu::RenderPipeline,
    target: Option<Target>,
    format: wgpu::TextureFormat,
}

struct Target {
    size: [u32; 2],
    view: wgpu::TextureView,
    bindings: wgpu::BindGroup,
}

impl Presentation {
    pub(super) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        mode: wgpu::CompositeAlphaMode,
    ) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sumie presentation shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("presentation.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sumie presentation pipeline"),
            layout: None,
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
                entry_point: Some(match mode {
                    wgpu::CompositeAlphaMode::PreMultiplied => "premultiplied",
                    wgpu::CompositeAlphaMode::PostMultiplied => "straight",
                    _ => unreachable!("not a transparent surface alpha mode"),
                }),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            target: None,
            format,
        }
    }

    pub(super) fn resize(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        if self
            .target
            .as_ref()
            .is_some_and(|target| target.size == size)
        {
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sumie translucent composition"),
            size: atlas::extent(size),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sumie presentation bindings"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        self.target = Some(Target {
            size,
            view,
            bindings,
        });
    }

    pub(super) fn view(&self) -> &wgpu::TextureView {
        &self.target.as_ref().expect("sized composition target").view
    }

    pub(super) fn encode(&self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("sumie surface encoding"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
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
        pass.set_bind_group(
            0,
            &self
                .target
                .as_ref()
                .expect("sized composition target")
                .bindings,
            &[],
        );
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::mpsc;
    use std::time::Duration;

    use sumie_core::graphics::{Image, ImageSource, PixelFormat, Placement};
    use sumie_core::splits::Rect;
    use wgpu::CompositeAlphaMode::{PostMultiplied, PreMultiplied};

    use super::super::OFFSCREEN_FORMAT;
    use crate::images::Images;
    use crate::{DrawList, Fill, FrameDraw, PaneDraw, PixelRect, Renderer, Rgba};

    #[test]
    fn translucent_default_background_and_opaque_explicit_background() {
        const DEFAULT: Rgba = [64, 128, 192, 128];
        const EXPLICIT: Rgba = [0, 255, 0, 255];

        let Some((device, queue)) = test_device() else {
            return;
        };
        let mut renderer = Renderer::new(&device, &queue);
        let target = renderer.create_offscreen([4, 1]);
        let pane = PaneDraw {
            rect: Rect {
                x: 0,
                y: 0,
                width: 3,
                height: 1,
            },
            draw: DrawList {
                grid_size: [2, 1],
                cell_size: [1, 1],
                origin: [0, 0],
                backgrounds: &[DEFAULT, EXPLICIT],
                default_background: DEFAULT,
                default_backgrounds: &[],
                images: &[],
                image_source: None,
                image_exclusions: [None; 2],
                glyphs: &[],
                cursor: None,
                chrome: None,
            },
            viewer: None,
            thumbnails: None,
        };
        let frame = FrameDraw {
            background: [16, 32, 48, 128],
            panes: std::iter::once(pane),
            fills: &[Fill {
                rect: PixelRect {
                    origin: [3, 0],
                    size: [1, 1],
                },
                color: [255; 4],
            }],
            chrome: None,
        };
        for (mode, bg, gutter) in [
            (PreMultiplied, [32, 64, 96, 128], [128; 4]),
            (PostMultiplied, DEFAULT, [255, 255, 255, 128]),
        ] {
            renderer.set_surface_alpha(Some(mode));
            renderer.draw_frame(&frame, &target);
            let pixels = read_row(&device, &queue, target.texture());
            for (pixel, want) in pixels
                .as_chunks::<4>()
                .0
                .iter()
                .zip([bg, EXPLICIT, bg, gutter])
            {
                assert_eq!(pixel[3], want[3], "{mode:?}: {pixel:?} != {want:?}");
                assert!(
                    pixel[..3]
                        .iter()
                        .zip(want)
                        .all(|(&a, b)| a.abs_diff(b) <= 1),
                    "{mode:?}: {pixel:?} != {want:?}"
                );
            }
        }
        // At this coverage, sRGB8 retains nonzero white ink but the alpha channel rounds to zero.
        renderer.set_surface_alpha(Some(PreMultiplied));
        renderer.draw_view(
            &pane.draw,
            &target.view,
            [4, 1],
            wgpu::Color {
                a: 0.001,
                ..wgpu::Color::WHITE
            },
        );
        assert_eq!(read_row(&device, &queue, target.texture())[12..], [0; 4]);
    }

    #[test]
    fn opaque_cover_culls_only_after_texture_admission() {
        let Some((device, queue)) = test_device() else {
            return;
        };
        let mut renderer = Renderer::new(&device, &queue);
        renderer.images = Images::new(OFFSCREEN_FORMAT, 128 * 1024);
        let target = renderer.create_offscreen([4, 1]);
        let placement = |image_id, [w, h]: [u32; 2], x, z| Placement {
            image: Image {
                generation: u64::from(image_id),
                size: [w, h],
                format: PixelFormat::Rgb,
            },
            image_id,
            placement_id: 1,
            storage_key: [0; 3],
            z,
            cell: [0, 0],
            offset: [x, 0],
            size: [1, 1],
            source: [0, 0, w, h],
            thumbnail: false,
        };
        // The right-hand image leaves 64 KiB. The larger cover cannot fit there but
        // the 1×1 image under it can.
        let images = [
            placement(1, [1, 1], 0, 0),
            placement(2, [257, 127], 0, 0),
            placement(3, [1, 1], 1, 1),
        ];
        let source = CoverPixels::default();
        let draw = DrawList {
            grid_size: [4, 1],
            cell_size: [1, 1],
            origin: [0, 0],
            backgrounds: &[[0, 0, 0, 255]; 4],
            default_background: [0, 0, 0, 255],
            default_backgrounds: &[],
            images: &images,
            image_source: Some(&source),
            image_exclusions: [None; 2],
            glyphs: &[],
            cursor: None,
            chrome: None,
        };
        renderer.draw(&draw, &target);
        assert_eq!(
            &read_row(&device, &queue, target.texture())[..8],
            &[255, 0, 0, 255, 0, 255, 0, 255]
        );
        assert_eq!(source.cover_reads.get(), 0);

        renderer.images = Images::new(OFFSCREEN_FORMAT, 256 * 1024);
        renderer.draw(&draw, &target);
        assert_eq!(
            &read_row(&device, &queue, target.texture())[..4],
            &[0, 0, 255, 255]
        );
        assert_eq!(source.cover_reads.get(), 1);
        assert_eq!(renderer.image_texture_count(), 2);
    }

    #[derive(Default)]
    struct CoverPixels {
        cover_reads: Cell<usize>,
    }

    impl ImageSource for CoverPixels {
        fn pixels(&self, placement: &Placement) -> Option<&[u8]> {
            static BLUE: [[u8; 3]; 257 * 127] = [[0, 0, 255]; 257 * 127];
            match placement.image_id {
                1 => Some(&[255, 0, 0]),
                2 => {
                    self.cover_reads.set(self.cover_reads.get() + 1);
                    Some(BLUE.as_flattened())
                }
                3 => Some(&[0, 255, 0]),
                _ => None,
            }
        }
    }

    fn test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(crate::instance_descriptor(
            wgpu::InstanceDescriptor::new_without_display_handle(),
            crate::backends(),
        ));
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()));
        let Ok(adapter) = adapter else {
            assert_ne!(std::env::var("SUMIE_REQUIRE_GPU").as_deref(), Ok("1"));
            return None;
        };
        Some(
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                required_limits: wgpu::Limits::downlevel_webgl2_defaults(),
                ..Default::default()
            }))
            .unwrap(),
        )
    }

    fn read_row(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> [u8; 16] {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sumie opacity probe readback"),
            size: 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout::default(),
            },
            texture.size(),
        );
        let (tx, rx) = mpsc::sync_channel(1);
        encoder.map_buffer_on_submit(&buffer, wgpu::MapMode::Read, .., move |result| {
            tx.send(result).unwrap();
        });
        let submission = queue.submit([encoder.finish()]);
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(10)),
            })
            .unwrap();
        rx.try_recv().unwrap().unwrap();
        let view = buffer.get_mapped_range(..).unwrap();
        let mut pixels = [0; 16];
        pixels.copy_from_slice(&view);
        pixels
    }
}
