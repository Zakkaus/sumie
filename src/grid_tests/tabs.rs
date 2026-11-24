use sumie_render::{Renderer, Rgba, wgpu};
use sumie_ui::TabHit;
use sumie_vt::Terminal;

use crate::view::{FontSettings, View};

/// Read actual chrome pixels at fractional scale, including a hover before and after present.
pub(super) fn assert_tab_bar(renderer: &mut Renderer, device: &wgpu::Device, queue: &wgpu::Queue) {
    const DEVICE: [u32; 2] = [600, 160];
    const LONG_TITLE: &str = "a long title that will not fit into its tab";
    let config = sumie_config::Config::default();
    let font = FontSettings::from_config(config.font).unwrap();
    let mut view = View::new(150, [480, 128], DEVICE, font, &config.colors, [0; 2]).unwrap();
    let target = renderer.create_offscreen(DEVICE);
    let size = view.size();
    let mut vt = Terminal::new(size.cols, size.rows).unwrap();
    let mut close = None;
    let mut new = None;
    for (count, light, hover, title) in [
        (1, false, None, LONG_TITLE),
        (2, false, None, LONG_TITLE),
        (2, true, None, LONG_TITLE),
        (2, true, Some(TabHit::New), LONG_TITLE),
        (2, false, Some(TabHit::Close(0)), LONG_TITLE),
        (2, false, Some(TabHit::Close(0)), ""),
        (1, true, None, LONG_TITLE),
    ] {
        view.set_tab_count(count).unwrap();
        let shrinking = title.is_empty();
        if !shrinking {
            view.set_tab_pointer(match hover {
                Some(TabHit::Close(0)) => close,
                Some(TabHit::New) => new,
                None => None,
                _ => unreachable!(),
            });
        }
        let size = view.size();
        vt.resize(size.cols, size.rows).unwrap();
        let bg = if light { [236, 231, 228] } else { [27, 24, 34] };
        let mut ansi = [[0; 3]; 16];
        ansi[3] = [255, 198, 0];
        vt.set_colors([100; 3], bg, None, &ansi).unwrap();
        let snapshot = vt.render_snapshot().unwrap();
        let labels = [(title, true), ("shell", true)];
        view.build(&snapshot, None, &[], None, true);
        view.draw_tabs(labels.into_iter().take(count), count - 1, &snapshot);
        if hover == Some(TabHit::Close(0)) {
            assert_eq!(
                view.tab_hit(close.unwrap()),
                Some(if shrinking {
                    TabHit::Close(0)
                } else {
                    TabHit::Tab(0)
                }),
                "clicks still use the presented strip"
            );
        }
        renderer.upload_atlas(&view.atlas());
        view.atlas_uploaded();
        renderer.draw(&view.draw_list(true), &target);
        view.frame_presented();
        let frame = super::readback(device, queue, target.texture());
        let pixel = |x: u32, y: u32| -> Rgba {
            let at = (y * DEVICE[0] + x) as usize * 4;
            frame[at..at + 4].try_into().unwrap()
        };
        let top = DEVICE[1] - u32::from(size.height_px);
        assert_eq!(top > 0, count > 1, "only multiple tabs displace the grid");
        let background = [bg[0], bg[1], bg[2], 255];
        assert_eq!(pixel(590, top + 10), background);
        assert_eq!(view.tab_hit((475.0, 16.0)), None, "empty strip after +");
        if count > 1 {
            let hits = presented_buttons(&view, !shrinking && hover == Some(TabHit::Close(0)));
            close = Some(hits[0]);
            new = Some(hits[1]);
            assert_colors(pixel, top, background, light);
            assert_grid_origin(&view, &snapshot, top);
        }
    }
}

fn presented_buttons(view: &View, close_visible: bool) -> [(f64, f64); 2] {
    let positions = || (0..480).map(|x| (f64::from(x), 16.0));
    let close = positions()
        .find(|&p| view.tab_hover_hit(p) == Some(TabHit::Close(0)))
        .unwrap();
    let expected = if close_visible {
        TabHit::Close(0)
    } else {
        TabHit::Tab(0)
    };
    assert_eq!(
        view.tab_hit(close),
        Some(expected),
        "close visibility follows the pointer after layout"
    );
    let new = positions()
        .find(|&p| view.tab_hit(p) == Some(TabHit::New))
        .unwrap();
    [close, new]
}

fn assert_grid_origin(view: &View, snapshot: &sumie_vt::Snapshot, top: u32) {
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

fn assert_colors(pixel: impl Fn(u32, u32) -> Rgba, top: u32, background: Rgba, light: bool) {
    assert_ne!(
        pixel(590, 5),
        background,
        "chrome follows the terminal theme"
    );
    assert_ne!(
        pixel(590, top),
        pixel(590, top + 10),
        "shadow below the bar"
    );
    let has_yellow = (0..top).any(|y| (0..600).any(|x| pixel(x, y) == [255, 198, 0, 255]));
    assert_eq!(has_yellow, !light, "low-contrast yellow yields to Astryx");
}
