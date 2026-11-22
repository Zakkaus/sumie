#[cfg(target_os = "linux")]
mod linux {
    use crate::{FontError, FontSystem};

    fn jetbrains() -> Option<FontSystem> {
        const FAMILY: &str = "JetBrains Mono";
        let fonts = FontSystem::new(FAMILY, 20.0, 1.5).unwrap();
        if fonts.family(fonts.primary_face()) != Some(FAMILY) {
            eprintln!("skipped: {FAMILY} is not installed");
            return None;
        }
        Some(fonts)
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
            assert!(
                (advance - slot).abs() < 0.001,
                "water advance: {advance}, slot: {slot}"
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
    assert!(atlas.dirty_regions().is_empty());
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
