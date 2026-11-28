use super::*;

#[test]
fn middle_click_uses_primary_only_without_tracking() {
    let mut state = MouseState::new(Platform::Linux);
    let press = MouseEvent {
        phase: MousePhase::Press(Button::Middle, 0),
        cell: CellPosition { row: 0, column: 0 },
        pixel: [0.0; 2],
        over_grid: true,
        modifiers: KeyModifiers::default(),
        link: false,
    };
    let release = MouseEvent {
        phase: MousePhase::Release(Button::Middle),
        ..press
    };
    for available in [false, true] {
        state.set_primary_selection(available);
        for tracking in [false, true] {
            let modes = MouseModes {
                tracking,
                alternate: false,
                alternate_scroll: false,
            };
            let (on_press, on_release) = if tracking {
                (
                    Dispatch::Press(Button::Middle),
                    Dispatch::Release(Button::Middle),
                )
            } else if available {
                (Dispatch::PastePrimary, Dispatch::Ignore)
            } else {
                (Dispatch::Ignore, Dispatch::Ignore)
            };
            assert_eq!(state.dispatch(press, modes), on_press);
            if tracking {
                state.press_reported(Button::Middle);
            }
            assert_eq!(state.dispatch(release, modes), on_release);
        }
    }
}

#[test]
fn routes_mouse_by_mode_and_gesture() {
    use WheelRoute::{Arrows, History, Report};
    for (tracking, alternate, alternate_scroll, wheel) in [
        (false, false, false, History),
        (false, false, true, History),
        (false, true, false, History),
        (false, true, true, Arrows),
        (true, false, false, Report),
        (true, false, true, Report),
        (true, true, false, Report),
        (true, true, true, Report),
    ] {
        let modes = MouseModes {
            tracking,
            alternate,
            alternate_scroll,
        };
        for shift in [false, true] {
            assert_gestures(modes, wheel, shift);
        }
    }
}

fn assert_gestures(modes: MouseModes, wheel: WheelRoute, shift: bool) {
    use MousePhase::{Motion, Press, Release};
    use SelectStep::{Begin, Drag, Finish};
    use SelectionUnit::{Cell, Line, Word};

    let mut state = MouseState::new(Platform::Linux);
    state.set_click_interval(150);
    let button = Button::Left;
    let tracking_input = modes.tracking && !shift;
    let route = |selection, report| {
        if tracking_input {
            report
        } else {
            Dispatch::Select(selection)
        }
    };
    let mut send = |phase, column, modifiers, expected| {
        let event = MouseEvent {
            phase,
            cell: CellPosition { row: 0, column },
            pixel: [0.0; 2],
            over_grid: true,
            modifiers,
            link: false,
        };
        let dispatch = state.dispatch(event, modes);
        assert_eq!(dispatch, expected, "column={column}, shift={shift}");
        if let Dispatch::Press(button) = dispatch {
            state.press_reported(button);
        }
    };
    let steps = if modes.tracking { 1 } else { 3 };
    let hover = if tracking_input {
        Dispatch::Motion(None)
    } else {
        Dispatch::Ignore
    };
    let plain = KeyModifiers {
        shift,
        ..KeyModifiers::default()
    };
    let wheel_dispatch = Dispatch::Wheel(wheel, steps);
    send(MousePhase::Wheel(120.0), 0, plain, wheel_dispatch);
    send(Motion, 0, plain, hover);
    for (time, column, unit, drag_to, hover_via, control) in [
        (0, 0, Cell, None, None, false),
        (100, 0, Word, None, None, false),
        (200, 0, Line, Some(1), None, false),
        (300, 0, Cell, None, None, false),
        (400, 0, Word, Some(1), None, false),
        (450, 1, Cell, Some(2), None, false),
        (500, 0, Cell, None, None, false),
        (600, 1, Cell, None, None, false),
        (751, 1, Cell, None, None, false),
        (800, 1, Cell, None, Some(0), false),
        (850, 1, Cell, None, None, true),
    ] {
        let mut modifiers = KeyModifiers { control, ..plain };
        if let Some(via) = hover_via {
            send(Motion, via, modifiers, hover);
            send(Motion, column, modifiers, hover);
        }
        let press = route(Begin(unit), Dispatch::Press(button));
        let release = route(Finish { unit, copy: true }, Dispatch::Release(button));
        send(Press(button, time), column, modifiers, press);
        if let Some(end) = drag_to {
            modifiers.shift = false;
            let drag = route(Drag(unit), Dispatch::Motion(Some(button)));
            send(Motion, end, modifiers, drag);
        }
        let end = drag_to.unwrap_or(column);
        send(Release(button), end, modifiers, release);
    }
}

#[test]
fn viewer_consuming_release_cancels_the_selection_drag() {
    let mut state = MouseState::new(Platform::Linux);
    let modes = MouseModes {
        tracking: false,
        alternate: false,
        alternate_scroll: false,
    };
    let event = |phase, column| MouseEvent {
        phase,
        cell: CellPosition { row: 0, column },
        pixel: [0.0; 2],
        over_grid: true,
        modifiers: KeyModifiers::default(),
        link: false,
    };
    assert!(matches!(
        state.dispatch(event(MousePhase::Press(Button::Left, 0), 0), modes),
        Dispatch::Select(SelectStep::Begin(SelectionUnit::Cell))
    ));
    state.reset_gestures();
    assert_eq!(
        state.dispatch(event(MousePhase::Motion, 1), modes),
        Dispatch::Ignore
    );
}

fn mac_event(phase: MousePhase, column: u16, modifiers: KeyModifiers) -> MouseEvent {
    MouseEvent {
        phase,
        cell: CellPosition { row: 0, column },
        pixel: [0.0; 2],
        over_grid: true,
        modifiers,
        link: false,
    }
}

#[test]
fn mac_drag_selects_without_copying_and_tracking_tells_a_click_from_a_drag() {
    use MousePhase::{Motion, Press, Release};
    let (left, right) = (Button::Left, Button::Right);
    let plain = KeyModifiers::default();
    let option = KeyModifiers { alt: true, ..plain };
    let begin = Dispatch::Select(SelectStep::Begin(SelectionUnit::Cell));
    let drag = Dispatch::Select(SelectStep::Drag(SelectionUnit::Cell));
    let finish = Dispatch::Select(SelectStep::Finish {
        unit: SelectionUnit::Cell,
        copy: false,
    });
    let from = Dispatch::Select(SelectStep::DragFrom(CellPosition { row: 0, column: 3 }));
    let mut state = MouseState::new(Platform::MacOS);
    for (step, (phase, column, modifiers, tracking, expected)) in [
        // Without tracking a drag selects, and its release does not copy.
        (Press(left, 0), 0, plain, false, begin),
        (Motion, 2, plain, false, drag),
        (Release(left), 2, plain, false, finish),
        // Under tracking a press waits; released in its cell it is one click.
        (Press(left, 1000), 3, plain, true, Dispatch::Ignore),
        (Motion, 3, plain, true, Dispatch::Ignore),
        (
            Release(left),
            3,
            plain,
            true,
            Dispatch::Click(mac_event(Press(left, 1000), 3, plain)),
        ),
        // Leaving the cell makes it a selection from the press cell; nothing is reported.
        (Press(left, 2000), 3, plain, true, Dispatch::Ignore),
        (Motion, 5, plain, true, from),
        (Motion, 6, plain, true, drag),
        (Release(left), 6, plain, true, finish),
        // Option hands the drag to the program.
        (Press(left, 3000), 3, option, true, Dispatch::Press(left)),
        (Motion, 5, option, true, Dispatch::Motion(Some(left))),
        (Release(left), 5, plain, true, Dispatch::Release(left)),
        // Other buttons and hover reach the program at once.
        (Motion, 7, plain, true, Dispatch::Motion(None)),
        (Press(right, 4000), 7, plain, true, Dispatch::Press(right)),
        (Release(right), 7, plain, true, Dispatch::Release(right)),
    ]
    .into_iter()
    .enumerate()
    {
        let modes = MouseModes {
            tracking,
            alternate: false,
            alternate_scroll: false,
        };
        let dispatch = state.dispatch(mac_event(phase, column, modifiers), modes);
        assert_eq!(dispatch, expected, "step {step}");
        if let Dispatch::Press(button) = dispatch {
            state.press_reported(button);
        }
    }
    // Option is not part of the program's reports.
    assert_eq!(state.report_modifiers(option), plain);
    assert_eq!(
        MouseState::new(Platform::Linux).report_modifiers(option),
        option
    );
}

fn held_back_steps(steps: &[(MouseEvent, Dispatch)]) {
    let modes = MouseModes {
        tracking: true,
        alternate: false,
        alternate_scroll: false,
    };
    let mut state = MouseState::new(Platform::MacOS);
    for (step, &(event, expected)) in steps.iter().enumerate() {
        let dispatch = state.dispatch(event, modes);
        assert_eq!(dispatch, expected, "step {step}");
        if let Dispatch::Press(button) = dispatch {
            state.press_reported(button);
        }
    }
}

fn mac_at(phase: MousePhase, pixel: f32, over_grid: bool) -> MouseEvent {
    MouseEvent {
        pixel: [pixel, 0.0],
        over_grid,
        ..mac_event(phase, 9, KeyModifiers::default())
    }
}

#[test]
fn a_held_back_press_is_a_click_only_inside_its_cell() {
    use MousePhase::{Motion, Press, Release};
    let press = mac_at(Press(Button::Left, 0), 95.0, true);
    let from = Dispatch::Select(SelectStep::DragFrom(CellPosition { row: 0, column: 9 }));
    let finish = Dispatch::Select(SelectStep::Finish {
        unit: SelectionUnit::Cell,
        copy: false,
    });
    held_back_steps(&[
        // The click reports the press where it landed, not where it was released.
        (press, Dispatch::Ignore),
        (
            mac_at(Release(Button::Left), 98.0, true),
            Dispatch::Click(press),
        ),
        // The padding past the last column clamps to it, but is outside the cell.
        (press, Dispatch::Ignore),
        (mac_at(Motion, 104.0, false), from),
        (mac_at(Release(Button::Left), 104.0, false), finish),
        (press, Dispatch::Ignore),
        (
            mac_at(Release(Button::Left), 104.0, false),
            Dispatch::Ignore,
        ),
    ]);
}

#[test]
fn another_button_drops_a_held_back_press() {
    use MousePhase::{Press, Release};
    let (left, right) = (Button::Left, Button::Right);
    // Right pressed and released while left is held: no left click follows them.
    held_back_steps(&[
        (mac_at(Press(left, 0), 95.0, true), Dispatch::Ignore),
        (mac_at(Press(right, 10), 95.0, true), Dispatch::Press(right)),
        (mac_at(Release(right), 95.0, true), Dispatch::Release(right)),
        (mac_at(Release(left), 95.0, true), Dispatch::Ignore),
    ]);
}

#[test]
fn a_mac_link_opens_where_its_press_moved_on_it_but_linux_needs_a_still_press() {
    use MousePhase::{Motion, Press, Release};
    let modes = MouseModes {
        tracking: false,
        alternate: false,
        alternate_scroll: false,
    };
    for (platform, modifiers, opens) in [
        (
            Platform::MacOS,
            KeyModifiers {
                super_key: true,
                ..KeyModifiers::default()
            },
            true,
        ),
        (
            Platform::Linux,
            KeyModifiers {
                control: true,
                ..KeyModifiers::default()
            },
            false,
        ),
    ] {
        let mut state = MouseState::new(platform);
        let mut send = |phase, column, pixel: f32| {
            let event = MouseEvent {
                pixel: [pixel, 0.0],
                link: true,
                ..mac_event(phase, column, modifiers)
            };
            state.dispatch(event, modes)
        };
        // A move within the press cell.
        send(Press(Button::Left, 0), 1, 10.0);
        send(Motion, 1, 12.0);
        let open = send(Release(Button::Left), 1, 12.0) == Dispatch::OpenLink;
        assert_eq!(open, opens, "{platform:?} within the cell");
        // A move to another cell of the same link: Linux selects from the press.
        send(Press(Button::Left, 1000), 1, 10.0);
        let moved = send(Motion, 2, 20.0);
        assert_eq!(
            moved == Dispatch::Ignore,
            opens,
            "{platform:?} onto the next cell"
        );
        let open = send(Release(Button::Left), 2, 20.0) == Dispatch::OpenLink;
        assert_eq!(open, opens, "{platform:?} on the next cell");
    }
}

#[test]
fn mac_links_take_command_and_a_drag_off_cancels_the_open() {
    use MousePhase::{Motion, Press, Release};
    let command = KeyModifiers {
        super_key: true,
        ..KeyModifiers::default()
    };
    let control = KeyModifiers {
        control: true,
        ..KeyModifiers::default()
    };
    for tracking in [false, true] {
        let modes = MouseModes {
            tracking,
            alternate: false,
            alternate_scroll: false,
        };
        let mut state = MouseState::new(Platform::MacOS);
        let mut send = |phase, column, modifiers, link| {
            let event = MouseEvent {
                link,
                ..mac_event(phase, column, modifiers)
            };
            state.dispatch(event, modes)
        };
        assert_eq!(
            send(Press(Button::Left, 0), 1, command, true),
            Dispatch::Ignore
        );
        let open = send(Release(Button::Left), 1, command, true);
        assert_eq!(open, Dispatch::OpenLink);
        // Dragging off the link selects from it and the release opens nothing.
        send(Press(Button::Left, 1000), 1, command, true);
        let from = Dispatch::Select(SelectStep::DragFrom(CellPosition { row: 0, column: 1 }));
        assert_eq!(send(Motion, 3, command, false), from);
        let release = send(Release(Button::Left), 3, command, false);
        assert!(matches!(
            release,
            Dispatch::Select(SelectStep::Finish { copy: false, .. })
        ));
        // Ctrl is not the macOS link modifier.
        send(Press(Button::Left, 5000), 1, control, true);
        let release = send(Release(Button::Left), 1, control, true);
        assert_ne!(release, Dispatch::OpenLink, "tracking={tracking}");
    }
}

#[test]
fn accumulates_value120() {
    use WheelRoute::{History, Report};
    let mut wheel = Wheel::default();
    for (route, delta, expected) in [
        (History, -15., 0),
        (History, -30., -1),
        (History, 85., 2),
        (History, 39., 0),
        (Report, 81., 0),
        (Report, 159., 2),
    ] {
        assert_eq!(wheel.steps(delta, route), expected);
    }
}
