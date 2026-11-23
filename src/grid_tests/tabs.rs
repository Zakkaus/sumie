use sumie_render::{Renderer, wgpu};
use sumie_ui::Palette;
use sumie_vt::Terminal;

use crate::view::{FontSettings, View};

/// Read the actual offscreen frame through a fractional-scale show, recolor, and hide.
pub(super) fn assert_tab_bar(renderer: &mut Renderer, device: &wgpu::Device, queue: &wgpu::Queue) {
    const DEVICE: [u32; 2] = [320, 160];
    let config = sumie_config::Config::default();
    let font = FontSettings::from_config(config.font).unwrap();
    let mut view = View::new(150, [256, 128], DEVICE, font, &config.colors).unwrap();
    let target = renderer.create_offscreen(DEVICE);
    let size = view.size();
    let mut vt = Terminal::new(size.cols, size.rows).unwrap();
    for (count, light) in [(1, false), (2, false), (2, true), (1, true)] {
        view.set_tab_count(count).unwrap();
        let size = view.size();
        vt.resize(size.cols, size.rows).unwrap();
        let bg = if light { [236, 231, 228] } else { [27, 24, 34] };
        vt.set_colors([100; 3], bg, None, &[[0; 3]; 16]).unwrap();
        let snapshot = vt.render_snapshot().unwrap();
        let labels = ["shell", "a long title that will not fit into its tab"];
        view.build(&snapshot, None, None, true);
        view.draw_tabs(
            labels.into_iter().take(count),
            count - 1,
            snapshot.background,
        );
        renderer.upload_atlas(&view.atlas());
        view.atlas_uploaded();
        renderer.draw(&view.draw_list(true), &target);
        view.frame_presented();
        let frame = super::readback(device, queue, target.texture());
        let pixel = |x: u32, y: u32| {
            let at = (y * DEVICE[0] + x) as usize * 4;
            &frame[at..at + 4]
        };
        let top = DEVICE[1] - u32::from(size.height_px);
        assert_eq!(top > 0, count > 1, "only multiple tabs displace the grid");
        assert_eq!(pixel(0, top), [bg[0], bg[1], bg[2], 255]);
        assert_eq!(view.tab_hit((180.0, 1.0)), (count > 1).then_some(1));
        if count > 1 {
            let surface = Palette::for_luminance(if light { 1.0 } else { 0.0 }).surface;
            assert_eq!(pixel(0, 0), surface);
            assert_ne!(pixel(80, top - 1), pixel(240, top - 1), "selected rail");
            for start in [0, 160] {
                assert!(
                    (0..top / 2).any(|y| (start..start + 160).any(|x| pixel(x, y) != surface)),
                    "label ink"
                );
            }
            let logical_top = f64::from(top) / 1.25;
            assert!(view.pointer_position((0.0, logical_top)).1[1].abs() < f32::EPSILON);
            assert_eq!(
                view.ime_rectangle(snapshot.cursor).unwrap().y,
                top.cast_signed() * 120 / 150
            );
        }
    }
}
