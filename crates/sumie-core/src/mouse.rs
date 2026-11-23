use crate::{CellPosition, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy)]
pub enum MousePhase {
    /// The platform press timestamp in milliseconds (wrapping).
    Press(Button, u32),
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
pub enum SelectionUnit {
    Cell,
    Word,
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectStep {
    Begin(SelectionUnit),
    Drag(SelectionUnit),
    Finish(SelectionUnit),
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

pub struct MouseState {
    selecting: Option<(SelectionUnit, CellPosition)>,
    last_click: Option<(u32, CellPosition, SelectionUnit)>,
    click_interval: u32,
    // Only buttons whose press was routed to the application belong in motion reports.
    held: [Option<Button>; 3],
    wheel: Wheel,
}

impl Default for MouseState {
    fn default() -> Self {
        Self {
            selecting: None,
            last_click: None,
            click_interval: 500,
            held: [None; 3],
            wheel: Wheel::default(),
        }
    }
}

impl MouseState {
    pub fn set_click_interval(&mut self, millis: u32) {
        self.click_interval = millis;
    }

    pub fn cancel_selection(&mut self) {
        self.selecting = None;
        self.last_click = None;
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
                self.last_click = None;
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
            MousePhase::Press(button, time) => self.press(
                button,
                modes.tracking && !event.modifiers.shift,
                time,
                event.cell,
            ),
            MousePhase::Release(button) => self.release(button, event.cell),
            MousePhase::Motion if self.selecting.is_some() => self.drag(event.cell),
            // Shift only chooses the route before a drag starts.
            MousePhase::Motion
                if modes.tracking && (self.any_button() || !event.modifiers.shift) =>
            {
                Dispatch::Motion(self.held[0])
            }
            MousePhase::Motion => Dispatch::Ignore,
            MousePhase::Cancel => {
                let selecting = self.selecting.is_some();
                self.reset_gestures();
                self.held = [None; 3];
                if selecting {
                    Dispatch::Select(SelectStep::Clear)
                } else {
                    Dispatch::Ignore
                }
            }
        }
    }

    fn release(&mut self, button: Button, cell: CellPosition) -> Dispatch {
        if button == Button::Left
            && let Some((unit, previous)) = self.selecting.take()
        {
            if previous != cell {
                self.last_click = None;
            }
            return Dispatch::Select(SelectStep::Finish(unit));
        }
        if let Some(i) = self.held.iter().position(|&b| b == Some(button)) {
            self.held[i..].rotate_left(1);
            self.held[2] = None;
            Dispatch::Release(button)
        } else {
            Dispatch::Ignore
        }
    }

    fn press(&mut self, button: Button, report: bool, time: u32, cell: CellPosition) -> Dispatch {
        let last = self.last_click.take();
        if report {
            if self.held.contains(&Some(button)) {
                Dispatch::Ignore
            } else {
                Dispatch::Press(button)
            }
        } else if button == Button::Left {
            let unit = match last
                .filter(|&(at, pos, _)| pos == cell && time.wrapping_sub(at) <= self.click_interval)
            {
                Some((_, _, SelectionUnit::Cell)) => SelectionUnit::Word,
                Some((_, _, SelectionUnit::Word)) => SelectionUnit::Line,
                _ => SelectionUnit::Cell,
            };
            self.last_click = Some((time, cell, unit));
            self.selecting = Some((unit, cell));
            Dispatch::Select(SelectStep::Begin(unit))
        } else {
            Dispatch::Ignore
        }
    }

    fn drag(&mut self, cell: CellPosition) -> Dispatch {
        let (unit, previous) = self.selecting.unwrap();
        if cell == previous {
            return Dispatch::Ignore;
        }
        self.last_click = None;
        self.selecting = Some((unit, cell));
        Dispatch::Select(SelectStep::Drag(unit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_mouse_by_mode_and_gesture() {
        use MousePhase::{Motion, Press, Release};
        use SelectStep::{Begin, Drag, Finish};
        use SelectionUnit::{Cell, Line, Word};
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
                let tracking_input = tracking && !shift;
                state.set_click_interval(150);
                let route = |selection, report| {
                    if tracking_input {
                        report
                    } else {
                        Dispatch::Select(selection)
                    }
                };
                let mut send = |phase, column, expected| {
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
                };
                let steps = if tracking { 1 } else { 3 };
                let hover = if tracking_input {
                    Dispatch::Motion(None)
                } else {
                    Dispatch::Ignore
                };
                send(MousePhase::Wheel(120.0), 0, Dispatch::Wheel(wheel, steps));
                send(Motion, 0, hover);
                for (time, column, unit, drag_to) in [
                    (0, 0, Cell, None),
                    (100, 0, Word, None),
                    (200, 0, Line, Some(1)),
                    (300, 0, Cell, None),
                    (400, 0, Word, Some(1)),
                    (450, 1, Cell, Some(2)),
                    (500, 0, Cell, None),
                    (600, 1, Cell, None),
                    (751, 1, Cell, None),
                ] {
                    let press = route(Begin(unit), Dispatch::Press(button));
                    let drag = route(Drag(unit), Dispatch::Motion(Some(button)));
                    let release = route(Finish(unit), Dispatch::Release(button));
                    send(Press(button, time), column, press);
                    if let Some(end) = drag_to {
                        send(Motion, end, drag);
                    }
                    send(Release(button), drag_to.unwrap_or(column), release);
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
