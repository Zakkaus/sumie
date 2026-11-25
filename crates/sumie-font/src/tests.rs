#[cfg(target_os = "linux")]
mod linux {
    use crate::{FontError, FontOptions, FontStyle, FontSystem, Hinting, TextRun};

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
    fn proportional_cutoff_measures_advance_not_character_count() {
        let mut fonts = FontSystem::new("sans-serif", 12.5, 1.25, FontOptions::default()).unwrap();
        let mut narrow = TextRun::default();
        let mut wide = TextRun::default();
        let mut ellipsis = TextRun::default();
        fonts
            .shape_text_into(["i"; 6].into_iter(), &mut narrow)
            .unwrap();
        fonts
            .shape_text_into(["W"; 6].into_iter(), &mut wide)
            .unwrap();
        fonts
            .shape_text_into(std::iter::once("…"), &mut ellipsis)
            .unwrap();

        let maximum = narrow.width() + ellipsis.width();
        assert!(wide.width() > maximum);
        assert!(narrow.fit(maximum, &ellipsis).1.is_none());
        let (glyphs, suffix, advance) = wide.fit(maximum, &ellipsis);
        assert!(!glyphs.is_empty() && glyphs.len() < 6);
        assert!(suffix.is_some() && advance <= maximum);
    }

    #[test]
    fn title_spaces_advance_even_without_ink() {
        let mut font = FontSystem::new("sans-serif", 12.5, 1.25, FontOptions::default()).unwrap();
        let face = &font.faces[0];
        let space = face.font.charmap().map_unicode(' ').unwrap();
        let advance = face
            .font
            .glyph_metrics()
            .scaled_f32(face.ppem)
            .h_advance(space);
        let mut run = TextRun::default();
        font.shape_text_into(["g", "i", "t", "l", "o", "g"].into_iter(), &mut run)
            .unwrap();
        let joined = run.glyphs.last().unwrap().x;
        for count in [1_u16, 16] {
            let title = ["g", "i", "t"]
                .into_iter()
                .chain(std::iter::repeat_n(" ", usize::from(count)))
                .chain(["l", "o", "g"]);
            font.shape_text_into(title, &mut run).unwrap();
            let shift = f64::from(run.glyphs.last().unwrap().x - joined);
            assert!((shift - f64::from(advance) * f64::from(count)).abs() < 1.0);
        }
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
    fn light_hinting_fits_vertical_stems() {
        let mut saturated = [0; 2];
        for (count, hinting) in saturated.iter_mut().zip([Hinting::Off, Hinting::Light]) {
            let options = FontOptions {
                hinting,
                ..FontOptions::default()
            };
            let Some(mut fonts) = jetbrains_at(18.33, 1.0, options) else {
                return;
            };
            let key = fonts.shape("1", &[1]).unwrap().glyphs[0].key;
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
        let [off, light] = saturated;
        assert!(light > off, "opaque pixels: Off={off}, Light={light}");
    }

    #[test]
    fn common_ideographs_share_the_fallback_face_size() {
        const FAMILY: &str = "JetBrainsMono Nerd Font";
        // Light hinting is the terminal default; unhinted masks do not reproduce the overflow.
        let options = FontOptions {
            hinting: Hinting::Light,
            weight: Some(500),
        };
        let mut fonts = FontSystem::new(FAMILY, 11.0 * 96.0 / 72.0, 1.25, options).unwrap();
        if fonts.family(fonts.primary_face()) != Some(FAMILY) {
            return;
        }
        let run = match fonts.shape("中文測試終端游標", &[2; 8]) {
            Err(FontError::MissingGlyph { .. }) => return,
            result => result.unwrap(),
        };
        let first = run.glyphs[0].key;
        if fonts.family(first.face()) != Some("Noto Sans CJK TC") {
            eprintln!("skipped: Noto Sans CJK TC is not the selected fallback");
            return;
        }
        let face_ppem = fonts.faces[first.face().0].ppem.to_bits();
        assert_eq!(face_ppem, fonts.faces[0].ppem.to_bits());
        assert_eq!(run.glyphs.len(), 8);
        for glyph in run.glyphs {
            assert_eq!(glyph.key.face(), first.face());
            assert_eq!(glyph.key.ppem_bits, face_ppem, "{glyph:?}");
        }
    }

    #[test]
    fn wide_fallback_preserves_baseline_and_punctuation_bearings() {
        let Some(mut fonts) = jetbrains() else { return };
        // Japanese and Taiwanese faces deliberately position the full stop differently.
        fonts.discovery = crate::Discovery::new("Noto Sans CJK JP", None).unwrap();
        let run = match fonts.shape("Aっつ。A", &[1, 2, 2, 2, 1]) {
            Err(FontError::MissingGlyph { .. }) => return,
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
        let mask = fonts.rasterize(stop.key).unwrap();
        let cell_width = metrics.width.cast_signed();
        let right = stop.x - stop.column.cast_signed() * cell_width
            + mask.left
            + mask.rect.width.cast_signed();
        assert!(right <= cell_width, "full stop extends past slot midpoint");
        assert!(stop.y + mask.top >= metrics.height.cast_signed() / 2);
    }

    #[test]
    fn fallback_masks_cover_cell_text() {
        let options = FontOptions {
            hinting: Hinting::Light,
            ..FontOptions::default()
        };
        let Some(mut fonts) = jetbrains_at(20.0, 1.25, options) else {
            return;
        };
        let cases = [
            ('\u{f800}', 1, "AR PL UKai HK"),
            ('龍', 2, "Noto Sans CJK TC"),
        ];
        let results = cases.map(|(ch, cells, fixture)| -> Result<(), String> {
            let mut discovery = crate::Discovery::new(fixture, None).unwrap();
            let matched = discovery.find(None, FontStyle::default()).unwrap();
            if matched.family != fixture
                || matched
                    .load(&[], None)
                    .unwrap()
                    .0
                    .charmap()
                    .map_unicode(ch)
                    .is_none()
                || fonts.faces[0].font.charmap().map_unicode(ch).is_some()
            {
                eprintln!("skipped: {fixture} is not available as a fallback for {ch:?}");
                return Ok(());
            }
            if cells == 2 {
                fonts.discovery = discovery;
            }
            let run = fonts
                .shape(ch.encode_utf8(&mut [0; 4]), &[cells])
                .map_err(|error| format!("{ch:?}: {error}"))?;
            let [glyph] = run.glyphs[..] else {
                return Err(format!("{ch:?}: unexpected glyphs: {:?}", run.glyphs));
            };
            let mask = fonts
                .rasterize(glyph.key)
                .map_err(|error| format!("{ch:?}: {error}"))?;
            let top = glyph.y + mask.top;
            let height = mask.rect.height.cast_signed();
            let row = fonts.metrics().height.cast_signed();
            println!(
                "{ch:?}: {:?}, top={top}, height={height}, row={row}",
                fonts.family(glyph.key.face())
            );
            if height == 0 || (cells == 2 && (top < 0 || top + height > row)) {
                return Err(format!(
                    "{ch:?}: mask at y={top}, height={height}, row={row}"
                ));
            }
            Ok(())
        });
        assert_eq!(results, [Ok(()), Ok(())]);
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
                Err(FontError::MissingGlyph {
                    codepoint: 0x4e2d, ..
                }) => {
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
fn bold_shape_uses_a_heavier_face_or_synthesizes_on_the_same_grid() {
    use crate::{FontOptions, FontStyle, FontSystem};

    #[cfg(target_os = "linux")]
    let cases = [
        ("JetBrains Mono", 500, false),
        ("Liberation Mono", 400, false),
        ("Liberation Mono", 700, true),
    ];
    #[cfg(target_os = "macos")]
    let cases = [("Menlo", 400, false), ("Menlo", 700, true)];

    let coverage = |fonts: &mut FontSystem, key| -> u32 {
        let rect = fonts.rasterize(key).unwrap().rect;
        let atlas = fonts.atlas();
        let stride = atlas.dimensions()[0] as usize;
        let x = rect.x as usize;
        let width = rect.width as usize;
        atlas
            .pixels()
            .chunks_exact(stride)
            .skip(rect.y as usize)
            .take(rect.height as usize)
            .flat_map(|row| &row[x..x + width])
            .map(|&coverage| u32::from(coverage))
            .sum()
    };

    for (family, weight, synthetic) in cases {
        let options = FontOptions {
            weight: Some(weight),
            ..FontOptions::default()
        };
        let mut fonts = FontSystem::new(family, 24.0, 1.0, options).unwrap();
        if fonts.family(fonts.primary_face()) != Some(family) {
            eprintln!("skipped: {family} is not installed");
            continue;
        }
        let bold = FontStyle {
            bold: true,
            italic: false,
        };
        let regular = fonts.shape("H", &[1]).unwrap().glyphs[0];
        let styled = fonts.shape_styled("H", &[1], bold).unwrap().glyphs[0];
        assert_eq!(
            (regular.column, regular.cells, regular.x, regular.y),
            (styled.column, styled.cells, styled.x, styled.y),
            "{family} {weight}"
        );
        assert_ne!(styled.key.face(), regular.key.face());
        assert_eq!(
            fonts.faces[styled.key.face().0].synthetic,
            FontStyle {
                bold: synthetic,
                italic: false,
            },
            "{family} {weight}"
        );
        assert!(
            coverage(&mut fonts, styled.key) > coverage(&mut fonts, regular.key),
            "{family} {weight}"
        );
    }
}

#[test]
fn missing_fallback_is_discovered_once_per_style() {
    use crate::{FontError, FontOptions, FontStyle, FontSystem};

    #[cfg(target_os = "linux")]
    const FAMILY: &str = "monospace";
    #[cfg(target_os = "macos")]
    const FAMILY: &str = "Menlo";

    let bold = FontStyle {
        bold: true,
        italic: false,
    };
    let italic = FontStyle {
        bold: false,
        italic: true,
    };
    // Unassigned code points, which no installed font covers.
    let cases = [
        ('\u{378}', FontStyle::default()),
        ('\u{378}', bold),
        ('\u{379}', italic),
    ];
    let mut fonts = FontSystem::new(FAMILY, 20.0, 1.0, FontOptions::default()).unwrap();
    for (ch, style) in cases {
        let before = fonts.fallback_finds;
        assert!(fonts.face_for(ch, style).is_err(), "{ch:?} {style:?}");
        let finds = fonts.fallback_finds;
        assert!(finds > before, "{ch:?} {style:?}");
        let codepoint = u32::from(ch);
        assert!(
            matches!(
                fonts.face_for(ch, style),
                Err(FontError::MissingGlyph { codepoint: missing, .. }) if missing == codepoint
            ),
            "{ch:?} {style:?}"
        );
        assert_eq!(fonts.fallback_finds, finds, "{ch:?} {style:?}");
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

#[cfg(target_os = "macos")]
#[test]
fn core_text_loads_the_named_collection_face_and_cjk_fallback() {
    use skrifa::{MetadataProvider as _, string::StringId};

    let mut font =
        crate::FontSystem::new("Menlo-Bold", 20.0, 1.0, crate::FontOptions::default()).unwrap();
    let face = font.faces[0].sfnt().unwrap();
    assert!(
        face.localized_strings(StringId::POSTSCRIPT_NAME)
            .any(|name| name.chars().eq("Menlo-Bold".chars()))
    );
    // A missing glyph lists each fallback font that was tried and why it was rejected.
    let run = font
        .shape("水", &[2])
        .unwrap_or_else(|error| panic!("{error}"));
    let key = run.glyphs[0].key;
    assert_ne!(key.face(), font.primary_face());
    let rect = font.rasterize(key).unwrap().rect;
    let atlas = font.atlas();
    let [stride, _] = atlas.dimensions();
    assert!((rect.y..rect.y + rect.height).any(|y| {
        let start = (y * stride + rect.x) as usize;
        atlas.pixels()[start..start + rect.width as usize]
            .iter()
            .any(|&coverage| coverage != 0)
    }));
}

#[cfg(target_os = "macos")]
#[test]
fn core_text_system_wide_fallback_covers_cjk() {
    use crate::FontStyle;

    let mut discovery = crate::discovery::Discovery::new("Menlo-Bold", None).unwrap();
    for style in [
        FontStyle::default(),
        FontStyle {
            bold: true,
            italic: false,
        },
    ] {
        let matched = discovery
            .find_system_wide('水', style)
            .unwrap_or_else(|error| panic!("{style:?}: {error}"));
        // The cascade follows the user's language order, which ranks Chinese families ahead
        // of Korean ones such as AppleGothic.
        assert!(
            ["Hiragino Sans GB", "PingFang", "Songti", "STHeiti"]
                .iter()
                .any(|family| matched.family.starts_with(family)),
            "{style:?}: {}",
            matched.family
        );
        let (font, ..) = matched.load(&[], None).unwrap();
        assert!(
            font.charmap().map_unicode('水').is_some(),
            "{style:?}: {}",
            matched.family
        );
    }
}
