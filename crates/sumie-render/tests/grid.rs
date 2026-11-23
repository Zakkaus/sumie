#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use sumie_render::{
    AtlasRect, AtlasUpload, Cursor, DrawList, GlyphQuad, PixelRect, Renderer, Rgba, backends,
    instance_descriptor, wgpu,
};

const WIDTH: u32 = 36;
const HEIGHT: u32 = 34;
const CELL: [u32; 2] = [8, 16];
const FULL: AtlasRect = AtlasRect {
    origin: [1, 1],
    size: CELL,
};
const HALF: AtlasRect = AtlasRect {
    origin: [10, 1],
    size: CELL,
};

#[test]
fn grid_on_selected_backend() {
    let artifact_dir = std::env::var_os("SUMIE_RENDER_ARTIFACT_DIR").map(PathBuf::from);
    snapshot(backends(), artifact_dir.as_deref());
}

#[cfg(target_os = "linux")]
#[test]
fn grid_on_gl() {
    snapshot(wgpu::Backends::GL, None);
}

fn snapshot(backends: wgpu::Backends, artifact_dir: Option<&Path>) {
    let instance = wgpu::Instance::new(instance_descriptor(
        wgpu::InstanceDescriptor::new_without_display_handle(),
        backends,
    ));
    let adapter =
        match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        {
            Ok(adapter) => adapter,
            Err(err) => {
                let reason = format!("no adapter for {backends:?}: {err}");
                assert_ne!(
                    std::env::var("SUMIE_REQUIRE_GPU").as_deref(),
                    Ok("1"),
                    "{reason}"
                );
                eprintln!("Skipping grid snapshot: {reason}");
                return;
            }
        };
    let info = adapter.get_info();
    eprintln!(
        "Grid snapshot: {:?}, {} ({:?}; {})",
        info.backend, info.name, info.device_type, info.driver_info
    );
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits:
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .expect("requesting a downlevel device");

    let mut renderer = Renderer::new(&device, &queue);
    let target = renderer.create_offscreen([WIDTH, HEIGHT]);
    upload_atlas(&mut renderer);
    for (setting, grays) in [
        (None, [70, 183]),
        (Some(1.0), [64, 192]),
        (Some(2.0), [109, 134]),
    ] {
        if let Some(ratio) = setting {
            renderer.set_minimum_contrast(ratio);
        }
        renderer.draw(&GRID, &target);
        let pixels = readback(&device, &queue, target.texture());
        assert_grid(&pixels, grays);
        if let (None, Some(dir)) = (setting, artifact_dir) {
            let file = std::fs::File::create(dir.join("grid.png")).unwrap();
            let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), WIDTH, HEIGHT);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&pixels).unwrap();
            writer.finish().unwrap();
        }
    }
}

fn upload_atlas(renderer: &mut Renderer) {
    const STRIDE: usize = 19;
    const HEIGHT: u32 = 18;
    renderer.upload_atlas(&AtlasUpload {
        width: 8,
        height: 16,
        bytes: &[0; 128],
        dirty: &[],
    });
    let mut bytes = [0; STRIDE * HEIGHT as usize];
    for row in &mut bytes.as_chunks_mut::<STRIDE>().0[1..17] {
        row[1..9].fill(255);
    }
    // The full glyph must arrive on growth despite an empty dirty list.
    renderer.upload_atlas(&AtlasUpload {
        width: 19,
        height: HEIGHT,
        bytes: &bytes,
        dirty: &[],
    });
    for row in &mut bytes.as_chunks_mut::<STRIDE>().0[1..17] {
        row[1..9].fill(0);
        row[10..18].fill(128);
    }
    // Poison the full glyph's source bytes to catch an unexpected whole-atlas upload.
    renderer.upload_atlas(&AtlasUpload {
        width: 19,
        height: HEIGHT,
        bytes: &bytes,
        dirty: &[HALF],
    });
}

const fn glyph(col: u32, x: i32, atlas: AtlasRect, color: Rgba) -> GlyphQuad {
    GlyphQuad {
        atlas,
        dest: PixelRect {
            origin: [x, 1],
            size: CELL,
        },
        color,
        cell: [col, 0],
    }
}

const GRID: DrawList<'static> = DrawList {
    grid_size: [4, 2],
    origin: [2, 1],
    cell_size: CELL,
    backgrounds: &[
        [12, 25, 40, 255],
        [16, 60, 128, 255],
        [64, 64, 64, 255],
        [192, 192, 192, 255],
        [65, 112, 94, 255],
        [71, 97, 31, 255],
        [22, 18, 40, 255],
        [236, 231, 242, 255],
    ],
    glyphs: &[
        glyph(0, 2, FULL, [245, 220, 115, 255]),
        glyph(1, 10, HALF, [240, 80, 40, 255]),
        glyph(2, 18, FULL, [64, 64, 64, 255]),
        glyph(3, 26, FULL, [192, 192, 192, 255]),
    ],
    cursor: Some(Cursor {
        rect: PixelRect {
            origin: [26, 17],
            size: [8, 2],
        },
        color: [191, 46, 200, 255],
    }),
};

fn assert_grid(pixels: &[u8], [lightened, darkened]: [u8; 2]) {
    let samples = [
        ([1, 1], [0, 0, 0, 0], 0),
        ([2, 1], [245, 220, 115, 255], 1),
        ([9, 16], [245, 220, 115, 255], 1),
        // 128/255 coverage darkens to 0.562 before the luminance correction.
        ([10, 16], [168, 70, 100, 255], 2),
        ([18, 1], [lightened, lightened, lightened, 255], 1),
        ([33, 16], [darkened, darkened, darkened, 255], 1),
        ([34, 16], [0, 0, 0, 0], 0),
        ([2, 17], [65, 112, 94, 255], 1),
        ([26, 17], [191, 46, 200, 255], 1),
        ([33, 18], [191, 46, 200, 255], 1),
        ([33, 19], [236, 231, 242, 255], 1),
    ];
    for ([x, y], expected, tolerance) in samples {
        let offset = ((y * WIDTH + x) * 4) as usize;
        let actual = &pixels[offset..offset + 4];
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(a, b)| a.abs_diff(b) <= tolerance),
            "pixel ({x}, {y}): expected {expected:?} ±{tolerance}, got {actual:?}"
        );
    }
}

fn readback(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    const ROW: u32 = WIDTH * 4;
    const STRIDE: u32 =
        ROW.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("grid readback"),
        size: u64::from(STRIDE * HEIGHT),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(STRIDE),
                rows_per_image: None,
            },
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
        .expect("waiting for grid readback");
    rx.try_recv()
        .expect("mapping callback")
        .expect("mapped readback buffer");
    let view = buffer.get_mapped_range(..).unwrap();
    let mut pixels = Vec::with_capacity((ROW * HEIGHT) as usize);
    for row in view.as_chunks::<{ STRIDE as usize }>().0 {
        pixels.extend_from_slice(&row[..ROW as usize]);
    }
    pixels
}
