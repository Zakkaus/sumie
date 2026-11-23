use super::{Axis, Direction, Layout, Rect, SplitTree};

const fn rect(x: u32, y: u32, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

const BOUNDS: Rect = rect(10, 20, 121, 81);

#[test]
fn nested_splits_partition_window_pixels() {
    let mut tree = SplitTree::new(1);
    tree.split(Axis::Horizontal, 2);
    tree.split(Axis::Vertical, 3);
    assert_eq!(tree.focused(), 3);

    let mut layout = Layout::default();
    let bounds = rect(10, 20, 10, 8);
    tree.layout(bounds, [1; 2], &mut layout);
    assert_eq!(
        layout.panes,
        [
            (1, rect(10, 20, 4, 8)),
            (2, rect(15, 20, 5, 3)),
            (3, rect(15, 24, 5, 4)),
        ]
    );
    assert_eq!(layout.dividers.len(), 2);
    assert_eq!(layout.dividers[0].rect, rect(14, 20, 1, 8));
    assert_eq!(layout.dividers[1].rect, rect(15, 23, 5, 1));
    for y in 19..=28 {
        for x in 9..=20 {
            let point = [x, y];
            let hits = layout
                .panes
                .iter()
                .map(|&(_, pane)| pane)
                .chain(layout.dividers.iter().map(|divider| divider.rect))
                .filter(|rect| rect.contains(point))
                .count();
            assert_eq!(hits, usize::from(bounds.contains(point)), "{point:?}");
        }
    }
}

#[test]
fn tiny_windows_partition_every_pixel() {
    let mut tree = SplitTree::new(1);
    tree.split(Axis::Horizontal, 2);
    let mut layout = Layout::default();

    for (width, left, divider, right) in [
        (0, 0, 0, 0),
        (1, 0, 0, 1),
        (2, 1, 0, 1),
        (3, 1, 1, 1),
        (4, 1, 1, 2),
        (5, 2, 1, 2),
    ] {
        tree.layout(rect(10, 20, width, 1), [1; 2], &mut layout);
        assert_eq!(
            layout.panes,
            [
                (1, rect(10, 20, left, 1)),
                (2, rect(10 + left + divider, 20, right, 1)),
            ],
            "width={width}"
        );
        assert_eq!(layout.dividers.len(), usize::from(divider != 0));
    }
    tree.layout(rect(10, 20, 3, 0), [1; 2], &mut layout);
    assert_eq!(layout.dividers, []);
    assert!(!tree.focus_neighbor(Direction::Left, &layout));

    tree.layout(rect(10, 20, 2, 1), [1; 2], &mut layout);
    assert!(tree.focus_neighbor(Direction::Left, &layout));
    assert!(!tree.resize(Direction::Right, &layout));
    assert!(!tree.drag(0, [10, 20], &layout));
}

#[test]
fn close_promotes_the_sibling_without_changing_its_divider_identity() {
    let mut tree = SplitTree::new(1);
    tree.split(Axis::Horizontal, 2);
    tree.split(Axis::Vertical, 3);
    tree.split(Axis::Horizontal, 4);
    let mut layout = Layout::default();
    tree.layout(BOUNDS, [1; 2], &mut layout);
    let root_divider = layout.dividers[0].id;
    let promoted_divider = layout.dividers[1].id;
    assert!(tree.focus(1));
    assert!(!tree.focus(1));
    assert!(!tree.focus(42));

    tree.close(1);
    assert_eq!(tree.focused(), 2);
    tree.layout(BOUNDS, [1; 2], &mut layout);
    assert_eq!(layout.dividers[0].id, promoted_divider);
    assert!(!tree.drag(root_divider, [40, 30], &layout));

    tree.close(3);
    assert_eq!(tree.focused(), 2);
    tree.layout(BOUNDS, [1; 2], &mut layout);
    assert_eq!(
        layout.panes,
        [(2, rect(10, 20, 121, 40)), (4, rect(10, 61, 121, 40))]
    );
    assert_eq!(layout.dividers[0].id, promoted_divider);

    tree.close(42);
    tree.close(2);
    tree.close(4);
    assert_eq!(tree.focused(), 4);
    tree.layout(BOUNDS, [1; 2], &mut layout);
    assert_eq!(layout.panes, [(4, BOUNDS)]);
    assert_eq!(layout.dividers, []);
}

#[test]
fn close_the_second_child_focuses_the_last_leaf_in_its_sibling() {
    let mut tree = SplitTree::new(1);
    tree.split(Axis::Horizontal, 3);
    assert!(tree.focus(1));
    tree.split(Axis::Vertical, 2);
    assert!(tree.focus(3));
    tree.close(3);
    assert_eq!(tree.focused(), 2);
}

#[test]
fn focus_uses_shared_edges_without_wrapping() {
    let mut tree = SplitTree::new(1);
    tree.split(Axis::Horizontal, 2);
    tree.split(Axis::Vertical, 3);
    let mut layout = Layout::default();
    tree.layout(BOUNDS, [1; 2], &mut layout);

    for (from, direction, to) in [
        (1, Direction::Right, 2),
        (2, Direction::Down, 3),
        (3, Direction::Up, 2),
        (3, Direction::Left, 1),
        (1, Direction::Up, 1),
        (1, Direction::Left, 1),
        (3, Direction::Right, 3),
    ] {
        tree.focus(from);
        assert_eq!(tree.focus_neighbor(direction, &layout), from != to);
        assert_eq!(tree.focused(), to, "{from} {direction:?}");
    }

    assert!(tree.drag(layout.dividers[1].id, [0, 30], &layout));
    tree.layout(BOUNDS, [1; 2], &mut layout);
    assert!(tree.focus(1));
    assert!(tree.focus_neighbor(Direction::Right, &layout));
    assert_eq!(tree.focused(), 3);
}

#[test]
fn keyboard_resize_uses_the_divider_on_the_requested_side_first() {
    let mut tree = SplitTree::new(1);
    tree.split(Axis::Horizontal, 2);
    tree.split(Axis::Horizontal, 3);
    assert!(tree.focus(2));
    let mut layout = Layout::default();
    tree.layout(BOUNDS, [1; 2], &mut layout);
    assert!(!tree.resize(Direction::Up, &layout));

    assert!(tree.resize(Direction::Right, &layout));
    tree.layout(BOUNDS, [1; 2], &mut layout);
    assert_eq!(layout.dividers[0].rect.x, 70);
    assert_eq!(layout.dividers[1].rect.x, 108);

    assert!(tree.resize(Direction::Left, &layout));
    tree.layout(BOUNDS, [1; 2], &mut layout);
    assert_eq!(layout.dividers[0].rect.x, 62);
    assert_eq!(layout.dividers[1].rect.x, 105);

    assert!(tree.focus(1));
    assert!(tree.resize(Direction::Left, &layout));
    tree.layout(BOUNDS, [1; 2], &mut layout);
    assert_eq!(layout.dividers[0].rect.x, 54);
}

#[test]
fn drag_uses_absolute_coordinates_and_keeps_its_fraction_on_window_resize() {
    let mut tree = SplitTree::new(1);
    tree.split(Axis::Vertical, 2);
    let mut layout = Layout::default();
    let bounds = rect(10, 20, 121, 101);
    tree.layout(bounds, [1; 2], &mut layout);
    let id = layout.dividers[0].id;
    assert!(!tree.drag(u64::MAX, [0, 47], &layout));
    assert!(!tree.drag(id, [0, 70], &layout));
    assert!(tree.drag(id, [0, 47], &layout));
    tree.layout(bounds, [1; 2], &mut layout);
    assert_eq!(layout.dividers[0].rect.y, 47);

    tree.layout(rect(10, 20, 121, 201), [1; 2], &mut layout);
    assert_eq!(layout.dividers[0].id, id);
    assert_eq!(layout.dividers[0].rect.y, 74);

    for (requested, actual) in [(0, 21), (u32::MAX, 119)] {
        tree.layout(bounds, [1; 2], &mut layout);
        assert!(tree.drag(id, [0, requested], &layout));
        tree.layout(bounds, [1; 2], &mut layout);
        assert_eq!(layout.dividers[0].rect.y, actual);
        assert!(!tree.drag(id, [0, requested], &layout));
    }
}

#[test]
fn moving_parent_divider_keeps_nested_panes_navigable() {
    let mut tree = SplitTree::new(1);
    tree.split(Axis::Horizontal, 2);
    tree.split(Axis::Horizontal, 3);
    assert!(tree.focus(1));
    let mut layout = Layout::default();
    let bounds = rect(0, 0, 801, 40);
    let cell = [12, 20];
    tree.layout(bounds, cell, &mut layout);
    let parent = layout.dividers[0].id;

    assert!(tree.drag(parent, [771, 0], &layout));
    tree.layout(bounds, cell, &mut layout);
    assert!(tree.resize(Direction::Right, &layout));
    tree.layout(bounds, cell, &mut layout);
    assert_eq!(
        layout.panes[1..],
        [(2, rect(776, 0, 12, 40)), (3, rect(789, 0, 12, 40))]
    );
    assert!(!tree.drag(parent, [u32::MAX, 0], &layout));
    assert!(tree.focus_neighbor(Direction::Right, &layout));
    assert_eq!(tree.focused(), 2);

    tree.layout(rect(0, 0, 2, 40), cell, &mut layout);
    assert_eq!(layout.dividers, []);
    assert!(tree.focus_neighbor(Direction::Right, &layout));
    assert_eq!(tree.focused(), 3);
}

#[test]
fn zoom_preserves_split_ratios_and_hidden_pty_sizes() {
    let mut tree = SplitTree::new(1);
    tree.split(Axis::Horizontal, 2);
    let mut layout = Layout::default();
    tree.layout(BOUNDS, [1; 2], &mut layout);
    let id = layout.dividers[0].id;
    assert!(tree.drag(id, [40, 20], &layout));
    tree.layout(BOUNDS, [1; 2], &mut layout);
    let hidden = layout.panes[0];
    let unzoomed = layout.dividers[0];

    tree.toggle_zoom();
    tree.layout(BOUNDS, [1; 2], &mut layout);
    assert_eq!(layout.panes, [(2, BOUNDS)]);
    assert_eq!(layout.hidden, [hidden]);
    assert_eq!(layout.dividers, []);
    assert!(!tree.resize(Direction::Right, &layout));
    assert!(!tree.drag(id, [60, 20], &layout));

    assert!(tree.focus_neighbor(Direction::Left, &layout));
    assert_eq!(tree.focused(), 1);
    tree.layout(BOUNDS, [1; 2], &mut layout);
    assert_eq!(layout.dividers, [unzoomed]);
    assert_eq!(layout.hidden, []);
}

#[test]
fn preview_uses_the_unzoomed_split_bounds_and_does_not_change_state() {
    let mut tree = SplitTree::new(1);
    tree.split(Axis::Horizontal, 2);
    let mut layout = Layout::default();
    let cell = [12, 20];
    tree.layout(BOUNDS, cell, &mut layout);
    assert!(tree.drag(layout.dividers[0].id, [u32::MAX, 20], &layout));
    tree.toggle_zoom();

    let preview = tree.split_preview(Axis::Horizontal, BOUNDS, cell);
    assert_eq!(tree.focused(), 2);
    tree.layout(BOUNDS, cell, &mut layout);
    assert_eq!(layout.panes, [(2, BOUNDS)]);
    assert_eq!(layout.hidden, [(1, rect(10, 20, 108, 81))]);

    tree.split(Axis::Horizontal, 3);
    tree.layout(BOUNDS, cell, &mut layout);
    assert_eq!(preview, rect(119, 20, 12, 81));
    assert_eq!(layout.panes[2], (3, preview));

    tree.toggle_zoom();
    tree.close(1);
    tree.layout(BOUNDS, cell, &mut layout);
    assert_eq!(layout.panes, [(3, BOUNDS)]);
    tree.close(3);
    tree.layout(BOUNDS, cell, &mut layout);
    assert_eq!(layout.panes, [(2, BOUNDS)]);
}
