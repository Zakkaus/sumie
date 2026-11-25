use std::time::{Duration, Instant};

use sumie_core::graphics::{Image, PixelFormat, Placement};
use sumie_render::{DrawList, OffscreenTarget, Renderer, Scroll, wgpu};
use sumie_vt::Terminal;

use super::{CELL, TestImages, assert_pixel, gpu_device, readback};

/// The design book's main-thread stall bound.
const STALL_BUDGET: Duration = Duration::from_millis(100);

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

#[test]
fn a_4k_kitty_frame_stays_within_the_stall_budget() {
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
        let elapsed = start.elapsed();
        if frame_number > 0 {
            worst = worst.max(elapsed);
        }
        let pixels = readback(&device, &queue, target.texture());
        assert_ne!(pixels[..4], [0; 4], "the 4K frame was not drawn");
    }
    eprintln!("4K RGB frame on the main thread: worst {worst:?}");
    assert!(
        worst < STALL_BUDGET,
        "a 4K RGB frame took {worst:?} on the main thread"
    );
}
