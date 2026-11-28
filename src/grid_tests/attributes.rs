use sumie_render::{Renderer, wgpu};
use sumie_vt::Terminal;

use crate::view::{FontSettings, View};

pub(super) fn assert_decorations(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) {
    const CASES: [(&str, &str); 8] = [
        ("single", "4"),
        ("double", "21"),
        ("curly", "4:3"),
        ("dotted", "4:4"),
        ("dashed", "4:5"),
        ("strike", "9"),
        ("over", "53"),
        ("faint", "2;4"),
    ];
    let config = sumie_config::Config::default();
    let font = FontSettings::from_config(config.font).unwrap();
    let mut view = View::new(150, [256, 64], [320, 80], font, &config.colors, [0; 2]).unwrap();
    view.zoom(sumie_core::FontZoom::Increase).unwrap();
    let size = view.size();
    let mut vt = Terminal::new(size.cols, size.rows).unwrap();
    vt.write(b"\x1b[48;2;0;0;0;38;2;255;255;255m");
    for (_, sgr) in CASES {
        vt.write(format!("\x1b[24;29;55;22;{sgr}m\x1b[58;2;255;0;0m ").as_bytes());
    }
    let snapshot = wide_link_snapshot(&mut vt, CASES.len());
    view.build(&snapshot, None, &[], None, true);
    let target = renderer.create_offscreen(view.device);
    renderer.set_minimum_contrast(1.0);
    renderer.upload_atlas(&view.atlas());
    renderer.draw(&view.draw_list(true), &target);
    let pixels = super::readback(device, queue, target.texture());
    let [w, h] = view.cell.map(|n| n as usize);
    let pitch = view.device[0] as usize;
    assert_link_hint(&pixels, pitch, CASES.len() * w, w, h);
    let mut single_y = 0;
    for (column, (name, _)) in CASES.into_iter().enumerate() {
        let red = |x, y| pixels[4 * (y * pitch + column * w + x)];
        let rows: Vec<_> = (0..h).filter(|&y| (0..w).any(|x| red(x, y) > 32)).collect();
        let first = *rows.first().unwrap_or_else(|| panic!("missing {name}"));
        let last = *rows.last().unwrap();
        let gaps = rows.windows(2).filter(|r| r[1] > r[0] + 1).count();
        let top_at = |x| (0..h).find(|&y| red(x, y) > 32);
        let middle = top_at(w / 2);
        match name {
            "single" => {
                single_y = first;
                assert!(first > h / 2 && (0..w).all(|x| red(x, first) == 255));
                assert_eq!(
                    pixels[4 * first * pitch + 1],
                    0,
                    "SGR 58 must override white text"
                );
            }
            "double" => assert!(first < single_y && last > single_y && gaps == 1),
            "curly" => {
                assert!(middle.is_some() && top_at(0) > middle && top_at(w - 1) > middle);
                assert!(
                    top_at(0).unwrap() - middle.unwrap() >= w / 4,
                    "curly underline is too shallow"
                );
                let bg = view.draw_list(true).backgrounds[usize::from(size.cols) + column];
                assert!(
                    (h..2 * h).all(|y| (0..w).all(|x| red(x, y) == bg[0])),
                    "underline entered the next row"
                );
            }
            "dotted" => assert!(
                (0..w).any(|x| top_at(x).is_none())
                    && (first..=last).any(|y| (0..w).any(|x| (1..255).contains(&red(x, y))))
            ),
            "dashed" => assert!(top_at(0) == Some(single_y) && middle.is_none()),
            "strike" => assert!(first > 0 && last < single_y),
            "over" => assert_eq!(first, 0),
            "faint" => assert!(
                red(0, first).abs_diff(188) <= 1,
                "50% linear red over black"
            ),
            _ => unreachable!(),
        }
    }
}

fn wide_link_snapshot(vt: &mut Terminal, column: usize) -> sumie_vt::Snapshot {
    vt.write(b"\x1b[0;48;2;0;0;0m\x1b]8;;https://example.test\x1b\\    \x1b]8;;\x1b\\ ");
    let mut snapshot = vt.render_snapshot().unwrap();
    // Supply a wide blank directly so the VT width probe can fail independently.
    let cells = &mut snapshot.rows[0][column..];
    // An empty wide cell avoids depending on a CJK font, which CI runners lack.
    cells[1].text.clear();
    cells[1].width = 2;
    cells[2].text.clear();
    cells[2].width = 0;
    snapshot
}

fn assert_link_hint(pixels: &[u8], pitch: usize, start: usize, w: usize, h: usize) {
    let rows = (0..h).map(|y| {
        let offset = 4 * (y * pitch + start);
        pixels[offset..offset + 4 * 5 * w].as_chunks::<4>().0.iter()
    });
    let mut ink = rows.filter(|row| row.clone().any(|p| p[..3] != [0, 0, 0]));
    let row = ink.next().expect("idle OSC 8 link must be visible");
    assert!(ink.next().is_none(), "hint must be one device pixel thick");
    for (x, pixel) in row.enumerate() {
        if x < 4 * w && (start + x) % 3 == 1 {
            assert!(
                pixel[0] <= 1 && (60..100).contains(&pixel[1]) && (145..185).contains(&pixel[2]),
                "missing hint dot at x={}",
                start + x
            );
        } else {
            assert_eq!(
                pixel[..3],
                [0, 0, 0],
                "unexpected hint dot at x={}",
                start + x
            );
        }
    }
}

#[test]
fn color_emoji_keeps_its_palette() {
    let Some((device, queue)) = super::gpu_device(sumie_render::backends()) else {
        return;
    };
    // Force the reviewed fontconfig match even on systems preferring monochrome emoji.
    #[cfg(target_os = "linux")]
    let config =
        sumie_config::Config::from_json(br#"{"font":{"family":"Noto Color Emoji"}}"#).unwrap();
    #[cfg(target_os = "macos")]
    let config = sumie_config::Config::from_json(br#"{"font":{"family":"Menlo"}}"#).unwrap();
    let font = FontSettings::from_config(config.font).unwrap();
    let mut view = View::new(120, [220, 64], [220, 64], font, &config.colors, [0; 2]).unwrap();
    let size = view.size();
    let mut vt = Terminal::new(size.cols, size.rows).unwrap();
    vt.write("\x1b[38;2;48;48;48;48;2;32;32;32m😀█".as_bytes());
    view.build(&vt.render_snapshot().unwrap(), None, &[], None, true);
    let mut renderer = Renderer::new(&device, &queue);
    renderer.upload_atlas(&view.atlas());
    let target = renderer.create_offscreen(view.device);
    let mut render = |contrast| {
        renderer.set_minimum_contrast(contrast);
        renderer.draw(&view.draw_list(true), &target);
        super::readback(&device, &queue, target.texture())
    };
    let normal = render(1.0);
    let contrasted = render(21.0);
    let [w, h] = view.cell.map(|n| n as usize);
    let mut colored = 0;
    for (a, b) in normal
        .chunks_exact(view.device[0] as usize * 4)
        .zip(contrasted.chunks_exact(view.device[0] as usize * 4))
        .take(h)
    {
        let (emoji, text) = a.split_at(2 * w * 4);
        colored += emoji
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|[r, g, b, _]| r.abs_diff(*b) > 32 || g.abs_diff(*b) > 32)
            .count();
        assert_eq!(emoji, &b[..emoji.len()], "contrast recolored emoji");
        assert_ne!(
            text[0],
            b[emoji.len()],
            "contrast must still affect the adjacent block"
        );
        assert!(
            text[..w * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .all(|[r, g, b, _]| r == g && g == b),
            "color escaped into the adjacent cell"
        );
    }
    assert!(colored > w * h / 4, "emoji has no substantial colored ink");
}
