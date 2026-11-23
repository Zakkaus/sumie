#[cfg(target_os = "linux")]
mod linux {
    use crate::{FontError, FontOptions, FontSystem, Hinting};

    fn jetbrains_at(logical_px: f32, scale: f32, options: FontOptions) -> Option<FontSystem> {
        const FAMILY: &str = "JetBrains Mono";
        let fonts = FontSystem::new(FAMILY, logical_px, scale, options).unwrap();
        if fonts.family(fonts.primary_face()) != Some(FAMILY) {
            eprintln!("skipped: {FAMILY} is not installed");
            return None;
        }
        Some(fonts)
    }

    fn jetbrains() -> Option<FontSystem> {
        jetbrains_at(20.0, 1.5, FontOptions::default())
    }

    #[test]
    fn device_cell_metrics() {
        let Some(fonts) = jetbrains() else { return };
        let metrics = fonts.metrics();
        assert_eq!(metrics.width, 18);
        assert!(metrics.baseline > 0 && metrics.baseline.unsigned_abs() < metrics.height);
        println!("JetBrains Mono: {metrics:?}");
    }

    #[test]
    fn sprites_fill_their_grid_cells() {
        let mut fonts = FontSystem::new("monospace", 20.0, 1.25, FontOptions::default()).unwrap();
        let m = fonts.metrics();
        let light = ((m.height + 8) / 16).max(1);
        let text = "─│┼█▀\u{e0b0}";
        let run = fonts.shape(text, &[1; 6]).unwrap();
        assert_eq!(run.glyphs.len(), 6);
        for ((column, ch), glyph) in (0..).zip(text.chars()).zip(run.glyphs) {
            let mask = fonts.rasterize(glyph.key).unwrap();
            assert_eq!((mask.rect.width, mask.rect.height), (m.width, m.height));
            assert_eq!(
                (glyph.x + mask.left, glyph.y + mask.top),
                ((column * m.width).cast_signed(), 0)
            );
            let atlas = fonts.atlas();
            for y in 0..m.height {
                for x in 0..m.width {
                    let h = (y.wrapping_sub((m.height - light) / 2)) < light;
                    let v = (x.wrapping_sub((m.width - light) / 2)) < light;
                    let covered = match ch {
                        '─' => h,
                        '│' => v,
                        '┼' => h || v,
                        '█' => true,
                        '▀' => y < m.height.div_ceil(2),
                        _ if x == 0 => true,
                        _ => continue,
                    };
                    let i = (mask.rect.y + y) * atlas.dimensions()[0] + mask.rect.x + x;
                    assert_eq!(
                        atlas.pixels()[i as usize],
                        u8::from(covered) * 255,
                        "{ch} at {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn outlines_produce_coverage() {
        let Some(mut fonts) = jetbrains() else { return };
        let run = fonts.shape("A", &[1]).unwrap();
        let mask = fonts.rasterize(run.glyphs[0].key).unwrap();
        assert!(mask.rect.width > 0 && mask.rect.height > 0);
        assert!(
            fonts
                .atlas()
                .pixels()
                .iter()
                .any(|&pixel| pixel > 0 && pixel < 255)
        );
    }

    #[test]
    fn normal_hinting_saturates_the_stem() {
        let mut saturated = [0; 2];
        for (count, hinting) in saturated.iter_mut().zip([Hinting::Off, Hinting::Normal]) {
            let options = FontOptions {
                hinting,
                ..FontOptions::default()
            };
            let Some(mut fonts) = jetbrains_at(18.33, 1.0, options) else {
                return;
            };
            let key = fonts.shape("l", &[1]).unwrap().glyphs[0].key;
            let rect = fonts.rasterize(key).unwrap().rect;
            let atlas = fonts.atlas();
            let stride = atlas.dimensions()[0] as usize;
            let x = rect.x as usize;
            let width = rect.width as usize;
            *count = atlas
                .pixels()
                .chunks_exact(stride)
                .skip(rect.y as usize)
                .take(rect.height as usize)
                .flat_map(|row| &row[x..x + width])
                .filter(|&&pixel| pixel == 255)
                .count();
        }
        let [off, normal] = saturated;
        println!("opaque pixels: Off={off}, Normal={normal}");
        assert!(normal > off, "opaque pixels: Off={off}, Normal={normal}");
    }

    #[test]
    fn wide_fallback_preserves_baseline_and_punctuation_bearings() {
        let Some(mut fonts) = jetbrains() else { return };
        // Japanese and Taiwanese faces deliberately position the full stop differently.
        fonts.discovery = crate::Discovery::new("Noto Sans CJK JP", None).unwrap();
        let run = match fonts.shape("Aっつ。A", &[1, 2, 2, 2, 1]) {
            Err(FontError::MissingGlyph(_)) => return,
            result => result.unwrap(),
        };
        let [_, small, full, stop, latin] = run.glyphs[..] else {
            panic!("unexpected glyphs: {:?}", run.glyphs);
        };
        let id = small.key.face();
        if fonts.family(id) != Some("Noto Sans CJK JP") {
            eprintln!("skipped: Japanese punctuation fixture is not installed");
            return;
        }
        assert_ne!(id, fonts.primary_face());
        assert_eq!(
            (run.cells, small.cluster, small.column, small.cells),
            (8, 1, 1, 2)
        );
        assert_eq!((latin.cluster, latin.column), (10, 7));
        assert_eq!((full.key.face(), stop.key.face()), (id, id));
        assert_eq!((small.y, stop.y), (full.y, full.y));
        let metrics = fonts.metrics();
        let face = &fonts.faces[id.0];
        if let Some(water) = face.font.charmap().map_unicode('水') {
            let advance = face
                .font
                .glyph_metrics()
                .scaled_f32(face.ppem)
                .h_advance(water);
            let slot = f32::from(u16::try_from(2 * metrics.width).unwrap());
            let max_ppem = crate::MAX_PPEM.min(2.0 * fonts.faces[0].ppem);
            let ic_width = face.font.glyph_metrics().scaled_f32(1.0).h_advance(water);
            let expected = slot.min(ic_width * max_ppem);
            assert!(
                (advance - expected).abs() < 0.001,
                "water advance: {advance}, expected: {expected}"
            );
        }
        let mask = fonts.rasterize(stop.key).unwrap();
        let cell_width = metrics.width.cast_signed();
        let right = stop.x - stop.column.cast_signed() * cell_width
            + mask.left
            + mask.rect.width.cast_signed();
        assert!(right <= cell_width, "full stop extends past slot midpoint");
        assert!(stop.y + mask.top >= metrics.height.cast_signed() / 2);
    }

    #[test]
    fn continuations_keep_the_base_cell() {
        let Some(mut fonts) = jetbrains() else { return };
        let cases = [
            ("a\u{e0001}b", [1, 0, 1], &[(0, 0, 1), (5, 1, 1)][..]),
            ("a\u{fe0f}b", [1, 0, 1], &[(0, 0, 1), (4, 1, 1)][..]),
            ("a\u{200b}b", [1, 0, 1], &[(0, 0, 1), (4, 1, 1)][..]),
            (
                "中\u{0301}a",
                [2, 0, 1],
                &[(0, 0, 2), (0, 0, 2), (5, 2, 1)][..],
            ),
        ];
        for (text, widths, expected) in cases {
            let run = match fonts.shape(text, &widths) {
                Err(FontError::MissingGlyph(0x4e2d)) => {
                    eprintln!("skipped: no CJK fallback font covers U+4E2D");
                    continue;
                }
                result => result.unwrap_or_else(|error| panic!("{text:?}: {error}")),
            };
            assert!(
                run.glyphs
                    .iter()
                    .map(|glyph| (glyph.cluster, glyph.column, glyph.cells))
                    .eq(expected.iter().copied()),
                "{text:?}: {:?}",
                run.glyphs
            );
            if widths[0] == 2 {
                let glyph = run.glyphs[1];
                let mask = fonts.rasterize(glyph.key).unwrap();
                let left = glyph.x + mask.left;
                let right = left + mask.rect.width.cast_signed();
                assert!(
                    0 <= left && left < right && right <= (2 * fonts.metrics().width).cast_signed()
                );
            }
        }
    }

    #[test]
    fn ligature_keeps_two_cells() {
        let Some(mut fonts) = jetbrains() else { return };
        let separate = ["-", ">"].map(|ch| fonts.shape(ch, &[1]).unwrap().glyphs[0].key);
        let arrow = fonts.shape("->", &[1, 1]).unwrap();
        assert_eq!(arrow.cells, 2);
        assert!(!arrow.glyphs.iter().map(|glyph| glyph.key).eq(separate));
        let last = arrow.glyphs.last().unwrap();
        assert_eq!(last.column + last.cells, 2);
        let followed = fonts.shape("->A", &[1, 1, 1]).unwrap();
        let next = followed.glyphs.last().unwrap();
        assert_eq!(
            (next.column, next.x),
            (2, (fonts.metrics().width * 2).cast_signed())
        );
    }
}

#[test]
fn atlas_keeps_rectangles_and_reports_uploads() {
    use skrifa::GlyphId;
    use zeno::Placement;

    use crate::{Atlas, FaceId, GlyphKey, Rect};

    let mut atlas = Atlas::new(4);
    let key = |glyph| GlyphKey::new(FaceId(0), GlyphId::new(glyph), 30.0);
    let small = Placement {
        width: 2,
        height: 2,
        ..Placement::default()
    };
    let first = atlas
        .insert(key(1), small, |pixels, stride| {
            pixels[0] = 255;
            pixels[stride + 1] = 127;
        })
        .unwrap();
    atlas.clear_dirty_regions();
    assert_eq!(atlas.get(key(1)), Some(first));
    assert_eq!(atlas.dirty_regions(), &[]);
    let next = atlas
        .insert(key(2), small, |pixels, _| pixels[0] = 200)
        .unwrap();
    assert_eq!(atlas.dirty_regions(), &[next.rect]);
    let tall = Placement { height: 3, ..small };
    atlas
        .insert(key(3), tall, |pixels, _| pixels[0] = 100)
        .unwrap();
    assert_eq!(atlas.dimensions(), [8, 8]);
    assert_eq!(atlas.get(key(1)), Some(first));
    assert_eq!(
        atlas.dirty_regions(),
        &[Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 8
        }]
    );
    assert_eq!(
        (atlas.pixels()[0], atlas.pixels()[9], atlas.pixels()[16]),
        (255, 127, 100)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn nerd_family_names() {
    use crate::discovery::is_nerd_family;
    for (family, nerd) in [
        ("JetBrainsMono Nerd Font", true),
        ("Maple Mono NF CN", true),
        ("DejaVu Sans", false),
        ("NFoo Sans", false),
    ] {
        assert_eq!(is_nerd_family(family), nerd, "{family}");
    }
}
