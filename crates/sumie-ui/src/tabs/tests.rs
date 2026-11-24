use super::*;

#[test]
fn scaled_tabs_share_edges_with_hit_regions() {
    for (scale, body, edge, plus, close_x, close_y, side) in [
        (120, 34, 98, 444, 74, 9, 16),
        (150, 43, 123, 556, 93, 11, 20),
        (180, 51, 147, 666, 111, 13, 24),
        (240, 68, 196, 888, 148, 18, 32),
    ] {
        let tabs = [32, 96, 512].map(|title| TabMeasurement {
            number: scaled(8, scale),
            title: scaled(title, scale),
            activity: false,
        });
        let layout = TabLayout::new([1000, 120], scale, 1, &tabs, scaled(8, scale)).unwrap();
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
            ([980.0, 0.0], None),
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
fn tabs_shrink_evenly_until_short_content_reaches_its_minimum() {
    let tabs = [10, 60, 80].map(|title| TabMeasurement {
        number: 8,
        title,
        activity: false,
    });
    let layout = TabLayout::new([345, 100], 120, 0, &tabs, 10).unwrap();
    assert_eq!(layout.tab(0).size[0], 70);
    assert_eq!(layout.tab(1).size[0], 106);
    assert_eq!(layout.tab(2).size[0], 125);
    assert_eq!(layout.hit([307.0, 0.0]), Some(TabHit::New));
}

#[test]
fn overflow_keeps_active_and_plus_in_view() {
    let tabs = [TabMeasurement {
        number: 8,
        title: 120,
        activity: false,
    }; 16];
    assert_eq!(tab_bar_height(1, 120, 100), 0);
    let layout = TabLayout::new([250, 100], 120, 12, &tabs, 8).unwrap();
    assert_eq!(layout.visible(), 11..14);
    assert_eq!(layout.hit([75.0, 0.0]), Some(TabHit::Tab(12)));
    assert_eq!(layout.hit([212.0, 0.0]), Some(TabHit::New));

    let clipped = TabLayout::new([31, 2], 120, 12, &tabs, 8).unwrap();
    assert!(clipped.visible().is_empty());
    assert_eq!(clipped.hit([0.0, 0.0]), Some(TabHit::New));
    assert_eq!(clipped.hit([0.0, 1.0]), None);
}

#[test]
fn underline_and_divider_follow_content_not_close_controls() {
    let mut tabs = [TabMeasurement {
        number: 8,
        title: 32,
        activity: false,
    }; 4];
    tabs[0].activity = true;
    let layout = TabLayout::new([500, 100], 120, 1, &tabs, 8).unwrap();
    assert_eq!(layout.number(0), rect(31, 0, 8, 34));
    assert_eq!(layout.title(0), rect(47, 0, 32, 34));

    let palette = TabPalette::new([18, 39, 56, 255], [255; 4], [255, 198, 0, 255]);
    let mut fills = Vec::new();
    layout.fills(palette, None, &mut fills);
    let mut dividers = fills.iter().filter(|fill| fill.rect.origin[1] == 10);
    assert_eq!(dividers.next().unwrap().rect, rect(295, 10, 1, 14));
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

    layout.underline(palette, 1, 171, 20, &mut fills);
    assert_eq!(
        fills.last(),
        Some(&Fill {
            rect: rect(123, 21, 48, 2),
            color: palette.active_number,
        })
    );
}
