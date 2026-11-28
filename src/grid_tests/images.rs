use std::time::{Duration, Instant};

use sumie_core::graphics::{Image, PixelFormat, Placement};
use sumie_render::{DrawList, OffscreenTarget, Renderer, Scroll, wgpu};
use sumie_vt::Terminal;

use super::{CELL, TestImages, assert_pixel, gpu_device, readback};

fn draw_list<'a>(
    images: &'a [Placement],
    source: &'a dyn sumie_core::graphics::ImageSource,
) -> DrawList<'a> {
    DrawList {
        grid_size: [4, 2],
        origin: [0, 0],
        scroll: Scroll::default(),
        cell_size: CELL,
        backgrounds: &[[0; 4]; 8],
        default_background: [0; 4],
        default_backgrounds: &[],
        images,
        image_source: Some(source),
        image_exclusions: [None; 2],
        glyphs: &[],
        cursor: None,
        chrome: None,
    }
}

/// Packed formats expand to the exact RGBA bytes, including rows that are not word-aligned.
pub(super) fn assert_packed_formats(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &OffscreenTarget,
) {
    const RGB: [u8; 18] = [
        255, 0, 0, 0, 255, 0, 0, 0, 255, //
        10, 20, 30, 40, 50, 60, 70, 80, 90,
    ];
    const GRAY: [u8; 3] = [0, 128, 255];
    const GRAY_ALPHA: [u8; 6] = [255, 255, 128, 255, 255, 0];
    let placement = |generation, size: [u32; 2], format, cell: [i32; 2]| Placement {
        image: Image {
            generation,
            size,
            format,
        },
        image_id: u32::try_from(u64::MAX - generation).unwrap(),
        placement_id: 1,
        storage_key: [0; 3],
        z: 0,
        cell,
        offset: [0, 0],
        size,
        source: [0, 0, size[0], size[1]],
        thumbnail: false,
    };
    // Revisions key the texture cache; keep clear of the VT revisions drawn earlier.
    let [rgb, gray, gray_alpha] = [1, 2, 3].map(|n| u64::MAX - n);
    let images = [
        placement(rgb, [3, 2], PixelFormat::Rgb, [0, 0]),
        placement(gray, [3, 1], PixelFormat::Gray, [1, 0]),
        placement(gray_alpha, [3, 1], PixelFormat::GrayAlpha, [2, 0]),
    ];
    let source = TestImages(&[(rgb, &RGB), (gray, &GRAY), (gray_alpha, &GRAY_ALPHA)]);
    renderer.draw(&draw_list(&images, &source), target);
    let pixels = readback(device, queue, target.texture());
    for (x, y, expected) in [
        (0, 0, [255, 0, 0, 255]),
        (1, 0, [0, 255, 0, 255]),
        (2, 0, [0, 0, 255, 255]),
        (0, 1, [10, 20, 30, 255]),
        (1, 1, [40, 50, 60, 255]),
        (2, 1, [70, 80, 90, 255]),
        (8, 0, [0, 0, 0, 255]),
        (9, 0, [128, 128, 128, 255]),
        (10, 0, [255, 255, 255, 255]),
        (16, 0, [255, 255, 255, 255]),
        (17, 0, [128, 128, 128, 255]),
        (18, 0, [0, 0, 0, 0]),
    ] {
        // The sRGB target may round a decoded channel by one.
        assert_pixel(&pixels, x, y, expected, 1);
    }
}

fn base64(data: &[u8]) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [0, 1, 2].map(|i| u32::from(chunk.get(i).copied().unwrap_or(0)));
        let word = bytes[0] << 16 | bytes[1] << 8 | bytes[2];
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= chunk.len() {
                ALPHABET[(word >> shift) as usize & 63]
            } else {
                b'='
            });
        }
    }
    out
}

/// One `mpv --vo=kitty` frame: an anonymous 4K RGB image in 4096-byte base64 chunks.
fn kitty_4k_frame() -> Vec<u8> {
    let pixels: Vec<u8> = (0..3840 * 2160 * 3).map(|i: u32| (i % 251) as u8).collect();
    let payload = base64(&pixels);
    let mut out = Vec::with_capacity(payload.len() + payload.len() / 256);
    for (i, chunk) in payload.chunks(4096).enumerate() {
        let more = u8::from((i + 1) * 4096 < payload.len());
        let control = if i == 0 {
            format!("a=T,f=24,s=3840,v=2160,C=1,q=2,m={more};")
        } else {
            format!("m={more};")
        };
        out.extend_from_slice(b"\x1b_G");
        out.extend_from_slice(control.as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// The main thread hands a 4K RGB frame to the GPU unchanged; it never converts pixels.
#[test]
fn a_4k_kitty_frame_expands_on_the_gpu() {
    const BYTES: u64 = 3840 * 2160 * 3;
    let Some((device, queue)) = gpu_device(sumie_render::backends()) else {
        return;
    };
    let mut renderer = Renderer::new(&device, &queue);
    let target = renderer.create_offscreen([64, 64]);
    let mut vt = Terminal::new(8, 4).unwrap();
    vt.resize_pixels(8, 4, CELL).unwrap();
    let frame = kitty_4k_frame();
    let mut worst = Duration::ZERO;
    // The first frame also builds the pipelines; later frames recycle its texture.
    for frame_number in 0..4 {
        let start = Instant::now();
        // A dispatch batch drains about this much PTY output.
        for batch in frame.chunks(sumie_pty::OUTPUT_SLOTS * 8192) {
            vt.write(batch);
        }
        let snapshot = vt.render_snapshot().unwrap();
        let mut images = snapshot.images.as_ref().clone();
        let placement = images.last_mut().unwrap();
        assert_eq!(placement.image.size, [3840, 2160]);
        placement.size = [32, 18];
        images.drain(..images.len() - 1);
        renderer.draw(&draw_list(&images, &vt), &target);
        if frame_number > 0 {
            worst = worst.max(start.elapsed());
        }
        assert_eq!(renderer.image_expansion().bytes, BYTES * (frame_number + 1));
        let pixels = readback(&device, &queue, target.texture());
        assert_ne!(pixels[..4], [0; 4], "the 4K frame was not drawn");
    }
    // Recorded for comparison only; wall time depends on the host.
    eprintln!("4K RGB frame on the main thread: worst {worst:?}");
    assert_eq!(renderer.image_expansion().scratch_allocations, 1);
    assert_eq!(renderer.image_expansion().scratch_bindings, 1);
}

#[test]
fn packed_images_share_and_regrow_the_scratch() {
    assert_scratch_growth(sumie_render::backends());
}

#[cfg(target_os = "linux")]
#[test]
fn packed_images_share_and_regrow_the_scratch_on_gl() {
    assert_scratch_growth(wgpu::Backends::GL);
}

fn pattern(len: usize, seed: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 7 + seed) % 251).unwrap())
        .collect()
}

fn expected_rgba(format: PixelFormat, source: &[u8]) -> Vec<[u8; 4]> {
    source
        .chunks_exact(format.channels())
        .map(|pixel| match *pixel {
            [gray] => [gray, gray, gray, 255],
            [gray, alpha] => [gray, gray, gray, alpha],
            [r, g, b] => [r, g, b, 255],
            _ => unreachable!(),
        })
        .collect()
}

/// Several packed images in one frame, a multi-row source with a partial tail row, and a
/// scratch texture that must be replaced mid-frame while an earlier image still reads the old one.
fn assert_scratch_growth(backends: wgpu::Backends) {
    let Some((device, queue)) = gpu_device(backends) else {
        return;
    };
    let words_per_row = device.limits().max_texture_dimension_2d as usize;
    // 100 scratch rows plus a tail; the first frame's scratch has far fewer rows.
    let width = 1024;
    let height = (100 * words_per_row * 4 + 1000).div_ceil(width as usize * 3);
    let height = u32::try_from(height).unwrap();
    let size = [width, height + CELL[1]];
    let mut renderer = Renderer::new(&device, &queue);
    let target = renderer.create_offscreen(size);
    let grid = [size[0] / CELL[0], size[1].div_ceil(CELL[1])];
    let backgrounds = vec![[0; 4]; (grid[0] * grid[1]) as usize];
    let small = pattern(3, 1);
    let rgb = pattern(5 * 3 * 3, 2);
    // Opaque except the last texel, so blending leaves the expanded bytes unchanged.
    let mut gray_alpha = pattern(3 * 2 * 2, 3);
    for (i, alpha) in gray_alpha.iter_mut().skip(1).step_by(2).enumerate() {
        *alpha = if i == 5 { 0 } else { 255 };
    }
    let large = pattern((width * height * 3) as usize, 4);
    // Higher z uploads first, so the small images take the old scratch before the large one.
    let frames: [&[Placement]; 2] = [
        &[native(1, [3, 1], PixelFormat::Gray, [0, 0], 1)],
        &[
            native(2, [5, 3], PixelFormat::Rgb, [0, 0], 2),
            native(3, [3, 2], PixelFormat::GrayAlpha, [1, 0], 1),
            native(4, [width, height], PixelFormat::Rgb, [0, 1], 0),
        ],
    ];
    let source = TestImages(&[(1, &small), (2, &rgb), (3, &gray_alpha), (4, &large)]);
    for (allocations, images) in [1, 2].into_iter().zip(frames) {
        renderer.draw(
            &DrawList {
                grid_size: grid,
                origin: [0, 0],
                scroll: Scroll::default(),
                cell_size: CELL,
                backgrounds: &backgrounds,
                default_background: [0; 4],
                default_backgrounds: &[],
                images,
                image_source: Some(&source),
                image_exclusions: [None; 2],
                glyphs: &[],
                cursor: None,
                chrome: None,
            },
            &target,
        );
        assert_eq!(renderer.image_expansion().scratch_allocations, allocations);
    }
    let pixels = readback(&device, &queue, target.texture());
    for (origin, image_size, format, source) in [
        ([0, 0], [5, 3], PixelFormat::Rgb, &rgb),
        ([CELL[0], 0], [3, 2], PixelFormat::GrayAlpha, &gray_alpha),
        ([0, CELL[1]], [width, height], PixelFormat::Rgb, &large),
    ] {
        assert_expanded(&pixels, size[0], origin, image_size[0], format, source);
    }
}

fn native(
    generation: u64,
    size: [u32; 2],
    format: PixelFormat,
    cell: [i32; 2],
    z: i32,
) -> Placement {
    Placement {
        image: Image {
            generation,
            size,
            format,
        },
        image_id: u32::try_from(generation).unwrap(),
        placement_id: 1,
        storage_key: [0; 3],
        z,
        cell,
        offset: [0, 0],
        size,
        source: [0, 0, size[0], size[1]],
        thumbnail: false,
    }
}

fn assert_expanded(
    pixels: &[u8],
    stride: u32,
    origin: [u32; 2],
    width: u32,
    format: PixelFormat,
    source: &[u8],
) {
    for (i, expected) in expected_rgba(format, source).into_iter().enumerate() {
        let i = u32::try_from(i).unwrap();
        let [x, y] = [i % width, i / width];
        let at = (((origin[1] + y) * stride + origin[0] + x) * 4) as usize;
        let actual = &pixels[at..at + 4];
        // Transparent texels stay as the cleared background.
        let expected = if expected[3] == 0 { [0; 4] } else { expected };
        assert!(
            actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
            "{format:?} pixel ({x}, {y}): expected {expected:?} ±1, got {actual:?}"
        );
    }
}
