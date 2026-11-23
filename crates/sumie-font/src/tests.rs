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
    fn fallback_ink_is_centered_in_two_cells() {
        let Some(mut fonts) = jetbrains() else { return };
        let run = match fonts.shape("A中A", &[1, 2, 1]) {
            Err(FontError::MissingGlyph(0x4e2d)) => {
                eprintln!("skipped: no CJK fallback font covers U+4E2D");
                return;
            }
            result => result.unwrap(),
        };
        let glyph = run.glyphs[1];
        assert_ne!(glyph.key.face(), fonts.primary_face());
        assert_eq!(
            (run.cells, glyph.cluster, glyph.column, glyph.cells),
            (4, 1, 1, 2)
        );
        assert_eq!((run.glyphs[2].cluster, run.glyphs[2].column), (4, 3));
        let mask = fonts.rasterize(glyph.key).unwrap();
        let metrics = fonts.metrics();
        let face = &fonts.faces[glyph.key.face().0];
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
        let left = glyph.x + mask.left - metrics.width.cast_signed();
        let right = (2 * metrics.width).cast_signed() - left - mask.rect.width.cast_signed();
        let top = glyph.y + mask.top;
        let bottom = metrics.height.cast_signed() - top - mask.rect.height.cast_signed();
        assert!(
            left >= 0 && right >= 0 && left.abs_diff(right) <= 1,
            "horizontal padding: {left}, {right}"
        );
        assert!(
            top >= 0 && bottom >= 0 && top.abs_diff(bottom) <= 1,
            "vertical padding: {top}, {bottom}"
        );
        println!(
            "{} at {} ppem: {} × {} mask in a {} × {} slot, padding [{left}, {right}, {top}, {bottom}]",
            fonts.family(glyph.key.face()).unwrap(),
            glyph.key.ppem(),
            mask.rect.width,
            mask.rect.height,
            2 * metrics.width,
            metrics.height
        );
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
