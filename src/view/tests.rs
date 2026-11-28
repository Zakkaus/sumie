use std::sync::Arc;

use sumie_core::graphics::{Image, PixelFormat, Placement};
use sumie_core::splits::Rect;
use sumie_core::{CellPosition, CellRange};
use sumie_render::PixelRect;
use sumie_vt::{Snapshot, Terminal};

use super::{FontSettings, LinkHighlight, PaneFocus, View, surface_logical_rect};
use crate::{
    input::{self, SearchTarget},
    platform::Position,
};

fn view() -> View {
    let mut view = scaled_view(150, [800, 500], [1000, 625]);
    view.set_tab_count(2).unwrap();
    view
}

/// A view at `scale`/120 with the given logical and buffer sizes.
fn scaled_view(scale: u32, logical: [u32; 2], device: [u32; 2]) -> View {
    let config = sumie_config::Config::default();
    let font = FontSettings::from_config(config.font).unwrap();
    View::new(
        scale,
        logical,
        device,
        font,
        &config.colors,
        config.window.padding(),
    )
    .unwrap()
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
        scroll: 0,
    }
}

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

        let mut frame = view.frame(false, |_| None);
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
fn scroll_offset_reveals_the_row_below_and_moves_hits_with_the_content() {
    let mut view = view();
    let rect = view.content_rect();
    let size = view.pane_size(rect);
    let mut vt = Terminal::new(size.cols, size.rows).unwrap();
    vt.write("line\r\n".repeat(usize::from(size.rows) * 2).as_bytes());
    vt.scroll_rows(-3).unwrap();
    let snapshot = vt.render_snapshot().unwrap();
    let grid = view.pane_grid(rect);
    let [columns, rows] = [u32::from(size.cols), u32::from(size.rows)];
    let cell = view.cell;
    let scale = f64::from(view.scale);
    let logical = |px: u32| f64::from(px) * 120.0 / scale;
    let mut carets = Vec::new();
    for (offset, extra, backgrounds) in [(0, 0, rows), (7, 1, rows + 1)] {
        view.begin_frame([], snapshot.background);
        let focus = PaneFocus {
            scroll: offset,
            ..focus(0, true)
        };
        view.build_pane(rect, &snapshot, None, LinkHighlight::default(), None, focus);
        view.end_frame();
        let draw = view.draw_list(false);
        assert_eq!(
            (draw.scroll.offset, draw.scroll.extra_rows),
            (offset, extra)
        );
        assert_eq!(draw.backgrounds.len(), (columns * backgrounds) as usize);
        // The caret rectangle follows the content up.
        let cursor = sumie_vt::Cursor {
            column: 0,
            row: 2,
            visible: true,
            color: [0; 3],
            style: sumie_vt::CursorStyle::Block,
            blink: false,
        };
        let rectangle = view
            .ime_rectangle(Some(cursor), rect, surface_logical_rect)
            .unwrap();
        carets.push(rectangle.y);
    }
    // Logical rectangles round outward, so allow one pixel.
    assert!((f64::from(carets[0] - carets[1]) - logical(7)).abs() <= 1.5);
    // With the content 7 px up, a point near the bottom of the first row is in the second.
    let near_first_row_end =
        Position::SurfaceLogical(logical(grid.x + 1), logical(grid.y + cell[1] - 2));
    let row_of = |scroll| {
        view.link_cell(near_first_row_end, rect, false, scroll)
            .unwrap()
            .row
    };
    assert_eq!((row_of(0), row_of(7)), (0, 1));
    // The strip of the partly shown row below the grid has no cells to link.
    let strip = Position::SurfaceLogical(logical(grid.x + 1), logical(grid.y + rows * cell[1] - 2));
    assert!(view.link_cell(strip, rect, false, 7).is_none());
    assert_eq!(view.pointer_position(strip, rect, 7).0.row, size.rows - 1);
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

    let frame = view.frame(true, |_| None);
    let mut draws = frame.panes;
    let left_draw = draws.next().unwrap().draw;
    assert_eq!(left_draw.image_exclusions, [None; 2]);
    let right_draw = draws.next().unwrap().draw;
    let search = right_draw.image_exclusions[1].unwrap();

    let logical = |x, y| Position::SurfaceLogical(f64::from(x) * 0.8, f64::from(y) * 0.8);
    let pos = logical(search.origin[0] + 1, search.origin[1] + 1);
    assert!(matches!(
        view.search_hit(pos, right),
        Some(SearchTarget::Field)
    ));
    assert!(view.search_hit(pos, left).is_none());
    assert!(
        view.ime_rectangle(None, left, surface_logical_rect)
            .is_none()
    );
    let caret = view
        .ime_rectangle(None, right, surface_logical_rect)
        .unwrap();
    // One inset cell, "Search: ", and one preedit cell. No space for the selected query.
    assert_eq!(
        caret.x,
        (search.origin[0] + (10 * view.cell[0]).cast_signed()) * 4 / 5
    );

    let [x, y] = right_draw.origin;
    let first_cell = logical(x + 1, y + 1);
    let (hit, pixel, over_grid) = view.pointer_position(first_cell, right, 0);
    assert_eq!([hit.column, hit.row], [0, 0]);
    assert!(pixel.into_iter().all(|value| (0.99..1.01).contains(&value)));
    assert!(over_grid);
    assert_eq!(view.link_cell(first_cell, right, false, 0), Some(hit));
    assert!(
        view.link_cell(logical(x - 1, y + 1), right, false, 0)
            .is_none()
    );
}

/// Device pixels on both sides of each grid edge and the divider of a two-pane split, with
/// the cell each one hits.
fn boundary_hits(view: &View) -> [(Rect, [u32; 2], Option<[u16; 2]>); 8] {
    let [left, divider, right] = panes(view);
    let [left_grid, right_grid] = [left, right].map(|rect| view.pane_grid(rect));
    let cells = |grid: Rect, axis: usize| [grid.width, grid.height][axis] / view.cell[axis];
    let last = |grid: Rect, axis: usize| u16::try_from(cells(grid, axis) - 1).unwrap();
    let [left_end, right_end] =
        [left_grid, right_grid].map(|grid| grid.x + cells(grid, 0) * view.cell[0]);
    let bottom = right_grid.y + cells(right_grid, 1) * view.cell[1];
    let row = left_grid.y + 2 * view.cell[1];
    let corner = [last(right_grid, 0), last(right_grid, 1)];
    [
        (left, [left_grid.x - 1, row], None),
        (left, [left_grid.x, row], Some([0, 2])),
        (left, [left_end - 1, row], Some([last(left_grid, 0), 2])),
        (left, [divider.x, row], None),
        (right, [divider.x, row], None),
        (right, [right_grid.x, row], Some([0, 2])),
        (right, [right_end - 1, bottom - 1], Some(corner)),
        (right, [right_end - 1, bottom], None),
    ]
}

/// Points just inside both edges of each boundary pixel, mapped by `to_position`.
fn assert_boundary_hits(view: &View, to_position: impl Fn([f64; 2]) -> Position) {
    let divider = panes(view)[1];
    for (rect, pixel, hit) in boundary_hits(view) {
        for inset in [0.02, 0.98] {
            let position = to_position(pixel.map(|px| f64::from(px) + inset));
            let (cell, _, over_grid) = view.pointer_position(position, rect, 0);
            let found = over_grid.then_some([cell.column, cell.row]);
            let scale = view.scale;
            assert_eq!(found, hit, "{scale}/120: device pixel {pixel:?} + {inset}");
            if pixel[0] == divider.x {
                let edge = f64::from(divider.x);
                assert!((edge..edge + 1.0).contains(&view.window_pixel(position)[0]));
            }
        }
    }
}

#[test]
fn odd_fractional_sizes_hit_by_the_per_axis_viewporter_ratio() {
    // At 1.25×, 801×599 rounds to a 1001×749 buffer, so each axis has its own ratio.
    let (logical, device) = ([801, 599], [1001, 749]);
    let view = scaled_view(150, logical, device);
    assert_boundary_hits(&view, |pixel| {
        let at = |axis: usize| pixel[axis] * f64::from(logical[axis]) / f64::from(device[axis]);
        Position::SurfaceLogical(at(0), at(1))
    });
}

#[test]
fn device_pixel_hits_ignore_the_rounded_point_extent() {
    // An odd backing extent at 2× or 1.5× is a fractional point extent (800.5 or 800.67
    // points), which the view keeps only rounded. Backing-pixel hits must not use it.
    for (scale, device) in [(240, [1601, 1199]), (180, [1201, 899])] {
        let points = device.map(|px| (px * 120 + scale / 2) / scale);
        let view = scaled_view(scale, points, device);
        assert_boundary_hits(&view, |[x, y]| Position::DevicePx(x, y));
    }
}

#[test]
fn padding_breaks_click_counts_but_clamps_an_active_selection() {
    use crate::platform::{
        Modifiers,
        PointerButton::Left,
        PointerPhase::{Motion, Press, Release},
    };
    use sumie_core::mouse::{Dispatch, MouseModes, MouseState, SelectStep, SelectionUnit};

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
            view.pointer_position(
                Position::SurfaceLogical(logical(x), logical(grid.y + 1)),
                rect,
                0,
            ),
        )
    };
    let inside = grid.x + 1;
    let left_padding = grid.x - 1;
    let right_padding = grid.x + grid.width;
    let release = Release(Left);
    let mut state = MouseState::new(sumie_core::Platform::Linux);
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
        let finish = |unit| SelectStep::Finish { unit, copy: true };
        assert_eq!(select(release, x), unit.map(finish));
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
        Some(SelectStep::Finish {
            unit: cell,
            copy: true
        })
    );
}

fn thumbnail_snapshot(view: &View) -> Snapshot {
    let [w, h] = view.cell;
    let mut snapshot = snapshot(view, view.content_rect());
    let thumbnail = Placement {
        image: Image {
            generation: 1,
            size: [1, 1],
            format: PixelFormat::Rgba,
        },
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
fn visible_animation_survives_neighbor_replacement() {
    let mut view = view();
    let mut snapshot = thumbnail_snapshot(&view);
    let images = Arc::make_mut(&mut snapshot.images);
    images[0].storage_key = [1, 2, 3];
    images[1].storage_key = [2, 4, 5];
    view.build(&snapshot, None, &[], None, true);
    let bounds = PixelRect {
        origin: [0; 2],
        size: [1; 2],
    };
    view.images_presented([(0, 0, bounds), (0, 1, bounds)].into_iter());

    let images = Arc::make_mut(&mut snapshot.images);
    images[0].storage_key[2] += 1;
    images[1].image.generation += 1;
    assert!(
        view.visible_placements(0, &snapshot.images)
            .eq([&snapshot.images[1]])
    );
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
        Position::SurfaceLogical(
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

#[test]
fn a_lone_tab_shows_progress_along_the_top_edge_and_only_while_reported() {
    use sumie_core::notify::{Progress, ProgressState};

    let mut view = view();
    view.set_tab_count(1).unwrap();
    let rect = view.content_rect();
    let snapshot = snapshot(&view, rect);
    let report = |state, percent| Some(Progress { state, percent });
    // (report, fills: the track and one for the state)
    for (progress, fills) in [
        (None, 0),
        (report(ProgressState::Set, Some(40)), 2),
        (report(ProgressState::Error, None), 2),
    ] {
        view.build(&snapshot, None, &[], None, true);
        view.draw_edge_progress(progress, &snapshot);
        let frame = view.frame(false, |_| None);
        assert_eq!(frame.fills.len(), fills, "{progress:?}");
        assert!(frame.chrome.is_none());
        for fill in frame.fills {
            assert_eq!(fill.rect.origin[1], 0);
            assert_eq!(fill.rect.size[1], 3, "2 logical px at scale 1.25");
        }
        if progress.is_some_and(|p| p.state == ProgressState::Error) {
            assert_eq!(
                frame.fills[1].color,
                [
                    snapshot.ansi_red[0],
                    snapshot.ansi_red[1],
                    snapshot.ansi_red[2],
                    255
                ]
            );
            assert_eq!(frame.fills[1].rect.size[0], view.device[0]);
        }
    }
}

mod preedit;
