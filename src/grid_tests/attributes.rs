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
    let mut view = View::new(150, [256, 64], [320, 80], font, &config.colors).unwrap();
    let size = view.size();
    let mut vt = Terminal::new(size.cols, size.rows).unwrap();
    vt.write(b"\x1b[48;2;0;0;0;38;2;255;255;255m");
    for (_, sgr) in CASES {
        vt.write(format!("\x1b[24;29;55;22;{sgr}m\x1b[58;2;255;0;0m ").as_bytes());
    }
    view.build(&vt.render_snapshot().unwrap(), None, None, None, true);
    let target = renderer.create_offscreen(view.device);
    renderer.set_minimum_contrast(1.0);
    renderer.upload_atlas(&view.atlas());
    renderer.draw(&view.draw_list(true), &target);
    let pixels = super::readback(device, queue, target.texture());
    let [w, h] = view.cell.map(|n| n as usize);
    let pitch = view.device[0] as usize;
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
