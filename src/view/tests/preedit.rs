use sumie_render::PixelRect;
use sumie_vt::Terminal;

use super::{LinkHighlight, focus, view};

#[test]
fn the_preedit_underline_spans_the_selected_clause() {
    let mut view = view();
    let rect = view.content_rect();
    let size = view.pane_size(rect);
    let snapshot = Terminal::new(size.cols, size.rows)
        .unwrap()
        .render_snapshot()
        .unwrap();
    let cell = view.cell;
    // Two wide characters and a narrow one: ㄅ, ㄆ and a span cells 0-1, 2-3 and 4.
    view.preedit = String::from("ㄅㄆa");
    let mut underline = |cursor| {
        view.preedit_cursor = cursor;
        view.begin_frame([], snapshot.background);
        view.build_pane(
            rect,
            &snapshot,
            None,
            LinkHighlight::default(),
            None,
            focus(1, true),
        );
        view.end_frame();
        let mut frame = view.frame(false, |_| None);
        let draw = frame.panes.next().unwrap().draw;
        let PixelRect { origin, size } = draw.cursor.unwrap().rect;
        (origin[0], size)
    };
    let (start, whole) = underline(None);
    assert_eq!(whole, [5 * cell[0], 2]);
    for caret in [(6, 6), (7, 7)] {
        assert_eq!(underline(Some(caret)), (start, whole), "caret {caret:?}");
    }
    let offset = cell[0].cast_signed();
    assert_eq!(
        underline(Some((3, 6))),
        (start + 2 * offset, [2 * cell[0], 2])
    );
    assert_eq!(underline(Some((6, 7))), (start + 4 * offset, [cell[0], 2]));
}

#[test]
fn the_search_caret_follows_a_collapsed_input_method_caret() {
    let mut view = view();
    let rect = view.content_rect();
    let size = view.pane_size(rect);
    let snapshot = Terminal::new(size.cols, size.rows)
        .unwrap()
        .render_snapshot()
        .unwrap();
    let cell = view.cell[0].cast_signed();
    view.search_bar = Some(String::from("Search: \t0/0"));
    view.search_selection = 8..8;
    // ㄅ spans two cells and a one.
    view.preedit = String::from("ㄅa");
    let mut caret = |cursor| {
        view.preedit_cursor = cursor;
        view.begin_frame([], snapshot.background);
        view.build_pane(
            rect,
            &snapshot,
            None,
            LinkHighlight::default(),
            None,
            focus(1, true),
        );
        view.end_frame();
        let mut frame = view.frame(false, |_| None);
        frame.panes.next().unwrap().draw.cursor.unwrap().rect.origin[0]
    };
    let end = caret(None);
    assert_eq!(caret(Some((4, 4))), end);
    assert_eq!(caret(Some((3, 3))), end - cell);
    assert_eq!(caret(Some((0, 0))), end - 3 * cell);
    // A selected clause leaves the caret after the preedit.
    assert_eq!(caret(Some((0, 3))), end);
}
