use sumie_core::{
    graphics::{Image, ImageSource, PixelFormat, Placement},
    splits::Rect,
};
use sumie_render::{
    AtlasUpload, ChromeLayer, DrawList, Fill, FrameDraw, GlyphQuad, ImageViewer, OffscreenTarget,
    PaneDraw, PixelRect, Renderer, Rgba, backends, wgpu,
};

use super::{FULL, HEIGHT, TestImages, WIDTH, assert_pixel, gpu_device, readback, upload_atlas};

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

const SOURCE: TestImages<'static> = TestImages(&[
    (1, [YELLOW, RED, CYAN].as_flattened()),
    (2, [YELLOW, BLUE, CYAN].as_flattened()),
    (3, [YELLOW, WHITE, CYAN].as_flattened()),
    (4, [YELLOW, BLACK, CYAN].as_flattened()),
    (5, [YELLOW, RED, CYAN].as_flattened()),
    (6, [YELLOW, GREEN, CYAN].as_flattened()),
]);

#[test]
fn gpu_split_pixels() {
    check(backends());
}

#[test]
fn gpu_split_pixels_gl() {
    check(wgpu::Backends::GL);
}

struct CapImages;

impl ImageSource for CapImages {
    fn pixels(&self, placement: &Placement) -> Option<&[u8]> {
        match placement.image.generation {
            1..=4096 => Some([YELLOW, BLUE, CYAN].as_flattened()),
            4097 => Some([YELLOW, RED, CYAN].as_flattened()),
            4098 => Some([YELLOW, GREEN, CYAN].as_flattened()),
            _ => None,
        }
    }
}

#[test]
fn gpu_viewers_precede_terminal_images_at_texture_cap() {
    let Some((device, queue)) = gpu_device(backends()) else {
        return;
    };
    let mut renderer = Renderer::new(&device, &queue);
    let target = renderer.create_offscreen([WIDTH, HEIGHT]);
    let terminal: Vec<_> = (1..=4096)
        .map(|id| {
            let x = u32::try_from((id - 1).min(2)).unwrap();
            image(id, [x, 0], [1, 1], 0)
        })
        .collect();
    let viewers = [
        image(4097, [0, 0], [1, 1], 0),
        image(4098, [0, 0], [1, 1], 0),
    ];
    let mut panes = [0, 8, 16].map(|x| PaneDraw {
        rect: Rect {
            x,
            y: 0,
            width: 8,
            height: 8,
        },
        draw: grid([i32::try_from(x).unwrap(), 0], [8, 8], &[BLACK], BLACK, &[]),
        viewer: None,
        thumbnails: None,
    });
    for pane in &mut panes {
        pane.draw.image_source = Some(&CapImages);
    }
    panes[0].draw.images = &terminal;
    for open in [true, true, false] {
        for (pane, placement) in panes[1..].iter_mut().zip(&viewers) {
            pane.viewer = open.then(|| ImageViewer {
                placement,
                dest: rect([i32::try_from(pane.rect.x).unwrap(), 0], [8, 8]),
                backdrop: BLACK,
            });
        }
        renderer.draw_frame(
            &FrameDraw {
                background: BLACK,
                panes: panes.into_iter(),
                fills: &[],
                chrome: None,
            },
            &target,
        );
        let pixels = readback(&device, &queue, target.texture());
        // Both viewers displace the two backmost ordinary placements.
        assert_pixel(&pixels, 0, 0, if open { BLACK } else { BLUE }, 0);
        assert_pixel(&pixels, 1, 0, if open { BLACK } else { BLUE }, 0);
        assert_pixel(&pixels, 2, 0, BLUE, 0);
        assert_pixel(&pixels, 8, 0, if open { RED } else { BLACK }, 0);
        assert_pixel(&pixels, 16, 0, if open { GREEN } else { BLACK }, 0);
    }
}

fn check(backend: wgpu::Backends) {
    let Some((device, queue)) = gpu_device(backend) else {
        return;
    };
    let mut renderer = Renderer::new(&device, &queue);
    renderer.set_minimum_contrast(1.0);
    upload_atlas(&mut renderer);
    let target = renderer.create_offscreen([WIDTH, HEIGHT]);

    let mut left_images = [image(1, [1, 1], [30, 22], 0)];
    left_images[0].thumbnail = true;
    // Both panes use protocol image ID 1, but each image revision has a unique generation.
    let right_images = [image(2, [1, 1], [17, 25], -1), image(3, [4, 0], [3, 3], 0)];
    let right = PaneDraw {
        rect: Rect {
            x: 17,
            y: 5,
            width: 16,
            height: 25,
        },
        thumbnails: None,
        viewer: None,
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
        thumbnails: Some(sumie_ui::Palette::for_luminance(0.0).thumbnails(150, None)),
        viewer: None,
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
                chrome: Some(TEST_STRIP),
            },
            &target,
        );
        assert_split(&readback(&device, &queue, target.texture()));
    }
    assert_viewer(&mut renderer, &device, &queue, &target, &mut [right, left]);
    assert_thumbnail_chrome(&mut renderer, &device, &queue, &target);
}

const TEST_STRIP: ChromeLayer<'static> = ChromeLayer {
    fills: &[fill([0, 0], [WIDTH, 4], WHITE)],
    glyphs: &[],
    text_background: BLACK,
    atlas: AtlasUpload {
        width: 1,
        height: 1,
        bytes: &[0],
        dirty: &[],
    },
};

fn assert_viewer(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &OffscreenTarget,
    panes: &mut [PaneDraw<'_>; 2],
) {
    panes[1].viewer = Some(ImageViewer {
        placement: &panes[1].draw.images[0],
        dest: rect([12, 24], [8, 8]),
        backdrop: [0, 0, 0, 128],
    });
    renderer.draw_frame(
        &FrameDraw {
            background: CLEAR,
            panes: panes.iter().copied(),
            fills: &[],
            chrome: None,
        },
        target,
    );
    let pixels = readback(device, queue, target.texture());
    assert_pixel(&pixels, 4, 8, [187, 0, 0, 255], 1);
    assert_pixel(&pixels, 12, 24, RED, 0); // No thumbnail border on the expanded image.
    assert_pixel(&pixels, 15, 29, RED, 0); // Cropped source covers the glyph in the pane gutter.
    assert_pixel(&pixels, 16, 29, CLEAR, 0);
    assert_pixel(&pixels, 21, 8, BLUE, 0);
    assert_pixel(&pixels, 15, 30, CLEAR, 0);
}

fn assert_thumbnail_chrome(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &OffscreenTarget,
) {
    let style = sumie_ui::Palette::for_luminance(0.0).thumbnails(150, Some(1));
    let images = [
        Placement {
            thumbnail: true,
            cell: [-1, 0],
            ..image(4, [WIDTH - 1, 1], [9, 17], 0)
        },
        Placement {
            thumbnail: true,
            ..image(5, [9, 1], [25, 24], 0)
        },
        image(6, [9, 1], [4, 3], 1),
    ];
    let pane = PaneDraw {
        rect: Rect {
            x: 0,
            y: 0,
            width: WIDTH,
            height: HEIGHT,
        },
        draw: DrawList {
            image_exclusions: [Some(rect([33, 17], [1, 1])), None],
            ..grid([0, 0], [WIDTH, HEIGHT], &[BLACK], BLACK, &images)
        },
        thumbnails: Some(style),
        viewer: None,
    };
    renderer.draw_frame(
        &FrameDraw {
            background: CLEAR,
            panes: std::iter::once(pane),
            fills: &[],
            chrome: None,
        },
        target,
    );
    let pixels = readback(device, queue, target.texture());
    for (x, y, color) in [
        (0, 4, BLACK), // Clipping the natural left edge must not create a new border.
        (7, 4, [83, 84, 85, 255]), // Dark Astryx rule blended over black in linear light.
        (9, 4, style.accent),
        (10, 4, RED),          // Still one device pixel wide at 125% scale.
        (9, 1, GREEN),         // Higher-z, unmarked image covers the hover border.
        (33, 17, BLACK),       // Exclusion also clips the affordance.
        (13, 4, style.accent), // Chip corner and both arrowheads.
        (28, 8, style.on_accent),
        (17, 19, style.on_accent),
    ] {
        assert_pixel(&pixels, x, y, color, 1);
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
        image_source: Some(&SOURCE),
        image_exclusions: [None; 2],
        glyphs: &[],
        cursor: None,
        chrome: None,
    }
}

fn image(generation: u64, offset: [u32; 2], size: [u32; 2], z: i32) -> Placement {
    Placement {
        image: Image {
            generation,
            size: [3, 1],
            format: PixelFormat::Rgba,
        },
        image_id: 1,
        placement_id: 1,
        storage_key: [0; 3],
        z,
        cell: [0, 0],
        offset,
        size,
        source: [1, 0, 1, 1],
        thumbnail: false,
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
