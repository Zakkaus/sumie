use sumie_core::splits::Rect;
use sumie_vt::{Snapshot, Terminal};

use super::{FontSettings, PaneFocus, View};
use crate::input::SearchTarget;

fn view() -> View {
    let config = sumie_config::Config::default();
    let font = FontSettings::from_config(config.font).unwrap();
    let mut view = View::new(150, [800, 500], [1000, 625], font, &config.colors).unwrap();
    view.set_tab_count(2).unwrap();
    view
}

fn snapshot(view: &View, rect: Rect) -> Snapshot {
    let size = view.pane_size(rect);
    Terminal::new(size.cols, size.rows)
        .unwrap()
        .render_snapshot()
        .unwrap()
}

fn panes(view: &View) -> [Rect; 3] {
    let content = view.content_rect();
    let left = Rect {
        width: view.cell[0] * 25 + 1,
        ..content
    };
    let divider = Rect {
        x: left.width,
        width: 1,
        ..content
    };
    let right = Rect {
        x: divider.x + 1,
        width: content.width - divider.x - 1,
        ..content
    };
    [left, divider, right]
}

fn focus(id: u64, active: bool) -> PaneFocus {
    PaneFocus {
        id,
        active,
        dirty: true,
        window_focused: true,
    }
}

#[test]
fn split_overlays_and_hit_targets_use_the_active_panes_origin() {
    let mut view = view();
    let [left, divider, right] = panes(&view);
    let bg = snapshot(&view, left);
    let fg = snapshot(&view, right);
    view.search_bar = Some(String::from("query\t0/0"));
    view.preedit = String::from("a");
    view.begin_frame([divider], fg.background);
    view.build_pane(left, &bg, None, &[], None, focus(1, false));
    view.build_pane(right, &fg, None, &[], None, focus(2, true));
    view.end_frame();
    view.frame_presented();

    let frame = view.frame(true);
    let mut draws = frame.panes;
    let left_draw = draws.next().unwrap().draw;
    assert_eq!(left_draw.image_exclusions, [None; 2]);
    let right_draw = draws.next().unwrap().draw;
    let search = right_draw.image_exclusions[1].unwrap();

    let logical = |x, y| (f64::from(x) * 0.8, f64::from(y) * 0.8);
    let pos = logical(search.origin[0] + 1, search.origin[1] + 1);
    assert!(matches!(
        view.search_hit(pos, right),
        Some(SearchTarget::Field)
    ));
    assert!(view.search_hit(pos, left).is_none());
    assert!(view.ime_rectangle(None, left).is_none());
    let caret = view.ime_rectangle(None, right).unwrap();
    assert!(caret.x >= (right.x * 4 / 5).cast_signed());

    let gutter = logical(left.width.cast_signed() - 1, left.y.cast_signed());
    let gutter = (gutter.0 + 0.4, gutter.1 + 0.4);
    assert!(view.link_cell(gutter, left, false).is_none());
}
