use sumie_core::splits::{Axis, Rect, SplitTree};

use super::{Button, Layout, LayoutKey, Phase, Pointer, Target};

const SHOWN: LayoutKey = (1, 0);
const PENDING: LayoutKey = (1, 1);

fn pointer() -> Pointer {
    Pointer {
        presented: Some(SHOWN),
        ..Pointer::default()
    }
}

#[test]
fn hits_are_absolute_and_wait_for_zoom_presentation() {
    let mut tree = SplitTree::new(1);
    tree.split(Axis::Horizontal, 2);
    let mut layout = Layout::default();
    let bounds = Rect {
        x: 0,
        y: 20,
        width: 81,
        height: 80,
    };
    tree.layout(bounds, [1; 2], [0; 2], &mut layout);
    let divider = layout.dividers[0].id;
    for (pixel, target) in [
        ([-0.1, 60.0], None),
        ([0.0, 19.9], None),
        ([39.9, 20.0], Some(Target::Pane(1))),
        ([40.0, 20.0], Some(Target::Divider(divider))),
        ([40.9, 99.9], Some(Target::Divider(divider))),
        ([41.0, 20.0], Some(Target::Pane(2))),
        ([81.0, 99.9], None),
        ([40.0, 100.0], None),
    ] {
        assert_eq!(Target::at(&layout, pixel, [81, 100]), target);
    }

    let press = Phase::Press(Button::Left, 0);
    let mut pointer = pointer();
    tree.toggle_zoom();
    tree.layout(bounds, [1; 2], [0; 2], &mut layout);
    let hit = || Target::at(&layout, [39.9, 20.0], [81, 100]);
    assert_eq!(pointer.route(press, PENDING, hit), None);
    assert!(pointer.take_frame_recovery());
    pointer.presented = Some(PENDING);
    assert_eq!(pointer.route(press, PENDING, hit), Some(Target::Pane(2)));
}

#[test]
fn drag_and_release_stay_with_the_pressed_pane_but_wheel_does_not() {
    let mut pointer = pointer();
    let left = Target::Pane(1);
    let right = Target::Pane(2);
    let over_right = || Some(right);
    assert!(pointer.press(Button::Left, left, false));
    assert_eq!(pointer.route(Phase::Motion, SHOWN, over_right), Some(left));
    assert_eq!(
        pointer.route(Phase::Wheel(120.0), SHOWN, over_right),
        Some(right)
    );
    assert_eq!(
        pointer.route(Phase::Release(Button::Left), SHOWN, over_right),
        Some(left)
    );
    assert_eq!(pointer.route(Phase::Motion, SHOWN, over_right), Some(right));
}

#[test]
fn a_left_click_that_moves_focus_stays_out_of_the_pane() {
    let pane = Target::Pane(2);
    let over_pane = || Some(pane);
    let left = Phase::Release(Button::Left);
    let right = Phase::Release(Button::Right);
    let mut pointer = pointer();
    // Focusing left click: neither press nor release reaches the pane; the drag does.
    assert!(!pointer.press(Button::Left, pane, true));
    assert_eq!(pointer.route(Phase::Motion, SHOWN, over_pane), Some(pane));
    assert!(pointer.press(Button::Right, pane, false));
    assert_eq!(pointer.route(right, SHOWN, over_pane), Some(pane));
    assert_eq!(pointer.route(left, SHOWN, over_pane), None);
    // The next click in the focused pane is delivered whole.
    assert!(pointer.press(Button::Left, pane, false));
    assert_eq!(pointer.route(left, SHOWN, over_pane), Some(pane));
    // Other buttons focus and still reach the pane.
    assert!(pointer.press(Button::Right, pane, true));
    assert_eq!(pointer.route(right, SHOWN, over_pane), Some(pane));
    // A cancelled focusing click leaves no swallowed release behind.
    assert!(!pointer.press(Button::Left, pane, true));
    pointer.route(Phase::Cancel, SHOWN, over_pane);
    assert!(pointer.press(Button::Left, pane, false));
    assert_eq!(pointer.route(left, SHOWN, over_pane), Some(pane));
}

#[test]
fn divider_drag_requires_a_left_press_and_ends_on_focus_change() {
    let mut pointer = pointer();
    let divider = Target::Divider(0);
    let over_divider = || Some(divider);
    let over_pane = || Some(Target::Pane(1));
    let left_press = Phase::Press(Button::Left, 0);
    let right_press = Phase::Press(Button::Right, 0);
    let release = Phase::Release(Button::Left);
    assert_eq!(pointer.route(Phase::Motion, SHOWN, over_divider), None);
    assert_eq!(pointer.route(right_press, SHOWN, over_divider), None);
    assert_eq!(
        pointer.route(left_press, SHOWN, over_divider),
        Some(divider)
    );
    pointer.press(Button::Left, divider, false);
    assert_eq!(pointer.cancel_stale_pane(PENDING), None);
    assert_eq!(
        pointer.route(Phase::Motion, PENDING, over_pane),
        Some(divider)
    );
    assert_eq!(pointer.route(release, PENDING, over_pane), Some(divider));

    pointer.press(Button::Left, divider, false);
    pointer.reset();
    assert_eq!(pointer.route(Phase::Motion, SHOWN, over_pane), None);
    assert_eq!(pointer.route(release, SHOWN, over_pane), None);
    assert_eq!(pointer.route(Phase::Motion, SHOWN, over_pane), over_pane());

    pointer.press(Button::Left, divider, false);
    assert_eq!(
        pointer.route(Phase::Cancel, SHOWN, over_pane),
        Some(divider)
    );
    assert_eq!(pointer.route(release, SHOWN, over_pane), None);
    assert_eq!(pointer.route(Phase::Motion, SHOWN, over_pane), over_pane());
}
