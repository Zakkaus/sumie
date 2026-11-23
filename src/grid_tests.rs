use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use sumie_render::{
    AtlasRect, AtlasUpload, Cursor, CursorShape, DrawList, GlyphQuad, OffscreenTarget, PixelRect,
    Renderer, Rgba, backends, instance_descriptor, wgpu,
};

#[cfg(target_os = "linux")]
mod attributes;

#[cfg(target_os = "linux")]
#[test]
fn search_keeps_terminal_dimensions() {
    use crate::view::{FontSettings, View};

    let config = sumie_config::Config::default();
    let font = FontSettings::from_config(config.font).unwrap();
    let mut view = View::new(150, [640, 320], [800, 400], font, &config.colors).unwrap();
    let dimensions = |view: &View| {
        let size = view.size();
        [size.cols, size.rows, size.width_px, size.height_px]
    };
    let original = dimensions(&view);
    for bar in [Some(String::new()), None] {
        view.search_bar = bar;
        assert_eq!(dimensions(&view), original);
    }
}

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
    #[cfg(target_os = "linux")]
    attributes::assert_decorations(&mut renderer, &device, &queue);
    assert_cursor_shapes(&mut renderer, &device, &queue, &target);
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
        GlyphQuad {
            dest: PixelRect {
                origin: [18, 17],
                size: CELL,
            },
            cell: [2, 1],
            ..glyph(2, 18, FULL, [250, 80, 40, 128])
        },
        GlyphQuad {
            atlas: AtlasRect {
                size: [4, 16],
                ..FULL
            },
            dest: PixelRect {
                origin: [26, 17],
                size: [4, 16],
            },
            cell: [3, 1],
            ..glyph(3, 26, FULL, [250, 80, 40, 255])
        },
    ],
    cursor: Some(Cursor {
        rect: PixelRect {
            origin: [18, 17],
            size: [16, 16],
        },
        color: [255; 4],
        shape: CursorShape::Block {
            cell: [2, 1],
            columns: 2,
        },
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
        // Cursor glyphs use the cell's background RGB but keep the glyph alpha.
        ([18, 17], [188, 188, 189, 255], 1),
        ([26, 17], [236, 231, 242, 255], 1),
        ([33, 18], [255; 4], 1),
    ];
    for ([x, y], expected, tolerance) in samples {
        assert_pixel(pixels, x, y, expected, tolerance);
    }
}

fn assert_cursor_shapes(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &OffscreenTarget,
) {
    const SIZE: [u32; 2] = [8, 24];
    const BACKGROUND: Rgba = [0, 0, 0, 255];
    const INK: Rgba = [255; 4];
    for shape in [
        CursorShape::Bar,
        CursorShape::Underline,
        CursorShape::Hollow,
    ] {
        renderer.draw(
            &DrawList {
                grid_size: [1, 1],
                cell_size: SIZE,
                origin: [2, 1],
                backgrounds: &[BACKGROUND],
                glyphs: &[],
                cursor: Some(Cursor {
                    rect: PixelRect {
                        origin: [2, 1],
                        size: SIZE,
                    },
                    shape,
                    color: INK,
                }),
            },
            target,
        );
        let pixels = readback(device, queue, target.texture());
        for y in 0..24 {
            for x in 0..8 {
                let marked = match shape {
                    CursorShape::Bar => x < 2,
                    CursorShape::Underline => y >= 22,
                    CursorShape::Hollow => x == 0 || x == 7 || y == 0 || y == 23,
                    CursorShape::Block { .. } | CursorShape::Solid => unreachable!(),
                };
                assert_pixel(
                    &pixels,
                    x + 2,
                    y + 1,
                    if marked { INK } else { BACKGROUND },
                    0,
                );
            }
        }
    }
}

fn assert_pixel(pixels: &[u8], x: u32, y: u32, expected: Rgba, tolerance: u8) {
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

fn readback(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let row_bytes = texture.width() * 4;
    let stride =
        row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("grid readback"),
        size: u64::from(stride) * u64::from(texture.height()),
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
                bytes_per_row: Some(stride),
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
    let mut pixels = Vec::with_capacity((row_bytes * texture.height()) as usize);
    for row in view.chunks_exact(stride as usize) {
        pixels.extend_from_slice(&row[..row_bytes as usize]);
    }
    pixels
}
