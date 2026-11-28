use sumie_core::splits::Rect;

use super::{LinkHighlight, focus, snapshot, view};

#[test]
fn osc8_target_is_drawn_in_the_banner_row() {
    let mut view = view();
    let rect = Rect {
        width: 20 * view.cell[0] + 2 * view.pane_padding()[0],
        ..view.content_rect()
    };
    let snapshot = snapshot(&view, rect);
    let mut draw = |target, banner| {
        view.begin_frame([], snapshot.background);
        let link = LinkHighlight {
            target,
            ..LinkHighlight::default()
        };
        view.build_pane(rect, &snapshot, None, link, banner, focus(1, true));
        view.end_frame();
        let draw = view.draw_list(false);
        (draw.backgrounds[0], draw.glyphs.to_vec())
    };
    let preview = draw(Some("https://long-user-name@actual.test/a@b"), None);
    assert_eq!(preview.0[..3], snapshot.foreground);
    assert_ne!(preview.1, []);
    assert_eq!(preview, draw(None, Some("…actual.test")));
    assert_eq!(
        draw(Some("https://actual.test"), None),
        draw(None, Some("https://actual.test")),
    );
    assert_eq!(
        draw(Some(r"https://actual.test\segment@other.test/path"), None),
        draw(None, Some("https://actual.test…")),
    );
    let (background, glyphs) = draw(None, None);
    assert_eq!(background[..3], snapshot.background);
    assert_eq!(glyphs, []);
}

#[test]
fn banner_row_covers_terminal_with_opaque_terminal_colors() {
    let mut view = view();
    let rect = view.content_rect();
    let mut snap = snapshot(&view, rect);
    snap.foreground = [255, 0, 0];
    view.set_background_alpha(96);
    view.begin_frame([], snap.background);
    view.build_pane(
        rect,
        &snap,
        None,
        LinkHighlight::default(),
        Some("Close? y · Enter/Esc/n"),
        focus(0, true),
    );
    view.end_frame();
    let draw = view.frame(false, |_| None).panes.next().unwrap().draw;
    let row = &draw.backgrounds[..usize::from(view.pane_size(rect).cols)];
    assert!(row.iter().all(|&bg| bg == [255, 0, 0, 255]));
    assert!(
        draw.glyphs
            .iter()
            .any(|glyph| glyph.color == super::super::rgba(snap.background))
    );
}
