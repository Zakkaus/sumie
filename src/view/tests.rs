use std::sync::Arc;

use sumie_core::graphics::{Image, Placement};
use sumie_core::splits::Rect;
use sumie_core::{CellPosition, CellRange};
use sumie_render::PixelRect;
use sumie_vt::{Snapshot, Terminal};

use super::{FontSettings, LinkHighlight, PaneFocus, View};
use crate::input::{self, SearchTarget};

fn view() -> View {
    let config = sumie_config::Config::default();
    let font = FontSettings::from_config(config.font).unwrap();
    let mut view = View::new(
        150,
        [800, 500],
        [1000, 625],
        font,
        &config.colors,
        config.window.padding(),
    )
    .unwrap();
    view.set_tab_count(2).unwrap();
    view
}

#[test]
fn configure_burst_applies_only_the_last_size_at_the_next_frame() {
    let mut view = view();
    view.request_resize(120, [900, 600], [900, 600]);
    view.request_resize(150, [960, 640], [1200, 800]);
    assert_eq!(view.device, [1000, 625]);
    assert!(view.apply_pending_resize().unwrap());
    assert_eq!(view.device, [1200, 800]);
    assert_eq!(view.logical, [960, 640]);
    assert!(!view.apply_pending_resize().unwrap());
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

fn focus(id: u64, active: bool) -> PaneFocus<'static> {
    PaneFocus {
        id,
        active,
        dirty: true,
        window_focused: true,
        viewer: None,
    }
}

#[test]
fn background_opacity_respects_vt_colors_and_opaque_overlays() {
    let mut view = view();
    let rect = view.content_rect();
    let size = view.pane_size(rect);
    let mut terminal = Terminal::new(size.cols, size.rows).unwrap();
    terminal
        .set_colors([18; 3], [72; 3], None, &[[18; 3]; 16])
        .unwrap();
    terminal.write(b"\x1b[?5hA\x1b[40mB\x1b[48;2;18;18;18mC\x1b[0;7mD\x1b[0mEFG");
    let snapshot = terminal.render_snapshot().unwrap();
    assert_eq!(snapshot.background, [18; 3]);
    view.search_highlights.push((0, 5, 5, true));
    view.preedit = String::from("x");
    let selection = CellRange::group(CellPosition { row: 0, column: 4 }, 1);
    let mut focus = focus(1, true);
    focus.dirty = false;

    for (opacity, alpha, changed) in [
        (1.0, 255, false),
        (0.999, 255, false),
        (0.5, 128, true),
        (1.0, 255, true),
    ] {
        assert_eq!(
            view.set_background_alpha(crate::gpu::background_alpha(opacity)),
            changed
        );
        view.begin_frame([], snapshot.background);
        view.build_pane(
            rect,
            &snapshot,
            Some(selection),
            LinkHighlight::default(),
            None,
            focus,
        );
        view.end_frame();

        let mut frame = view.frame(false);
        let draw = frame.panes.next().unwrap().draw;
        assert_eq!(frame.background, [18, 18, 18, alpha]);
        assert_eq!(draw.default_background, frame.background);
        assert_eq!(draw.backgrounds[1..3], [[18, 18, 18, 255]; 2]);
        assert_eq!(
            std::array::from_fn(|i| draw.backgrounds[i][3]),
            [alpha, 255, 255, 255, 255, 255, alpha, 255],
        );
        assert_eq!(draw.cursor.unwrap().color[3], 255);
    }
}

#[test]
fn split_overlays_and_hit_targets_use_the_active_panes_origin() {
    let mut view = view();
    let [left, divider, right] = panes(&view);
    let bg = snapshot(&view, left);
    let fg = snapshot(&view, right);
    view.search_bar = Some(String::from("Search: query\t0/0"));
    view.search_selection = 8..13;
    view.preedit = String::from("a");
    view.begin_frame([divider], fg.background);
    view.build_pane(
        left,
        &bg,
        None,
        LinkHighlight::default(),
        None,
        focus(1, false),
    );
    view.build_pane(
        right,
        &fg,
        None,
        LinkHighlight::default(),
        None,
        focus(2, true),
    );
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
    // One inset cell, "Search: ", and one preedit cell. No space for the selected query.
    assert_eq!(
        caret.x,
        (search.origin[0] + (10 * view.cell[0]).cast_signed()) * 4 / 5
    );

    let [x, y] = right_draw.origin;
    let first_cell = logical(x + 1, y + 1);
    let (hit, pixel, over_grid) = view.pointer_position(first_cell, right);
    assert_eq!([hit.column, hit.row], [0, 0]);
    assert!(pixel.into_iter().all(|value| (0.99..1.01).contains(&value)));
    assert!(over_grid);
    assert_eq!(view.link_cell(first_cell, right, false), Some(hit));
    assert!(
        view.link_cell(logical(x - 1, y + 1), right, false)
            .is_none()
    );
}

#[test]
fn padding_breaks_click_counts_but_clamps_an_active_selection() {
    use sumie_core::mouse::{Dispatch, MouseModes, MouseState, SelectStep, SelectionUnit};
    use sumie_wayland::{
        Modifiers,
        PointerButton::Left,
        PointerPhase::{Motion, Press, Release},
    };

    let mut view = view();
    view.set_padding([8; 2]);
    let mut rect = view.content_rect();
    rect.width = 2 * (view.cell[0] + view.pane_padding()[0]) + 1;
    let grid = view.pane_grid(rect);
    let logical = |px| f64::from(px) * 120.0 / f64::from(view.scale);
    let event = |phase, x| {
        input::mouse_event(
            phase,
            Modifiers::default(),
            view.pointer_position((logical(x), logical(grid.y + 1)), rect),
        )
    };
    let inside = grid.x + 1;
    let left_padding = grid.x - 1;
    let right_padding = grid.x + grid.width;
    let release = Release(Left);
    let mut state = MouseState::default();
    let modes = MouseModes {
        tracking: false,
        alternate: false,
        alternate_scroll: false,
    };
    let mut select = |phase, x| match state.dispatch(event(phase, x), modes) {
        Dispatch::Select(step) => Some(step),
        Dispatch::Ignore => None,
        _ => panic!("unexpected terminal mouse report"),
    };

    for (time, x, unit) in [
        (1, inside, Some(SelectionUnit::Cell)),
        (2, left_padding, None),
        (3, left_padding, None),
        (4, left_padding, None),
    ] {
        assert_eq!(select(Press(Left, time), x), unit.map(SelectStep::Begin));
        assert_eq!(select(release, x), unit.map(SelectStep::Finish));
    }
    let cell = SelectionUnit::Cell;
    assert_eq!(
        select(Press(Left, 5), inside),
        Some(SelectStep::Begin(cell))
    );
    assert_eq!(event(Motion, right_padding).cell.column, 1);
    assert_eq!(select(Motion, right_padding), Some(SelectStep::Drag(cell)));
    assert_eq!(
        select(release, right_padding),
        Some(SelectStep::Finish(cell))
    );
}

fn thumbnail_snapshot(view: &View) -> Snapshot {
    let [w, h] = view.cell;
    let mut snapshot = snapshot(view, view.content_rect());
    let thumbnail = Placement {
        image: Arc::new(Image {
            generation: 1,
            size: [1, 1],
            rgba: vec![255; 4],
        }),
        image_id: 1,
        placement_id: 1,
        storage_key: [0; 3],
        z: 0,
        cell: [1, -1],
        offset: [0, h - 2],
        size: [w, h + 2],
        source: [0, 0, 1, 1],
        thumbnail: true,
    };
    snapshot.images = Arc::new(vec![
        thumbnail.clone(),
        Placement {
            placement_id: 2,
            cell: [1, 0],
            offset: [0, 0],
            size: [w, h / 2],
            thumbnail: false,
            ..thumbnail
        },
    ]);
    snapshot
}

#[test]
fn thumbnail_hit_uses_the_presented_row_after_scrolling() {
    let mut view = view();
    let rect = view.pane_grid(view.content_rect());
    let [w, h] = view.cell;
    let mut snapshot = thumbnail_snapshot(&view);
    view.build(&snapshot, None, &[], None, true);
    // The renderer clips the two pixels above the scrolled viewport.
    let bounds = PixelRect {
        origin: [(rect.x + w).cast_signed(), rect.y.cast_signed()],
        size: [w, h],
    };
    let covering = PixelRect {
        size: [w, h / 2],
        ..bounds
    };
    let logical_y = |y| {
        (
            f64::from(rect.x + w) * 0.8 + 0.4,
            (f64::from(y) + 0.5) * 0.8,
        )
    };
    let inside = logical_y(rect.y + h - 1);
    assert!(view.thumbnail_hit(inside, 0).is_none());
    view.images_presented([(0, 0, bounds), (0, 1, covering)].into_iter());
    assert_eq!(view.thumbnail_hit(inside, 0), snapshot.images.first());
    for (point, pane) in [
        (logical_y(rect.y), 0),
        (logical_y(rect.y + h), 0),
        (logical_y(rect.y - 1), 0),
        (inside, 1),
    ] {
        assert!(view.thumbnail_hit(point, pane).is_none());
    }
    for (point, eligible, hovered) in [
        (logical_y(rect.y), true, false),
        (inside, false, false),
        (inside, true, true),
    ] {
        assert_eq!(
            view.hover_thumbnail(Some((0, point)), |_| eligible),
            hovered
        );
        assert_eq!(view.thumbnail_hover_dirty(), hovered);
    }
    assert_eq!(
        view.thumbnail_chrome(&view.panes[0]).unwrap().hovered,
        Some(0)
    );
    view.images_presented([(0, 0, bounds), (0, 1, covering)].into_iter());
    let steady = view.hover_thumbnail(Some((0, inside)), |_| true);
    assert_eq!((steady, view.thumbnail_hover_dirty()), (true, false));

    // A fresh list at the same index must not inherit the previous placement's hover.
    Arc::make_mut(&mut snapshot.images)[0].placement_id = 3;
    view.build(&snapshot, None, &[], None, true);
    assert_eq!(view.thumbnail_chrome(&view.panes[0]).unwrap().hovered, None);

    snapshot.images = Arc::default();
    view.build(&snapshot, None, &[], None, true);
    assert!(view.thumbnail_hit(inside, 0).is_some());
    view.images_presented(std::iter::empty());
    assert!(view.thumbnail_hit(inside, 0).is_none());
    let cleared = view.hover_thumbnail(Some((0, inside)), |_| true);
    assert_eq!((cleared, view.thumbnail_hover_dirty()), (false, true));
}
