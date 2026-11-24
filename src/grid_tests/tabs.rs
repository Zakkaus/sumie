use std::ops::Range;

use sumie_render::{Renderer, Rgba, wgpu};
use sumie_ui::{Palette, TabHit};
use sumie_vt::Terminal;

use crate::view::{FontSettings, View};

/// Read actual chrome pixels at fractional scale, including a hover before and after present.
pub(super) fn assert_tab_bar(renderer: &mut Renderer, device: &wgpu::Device, queue: &wgpu::Queue) {
    const DEVICE: [u32; 2] = [320, 160];
    const CLOSE: (f64, f64) = (94.0, 16.0);
    const NEW: (f64, f64) = (238.0, 16.0);
    let config = sumie_config::Config::default();
    let font = FontSettings::from_config(config.font).unwrap();
    let mut view = View::new(150, [256, 128], DEVICE, font, &config.colors).unwrap();
    let target = renderer.create_offscreen(DEVICE);
    let size = view.size();
    let mut vt = Terminal::new(size.cols, size.rows).unwrap();
    for (count, light, hover) in [
        (1, false, None),
        (2, false, None),
        (2, false, Some(TabHit::Close(0))),
        (2, true, Some(TabHit::New)),
        (1, true, None),
    ] {
        view.set_tab_count(count).unwrap();
        view.set_tab_hover(hover);
        let size = view.size();
        vt.resize(size.cols, size.rows).unwrap();
        let bg = if light { [236, 231, 228] } else { [27, 24, 34] };
        vt.set_colors([100; 3], bg, None, &[[0; 3]; 16]).unwrap();
        let snapshot = vt.render_snapshot().unwrap();
        let labels = [
            ("a long title that will not fit into its tab", true),
            ("shell", true),
        ];
        view.build(&snapshot, None, &[], None, true);
        view.draw_tabs(
            labels.into_iter().take(count),
            count - 1,
            snapshot.background,
        );
        if hover == Some(TabHit::Close(0)) {
            assert_eq!(view.tab_hover_hit(CLOSE), hover);
            assert_eq!(
                view.tab_hit(CLOSE),
                Some(TabHit::Tab(0)),
                "not yet presented"
            );
        }
        renderer.upload_atlas(&view.atlas());
        view.atlas_uploaded();
        renderer.draw(&view.draw_list(true), &target);
        view.frame_presented();
        if hover == Some(TabHit::Close(0)) {
            assert_eq!(view.tab_hit(CLOSE), hover);
        }
        let frame = super::readback(device, queue, target.texture());
        let pixel = |x: u32, y: u32| -> Rgba {
            let at = (y * DEVICE[0] + x) as usize * 4;
            frame[at..at + 4].try_into().unwrap()
        };
        let top = DEVICE[1] - u32::from(size.height_px);
        assert_eq!(top > 0, count > 1, "only multiple tabs displace the grid");
        assert_eq!(pixel(0, top), [bg[0], bg[1], bg[2], 255]);
        assert_eq!(view.tab_hit(NEW), (count > 1).then_some(TabHit::New));
        if count > 1 {
            let palette = Palette::for_luminance(if light { 1.0 } else { 0.0 });
            assert_chrome(pixel, top, palette, hover);
            let logical_top = f64::from(top) / 1.25;
            assert!(
                view.pointer_position((0.0, logical_top), view.content_rect())
                    .1[1]
                    .abs()
                    < f32::EPSILON
            );
            assert_eq!(
                view.ime_rectangle(snapshot.cursor, view.content_rect())
                    .unwrap()
                    .y,
                top.cast_signed() * 120 / 150
            );
        }
    }
}

fn assert_chrome(
    pixel: impl Fn(u32, u32) -> Rgba,
    top: u32,
    palette: Palette,
    hover: Option<TabHit>,
) {
    // At 1.25× the fixed 45px plus leaves 275px, split at x = 137.
    let accent = pixel(137, top - 2);
    assert_eq!(pixel(136, top - 2), palette.surface);
    assert_ne!(accent, palette.surface, "selected rail");
    assert_eq!(pixel(274, top - 2), accent, "full-width selected rail");
    assert_ne!(accent, pixel(137, top - 1), "rule below selected rail");
    assert!(
        (25..48).any(|x| pixel(x, 20) == accent),
        "inactive activity dot"
    );
    assert!(
        !(165..192).any(|x| pixel(x, 20) == accent),
        "active tab has no dot"
    );

    let ink = |xs: Range<u32>, background| {
        (10..30).any(|y| xs.clone().any(|x| pixel(x, y) != background))
    };
    let inactive = pixel(5, 20);
    let active = pixel(140, 20);
    let plus = pixel(280, 20);
    assert_eq!(inactive != palette.surface, hover == Some(TabHit::Close(0)));
    assert_ne!(active, palette.surface, "raised tab fill");
    assert_eq!(plus != palette.surface, hover == Some(TabHit::New));
    assert!(
        ink(15..25, inactive) && ink(49..102, inactive),
        "number and title ink"
    );
    assert!(ink(166..240, active), "selected title ink");
    assert_eq!(
        ink(109..127, inactive),
        hover == Some(TabHit::Close(0)),
        "close hidden until hover"
    );
    assert!(ink(247..265, active), "selected close ink");
    assert!(ink(289..305, plus), "plus ink");
}
