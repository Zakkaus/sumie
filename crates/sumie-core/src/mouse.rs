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
    /// Whether this press hit a link the desktop opener can handle.
    pub link: bool,
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
    OpenLink,
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
    last_click: Option<(u32, CellPosition, KeyModifiers, SelectionUnit)>,
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

    pub fn reset_clicks(&mut self) {
        self.last_click = None;
    }

    pub fn modifiers_changed(&mut self, modifiers: KeyModifiers) {
        if self
            .last_click
            .is_some_and(|(_, _, previous, _)| previous != modifiers)
        {
            self.reset_clicks();
        }
    }

    /// Reset local gestures without forgetting buttons already reported to the terminal.
    pub fn reset_gestures(&mut self) {
        self.selecting = None;
        self.reset_clicks();
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
        self.modifiers_changed(event.modifiers);
        if self
            .last_click
            .is_some_and(|(_, cell, _, _)| cell != event.cell)
        {
            self.reset_clicks();
        }
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
            MousePhase::Press(Button::Left, _) if event.modifiers.control && event.link => {
                self.reset_clicks();
                Dispatch::OpenLink
            }
            MousePhase::Press(button, time) => self.press(
                button,
                modes.tracking && !event.modifiers.shift,
                time,
                event.cell,
                event.modifiers,
            ),
            MousePhase::Release(button) => self.release(button),
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

    fn release(&mut self, button: Button) -> Dispatch {
        if button == Button::Left
            && let Some((unit, _)) = self.selecting.take()
        {
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

    fn press(
        &mut self,
        button: Button,
        report: bool,
        time: u32,
        cell: CellPosition,
        modifiers: KeyModifiers,
    ) -> Dispatch {
        let last = self.last_click.take();
        if report {
            if self.held.contains(&Some(button)) {
                Dispatch::Ignore
            } else {
                Dispatch::Press(button)
            }
        } else if button == Button::Left {
            let unit =
                match last.filter(|&(at, _, _, _)| time.wrapping_sub(at) <= self.click_interval) {
                    Some((_, _, _, SelectionUnit::Cell)) => SelectionUnit::Word,
                    Some((_, _, _, SelectionUnit::Word)) => SelectionUnit::Line,
                    _ => SelectionUnit::Cell,
                };
            self.last_click = Some((time, cell, modifiers, unit));
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
        self.selecting = Some((unit, cell));
        Dispatch::Select(SelectStep::Drag(unit))
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
                assert_gestures(modes, wheel, shift);
            }
        }
    }

    fn assert_gestures(modes: MouseModes, wheel: WheelRoute, shift: bool) {
        use MousePhase::{Motion, Press, Release};
        use SelectStep::{Begin, Drag, Finish};
        use SelectionUnit::{Cell, Line, Word};

        let mut state = MouseState::default();
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
            let release = route(Finish(unit), Dispatch::Release(button));
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
