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
pub enum Dispatch {
    Ignore,
    Select(SelectStep),
    Click(MouseEvent),
    Press(Button),
    Release(Button),
    Motion(Option<Button>),
    Wheel(WheelRoute, i32),
}

#[derive(Default)]
enum Left {
    #[default]
    Idle,
    Pending(MouseEvent),
    Selecting,
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
    left: Left,
    // Only buttons whose press was routed to the application belong in motion reports.
    held: [Option<Button>; 3],
    wheel: Wheel,
}

impl MouseState {
    pub fn cancel_selection(&mut self) {
        self.left = Left::Idle;
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
            MousePhase::Press(button) => self.press(button, event, modes.tracking),
            MousePhase::Release(Button::Left) if !matches!(self.left, Left::Idle) => {
                match std::mem::take(&mut self.left) {
                    Left::Pending(press) if press.cell == event.cell => Dispatch::Click(press),
                    _ => Dispatch::Select(SelectStep::Finish),
                }
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
            MousePhase::Motion => self.motion(event.cell, modes.tracking),
            MousePhase::Cancel => {
                let selecting = !matches!(self.left, Left::Idle);
                *self = Self::default();
                if selecting {
                    Dispatch::Select(SelectStep::Clear)
                } else {
                    Dispatch::Ignore
                }
            }
        }
    }

    fn press(&mut self, button: Button, event: MouseEvent, tracking: bool) -> Dispatch {
        let pass = event.modifiers.alt && !event.modifiers.control;
        if button == Button::Left && !(tracking && pass) {
            self.left = if tracking && !(event.modifiers.alt && event.modifiers.control) {
                Left::Pending(event)
            } else {
                Left::Selecting
            };
            return Dispatch::Select(SelectStep::Begin);
        }
        if tracking && !self.held.contains(&Some(button)) {
            Dispatch::Press(button)
        } else {
            Dispatch::Ignore
        }
    }

    fn motion(&mut self, cell: CellPosition, tracking: bool) -> Dispatch {
        match self.left {
            Left::Pending(press) if press.cell == cell => Dispatch::Ignore,
            Left::Pending(_) | Left::Selecting => {
                self.left = Left::Selecting;
                Dispatch::Select(SelectStep::Drag)
            }
            Left::Idle if tracking => Dispatch::Motion(self.held[0]),
            Left::Idle => Dispatch::Ignore,
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
            for (alt, control) in [(false, false), (true, false), (true, true)] {
                let mut state = MouseState::default();
                let mut event = MouseEvent {
                    phase: MousePhase::Wheel(120.0),
                    cell: CellPosition { row: 0, column: 0 },
                    pixel: [0.0; 2],
                    modifiers: KeyModifiers {
                        control,
                        alt,
                        ..KeyModifiers::default()
                    },
                };
                assert!(
                    matches!(state.dispatch(event, modes), Dispatch::Wheel(r, _) if r == wheel)
                );
                let mut fire = |phase, col| {
                    event.phase = phase;
                    event.cell.column = col;
                    let dispatch = state.dispatch(event, modes);
                    if let Dispatch::Press(button) = dispatch {
                        state.press_reported(button);
                    }
                    dispatch
                };
                let pass = tracking && alt && !control;
                let press = MousePhase::Press(Button::Left);
                let release = MousePhase::Release(Button::Left);
                assert_eq!(
                    matches!(fire(press, 0), Dispatch::Press(Button::Left)),
                    pass
                );
                assert_eq!(
                    matches!(fire(release, 0), Dispatch::Click(_)),
                    tracking && !alt
                );
                let _ = fire(press, 0);
                let motion = fire(MousePhase::Motion, 1);
                assert!(matches!(
                    (pass, motion),
                    (true, Dispatch::Motion(Some(Button::Left)))
                        | (false, Dispatch::Select(SelectStep::Drag))
                ));
                assert_eq!(
                    matches!(fire(release, 1), Dispatch::Select(SelectStep::Finish)),
                    !pass
                );
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
