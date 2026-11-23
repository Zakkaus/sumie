use crate::{CellPosition, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy)]
pub enum MousePhase {
    Press(Button),
    Release(Button),
    Motion,
    /// Signed Wayland value120 units: positive scrolls down.
    Wheel(f64),
    Cancel,
}

#[derive(Clone, Copy)]
pub struct MouseEvent {
    pub phase: MousePhase,
    pub cell: CellPosition,
    pub pixel: [f32; 2],
    pub modifiers: KeyModifiers,
}

#[derive(Clone, Copy)]
pub struct MouseModes {
    pub tracking: bool,
    pub alternate: bool,
    pub alternate_scroll: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectStep {
    Begin,
    Drag,
    Finish,
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelRoute {
    Report,
    Arrows,
    History,
}

#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub enum Dispatch {
    Ignore,
    Select(SelectStep),
    Press(Button),
    Release(Button),
    Motion(Option<Button>),
    Wheel(WheelRoute, i32),
}

#[derive(Default)]
struct Wheel {
    route: Option<WheelRoute>,
    remainder: f64,
}

impl Wheel {
    fn steps(&mut self, delta: f64, route: WheelRoute) -> i32 {
        if self.route != Some(route) {
            self.route = Some(route);
            self.remainder = 0.0;
        }
        let unit = if route == WheelRoute::Report {
            120.0
        } else {
            40.0
        };
        let total = self.remainder + delta;
        self.remainder = total % unit;
        #[expect(clippy::cast_possible_truncation, reason = "only emit whole steps")]
        let steps = (total / unit) as i32;
        steps
    }
}

#[derive(Default)]
pub struct MouseState {
    selecting: bool,
    // Only buttons whose press was routed to the application belong in motion reports.
    held: [Option<Button>; 3],
    wheel: Wheel,
}

impl MouseState {
    pub fn cancel_selection(&mut self) {
        self.selecting = false;
    }

    /// Reset local gestures without forgetting buttons already reported to the terminal.
    pub fn reset_gestures(&mut self) {
        self.cancel_selection();
        self.wheel = Wheel::default();
    }

    /// Track a `Dispatch::Press` after its report has been queued to the PTY.
    pub fn press_reported(&mut self, button: Button) {
        self.held.rotate_right(1);
        self.held[0] = Some(button);
    }

    #[must_use]
    pub fn any_button(&self) -> bool {
        self.held[0].is_some()
    }

    #[must_use]
    pub fn dispatch(&mut self, event: MouseEvent, modes: MouseModes) -> Dispatch {
        match event.phase {
            MousePhase::Wheel(delta) => {
                let route = if modes.tracking {
                    WheelRoute::Report
                } else if modes.alternate && modes.alternate_scroll {
                    WheelRoute::Arrows
                } else {
                    WheelRoute::History
                };
                let steps = self.wheel.steps(delta, route);
                if steps == 0 {
                    Dispatch::Ignore
                } else {
                    Dispatch::Wheel(route, steps)
                }
            }
            MousePhase::Press(button) => {
                self.press(button, modes.tracking && !event.modifiers.shift)
            }
            MousePhase::Release(Button::Left) if self.selecting => {
                self.selecting = false;
                Dispatch::Select(SelectStep::Finish)
            }
            MousePhase::Release(button) => {
                if let Some(i) = self.held.iter().position(|&b| b == Some(button)) {
                    self.held[i..].rotate_left(1);
                    self.held[2] = None;
                    Dispatch::Release(button)
                } else {
                    Dispatch::Ignore
                }
            }
            MousePhase::Motion if self.selecting => Dispatch::Select(SelectStep::Drag),
            // Shift only chooses the route before a drag starts.
            MousePhase::Motion
                if modes.tracking && (self.any_button() || !event.modifiers.shift) =>
            {
                Dispatch::Motion(self.held[0])
            }
            MousePhase::Motion => Dispatch::Ignore,
            MousePhase::Cancel => {
                let selecting = self.selecting;
                *self = Self::default();
                if selecting {
                    Dispatch::Select(SelectStep::Clear)
                } else {
                    Dispatch::Ignore
                }
            }
        }
    }

    fn press(&mut self, button: Button, report: bool) -> Dispatch {
        if report {
            if self.held.contains(&Some(button)) {
                Dispatch::Ignore
            } else {
                Dispatch::Press(button)
            }
        } else if button == Button::Left {
            self.selecting = true;
            Dispatch::Select(SelectStep::Begin)
        } else {
            Dispatch::Ignore
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                let mut state = MouseState::default();
                let button = Button::Left;
                let (hover, press, drag, release) = if tracking && !shift {
                    (
                        Dispatch::Motion(None),
                        Dispatch::Press(button),
                        Dispatch::Motion(Some(button)),
                        Dispatch::Release(button),
                    )
                } else {
                    (
                        Dispatch::Ignore,
                        Dispatch::Select(SelectStep::Begin),
                        Dispatch::Select(SelectStep::Drag),
                        Dispatch::Select(SelectStep::Finish),
                    )
                };
                for (phase, column, expected) in [
                    (
                        MousePhase::Wheel(120.0),
                        0,
                        Dispatch::Wheel(wheel, if tracking { 1 } else { 3 }),
                    ),
                    (MousePhase::Motion, 0, hover),
                    (MousePhase::Press(button), 0, press),
                    (MousePhase::Motion, 1, drag),
                    (MousePhase::Release(button), 1, release),
                ] {
                    let event = MouseEvent {
                        phase,
                        cell: CellPosition { row: 0, column },
                        pixel: [0.0; 2],
                        modifiers: KeyModifiers {
                            shift,
                            ..KeyModifiers::default()
                        },
                    };
                    let dispatch = state.dispatch(event, modes);
                    assert_eq!(dispatch, expected, "tracking={tracking}, shift={shift}");
                    if let Dispatch::Press(button) = dispatch {
                        state.press_reported(button);
                    }
                }
            }
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
}
