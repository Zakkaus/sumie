use super::*;

#[test]
fn scaled_tabs_share_edges_with_hit_regions() {
    for (scale, body, edge, plus, close_x, close_y, side) in [
        (120, 34, 196, 576, 168, 9, 16),
        (150, 43, 246, 722, 211, 11, 20),
        (180, 51, 294, 864, 252, 13, 24),
        (240, 68, 392, 1152, 336, 18, 32),
    ] {
        let tabs = [32, 96, 512].map(|title| TabMeasurement {
            number: scaled(8, scale),
            title: scaled(title, scale),
            activity: false,
            progress: None,
        });
        let layout = TabLayout::new([1400, 120], scale, 1, &tabs).unwrap();
        assert_eq!(layout.visible(), 0..3);
        let [body, edge, plus, close_x, close_y, side] =
            [body, edge, plus, close_x, close_y, side].map(f64::from);
        for (point, expected) in [
            ([close_x - 0.01, close_y], Some(TabHit::Tab(0))),
            ([close_x, close_y - 0.01], Some(TabHit::Tab(0))),
            ([close_x, close_y], Some(TabHit::Close(0))),
            (
                [close_x + side - 0.01, close_y + side - 0.01],
                Some(TabHit::Close(0)),
            ),
            ([close_x + side, close_y], Some(TabHit::Tab(0))),
            ([edge - 0.01, 0.0], Some(TabHit::Tab(0))),
            ([edge, 0.0], Some(TabHit::Tab(1))),
            ([plus - 0.01, 0.0], Some(TabHit::Tab(2))),
            ([plus, body - 0.01], Some(TabHit::New)),
            ([plus, body], None),
            ([1350.0, 0.0], None),
        ] {
            assert_eq!(
                layout.hit(point),
                expected,
                "scale {scale}, point {point:?}"
            );
        }
    }
}

#[test]
fn tabs_shrink_evenly_regardless_of_title_width() {
    let tabs = [10, 60, 80].map(|title| TabMeasurement {
        number: 8,
        title,
        activity: false,
        progress: None,
    });
    let layout = TabLayout::new([495, 100], 120, 0, &tabs).unwrap();
    assert_eq!([0, 1, 2].map(|i| layout.tab(i).size[0]), [151, 150, 150]);
    assert_eq!(layout.hit([457.0, 0.0]), Some(TabHit::New));
}

#[test]
fn overflow_keeps_active_and_plus_in_view() {
    let tabs = [TabMeasurement {
        number: 17,
        title: 240,
        activity: false,
        progress: None,
    }; 20];
    assert_eq!(tab_bar_height(1, 120, 100), 0);
    let layout = TabLayout::new([1400, 100], 150, 19, &tabs).unwrap();
    assert_eq!(layout.visible(), 15..20);
    assert!(layout.visible().all(|i| layout.title(i).size[0] >= 140));
    assert_eq!(layout.hit([1100.0, 0.0]), Some(TabHit::Tab(19)));
    assert_eq!(layout.hit([1198.0, 0.0]), Some(TabHit::New));

    let clipped = TabLayout::new([31, 2], 120, 19, &tabs).unwrap();
    assert!(clipped.visible().is_empty());
    assert_eq!(clipped.hit([0.0, 0.0]), Some(TabHit::New));
    assert_eq!(clipped.hit([0.0, 1.0]), None);
}

#[test]
fn underline_spans_content_and_dividers_align_with_edges() {
    let mut tabs = [TabMeasurement {
        number: 8,
        title: 32,
        activity: false,
        progress: None,
    }; 4];
    tabs[0].activity = true;
    let layout = TabLayout::new([500, 100], 120, 1, &tabs).unwrap();
    assert_eq!(layout.number(0), rect(31, 0, 8, 34));
    assert_eq!(layout.title(0), rect(47, 0, 37, 34));

    let palette = TabPalette::new(
        [18, 39, 56, 255],
        [255; 4],
        [255, 198, 0, 255],
        [255, 70, 80, 255],
    );
    let mut fills = Vec::new();
    layout.fills(palette, None, &mut fills);
    let mut dividers = fills.iter().filter(|fill| fill.rect.origin[1] == 10);
    assert_eq!(dividers.next().unwrap().rect, rect(348, 10, 1, 14));
    assert!(dividers.next().is_none());
    assert!(fills.contains(&Fill {
        rect: rect(0, 34, 500, 1),
        color: black(89),
    }));
    assert!(fills.contains(&Fill {
        rect: rect(0, 39, 500, 1),
        color: black(7),
    }));

    layout.activity_dot(palette, 0, &mut fills);
    assert!(fills.contains(&Fill {
        rect: rect(18, 15, 5, 3),
        color: palette.active_number,
    }));

    assert!(fills.contains(&Fill {
        rect: rect(132, 30, 90, 2),
        color: palette.active_number,
    }));
}

#[test]
fn progress_fills_a_track_along_the_top_edge_in_the_state_color() {
    use sumie_core::notify::{Progress, ProgressState::*};

    let palette = TabPalette::new(
        [0, 0, 0, 255],
        [255, 255, 255, 255],
        [255, 255, 0, 255],
        [255, 0, 0, 255],
    );
    let bars = |progress| {
        let tabs = [progress, None].map(|progress| TabMeasurement {
            number: 8,
            title: 96,
            activity: false,
            progress,
        });
        let layout = TabLayout::new([500, 80], 120, 1, &tabs).unwrap();
        let content = layout.content(0);
        let mut out = Vec::new();
        layout.progress_bars(palette, &mut out);
        (content, out)
    };
    assert_eq!(bars(None).1.len(), 0);
    let report = |state, percent| Some(Progress { state, percent });
    // (report, widths and colors of the fills after the track)
    let width = bars(report(Paused, None)).0.size[0];
    for (progress, expected) in [
        (
            report(Set, Some(50)),
            vec![(width / 2, palette.active_number)],
        ),
        (report(Set, None), vec![]),
        (report(Paused, None), vec![(width, palette.label)]),
        (report(Error, Some(100)), vec![(width, palette.error)]),
        (
            report(Indeterminate, None),
            (0..width.div_ceil(16))
                .map(|dash| ((width - 16 * dash).min(8), palette.active_number))
                .collect(),
        ),
    ] {
        let (content, out) = bars(progress);
        let track = out[0].rect;
        assert_eq!(
            (track.origin[1], track.size),
            (0, [content.size[0], 2]),
            "{progress:?}"
        );
        let fills: Vec<_> = out[1..]
            .iter()
            .map(|fill| (fill.rect.size[0], fill.color))
            .collect();
        assert_eq!(fills, expected, "{progress:?}");
    }
}
