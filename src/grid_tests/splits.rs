use std::sync::Arc;

use sumie_core::{
    graphics::{Image, Placement},
    splits::Rect,
};
use sumie_render::{
    AtlasUpload, ChromeLayer, DrawList, Fill, FrameDraw, GlyphQuad, PaneDraw, PixelRect, Renderer,
    Rgba, backends, wgpu,
};

use super::{FULL, HEIGHT, WIDTH, assert_pixel, gpu_device, readback, upload_atlas};

const BLACK: Rgba = [0, 0, 0, 255];
const WHITE: Rgba = [255; 4];
const RED: Rgba = [255, 0, 0, 255];
const GREEN: Rgba = [0, 255, 0, 255];
const BLUE: Rgba = [0, 0, 255, 255];
const YELLOW: Rgba = [255, 255, 0, 255];
const CYAN: Rgba = [0, 255, 255, 255];
const DIVIDER: Rgba = [90, 70, 50, 255];
const LEFT_BASE: Rgba = [16, 32, 48, 255];
const CLEAR: Rgba = [2, 4, 6, 255];

#[test]
fn gpu_split_pixels() {
    check(backends());
}

#[test]
fn gpu_split_pixels_gl() {
    check(wgpu::Backends::GL);
}

fn check(backend: wgpu::Backends) {
    let Some((device, queue)) = gpu_device(backend) else {
        return;
    };
    let mut renderer = Renderer::new(&device, &queue);
    renderer.set_minimum_contrast(1.0);
    upload_atlas(&mut renderer);
    let target = renderer.create_offscreen([WIDTH, HEIGHT]);

    let left_images = [image(1, RED, [1, 1], [30, 22], 0)];
    // Both panes use protocol image ID 1, but each image revision has a unique generation.
    let right_images = [
        image(2, BLUE, [1, 1], [17, 25], -1),
        image(3, WHITE, [4, 0], [3, 3], 0),
    ];
    let right = PaneDraw {
        rect: Rect {
            x: 17,
            y: 5,
            width: 16,
            height: 25,
        },
        draw: DrawList {
            image_exclusions: [None, Some(rect([22, 10], [2, 3]))],
            glyphs: &[GlyphQuad {
                atlas: FULL,
                dest: rect([28, 26], FULL.size),
                color: CYAN,
                cell: [0, 0],
            }],
            ..grid([20, 5], [9, 20], &[WHITE], BLACK, &right_images)
        },
    };
    let left = PaneDraw {
        rect: Rect {
            x: 2,
            y: 5,
            width: 14,
            height: 25,
        },
        draw: DrawList {
            image_exclusions: [Some(rect([5, 10], [2, 3])), None],
            glyphs: &[GlyphQuad {
                atlas: FULL,
                dest: rect([12, 26], FULL.size),
                color: YELLOW,
                cell: [0, 0],
            }],
            ..grid([2, 5], [16, 20], &[GREEN], LEFT_BASE, &left_images)
        },
    };
    let fills = [
        fill([16, 5], [1, 25], DIVIDER),
        fill([17, 5], [16, 25], [0, 0, 0, 128]),
    ];
    for panes in [[right, left], [left, right]] {
        renderer.draw_frame(
            &FrameDraw {
                background: CLEAR,
                panes: panes.into_iter(),
                fills: &fills,
                chrome: Some(ChromeLayer {
                    fills: &[fill([0, 0], [WIDTH, 4], WHITE)],
                    glyphs: &[],
                    text_background: BLACK,
                    atlas: AtlasUpload {
                        width: 1,
                        height: 1,
                        bytes: &[0],
                        dirty: &[],
                    },
                }),
            },
            &target,
        );
        assert_split(&readback(&device, &queue, target.texture()));
    }
}

fn assert_split(pixels: &[u8]) {
    for (x, y, color, tolerance) in [
        (1, 0, WHITE, 0), // Chrome across the full window, not the last pane's scissor.
        (1, 6, CLEAR, 0),
        (4, 8, RED, 0),
        (5, 10, GREEN, 0), // Separate pane-local image exclusions.
        (15, 20, RED, 0),
        (16, 20, DIVIDER, 0),
        (17, 20, BLACK, 0), // Overflowing left grid must not paint the right gutter.
        (21, 8, [0, 0, 187, 255], 1), // Right image visibly dimmed in linear light.
        (22, 10, [187, 187, 187, 255], 1),
        (24, 6, [187, 187, 187, 255], 1), // Foreground image in front of the blue one.
        (29, 8, BLACK, 0), // Oversized image stops at the grid, even inside its leaf.
        (4, 27, LEFT_BASE, 0),
        (15, 29, YELLOW, 0), // Glyph may overhang into its own gutter.
        (17, 29, BLACK, 0),
        (32, 29, [0, 187, 187, 255], 1),
        (33, 29, CLEAR, 0),
        (15, 30, CLEAR, 0), // Glyph must also be clipped vertically.
    ] {
        assert_pixel(pixels, x, y, color, tolerance);
    }
}

fn grid<'a>(
    origin: [i32; 2],
    cell_size: [u32; 2],
    backgrounds: &'a [Rgba],
    default_background: Rgba,
    images: &'a [Placement],
) -> DrawList<'a> {
    DrawList {
        grid_size: [1, 1],
        cell_size,
        origin,
        backgrounds,
        default_background,
        default_backgrounds: &[],
        images,
        image_exclusions: [None; 2],
        glyphs: &[],
        cursor: None,
        chrome: None,
    }
}

fn image(generation: u64, color: Rgba, offset: [u32; 2], size: [u32; 2], z: i32) -> Placement {
    Placement {
        image: Arc::new(Image {
            generation,
            size: [1, 1],
            rgba: color.to_vec(),
        }),
        image_id: 1,
        placement_id: 1,
        z,
        cell: [0, 0],
        offset,
        size,
        source: [0, 0, 1, 1],
    }
}

const fn rect(origin: [i32; 2], size: [u32; 2]) -> PixelRect {
    PixelRect { origin, size }
}

const fn fill(origin: [i32; 2], size: [u32; 2], color: Rgba) -> Fill {
    Fill {
        rect: rect(origin, size),
        color,
    }
}
