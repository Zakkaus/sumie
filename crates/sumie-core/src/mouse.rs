use crate::{CellPosition, KeyModifiers, Platform};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub enum MousePhase {
    /// The platform press timestamp in milliseconds (wrapping).
    Press(Button, u32),
    Release(Button),
    Motion,
    /// Signed Wayland value120 units: positive scrolls down.
    Wheel(f64),
    /// Touchpad finger movement, in the same units as `Wheel`.
    Touch(f64),
    /// The finger lifted; the content may slide on.
    TouchEnd,
    Cancel,
}

#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct MouseEvent {
    pub phase: MousePhase,
    pub cell: CellPosition,
    pub pixel: [f32; 2],
    /// Whether the pointer actually hit a visible cell rather than clamping to one.
    pub over_grid: bool,
    pub modifiers: KeyModifiers,
    /// Whether this link-modifier left press found a link or this release hit the captured
    /// target.
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
    /// A cell selection that starts where an earlier press landed: a link press or, on macOS,
    /// a left press held back from mouse tracking.
    DragFrom(CellPosition),
    Drag(SelectionUnit),
    /// The drag ended; `copy` is the platform's choice to copy it now.
    Finish {
        unit: SelectionUnit,
        copy: bool,
    },
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelRoute {
    Report,
    Arrows,
    History,
}

#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub enum Dispatch {
    Ignore,
    Select(SelectStep),
    OpenLink,
    PastePrimary,
    Press(Button),
    Release(Button),
    /// A held-back left press, carried here, ended in its cell: report it and the release
    /// together.
    Click(MouseEvent),
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
    platform: Platform,
    primary_selection: bool,
    selecting: Option<(SelectionUnit, CellPosition)>,
    link_press: Option<(CellPosition, Option<[u32; 2]>)>,
    /// macOS under mouse tracking: a left press not yet reported. Leaving its cell makes it a
    /// selection; releasing in it makes it a click; another button drops it.
    held_back: Option<MouseEvent>,
    last_click: Option<(u32, CellPosition, KeyModifiers, SelectionUnit)>,
    click_interval: u32,
    // Only buttons whose press was routed to the application belong in motion reports.
    held: [Option<Button>; 3],
    wheel: Wheel,
}

impl MouseState {
    #[must_use]
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            primary_selection: false,
            selecting: None,
            link_press: None,
            held_back: None,
            last_click: None,
            click_interval: 500,
            held: [None; 3],
            wheel: Wheel::default(),
        }
    }

    pub fn set_click_interval(&mut self, millis: u32) {
        self.click_interval = millis;
    }

    /// Enable Linux middle-click paste when the compositor offers primary selection.
    pub fn set_primary_selection(&mut self, available: bool) {
        self.primary_selection = available;
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
        self.link_press = None;
        self.held_back = None;
        self.reset_clicks();
        self.wheel = Wheel::default();
    }

    #[must_use]
    pub fn awaiting_link_release(&self) -> bool {
        self.link_press.is_some()
    }

    /// The modifiers a button report carries. On macOS Option hands the gesture to the
    /// program, so it is not part of the report.
    #[must_use]
    pub const fn report_modifiers(&self, mut modifiers: KeyModifiers) -> KeyModifiers {
        if matches!(self.platform, Platform::MacOS) {
            modifiers.alt = false;
            modifiers.right_alt = false;
        }
        modifiers
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
            MousePhase::Wheel(delta) | MousePhase::Touch(delta) => {
                self.last_click = None;
                if let Some((_, pixel)) = &mut self.link_press {
                    *pixel = None;
                }
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
            MousePhase::Press(button, time) => self.route_press(button, time, event, modes),
            MousePhase::Release(button) => self.release(button, event),
            MousePhase::Motion
                if self.link_press.is_some()
                    || self.selecting.is_some()
                    || self.held_back.is_some() =>
            {
                self.drag(event)
            }
            // On Linux Shift only chooses the route before a drag starts.
            MousePhase::Motion
                if modes.tracking
                    && (self.any_button()
                        || self.platform == Platform::MacOS
                        || !event.modifiers.shift) =>
            {
                Dispatch::Motion(self.held[0])
            }
            MousePhase::Motion | MousePhase::TouchEnd => Dispatch::Ignore,
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

    /// A link press waits for its release. On macOS a left press under tracking waits to
    /// become a click or a selection, unless Option hands it to the program; on Linux Shift
    /// selects instead.
    fn route_press(
        &mut self,
        button: Button,
        time: u32,
        event: MouseEvent,
        modes: MouseModes,
    ) -> Dispatch {
        let left = button == Button::Left;
        // Another button drops a held-back left press: its click would follow this press.
        if !left {
            self.held_back = None;
        }
        if left && self.platform.link_modifier(event.modifiers) && event.link {
            self.reset_clicks();
            self.link_press = Some((event.cell, Some(event.pixel.map(f32::to_bits))));
            return Dispatch::Ignore;
        }
        if self.platform == Platform::Linux
            && self.primary_selection
            && button == Button::Middle
            && !modes.tracking
        {
            self.reset_clicks();
            return Dispatch::PastePrimary;
        }
        let report = match self.platform {
            Platform::Linux => modes.tracking && !event.modifiers.shift,
            Platform::MacOS
                if left && modes.tracking && !event.modifiers.alt && event.over_grid =>
            {
                if self.held.contains(&Some(button)) {
                    return Dispatch::Ignore;
                }
                self.reset_clicks();
                self.held_back = Some(event);
                return Dispatch::Ignore;
            }
            Platform::MacOS => modes.tracking,
        };
        self.press(button, time, event, report)
    }

    fn release(&mut self, button: Button, event: MouseEvent) -> Dispatch {
        if button == Button::Left {
            if let Some((origin, pixel)) = self.link_press.take() {
                let unmoved = event.cell == origin && pixel == Some(event.pixel.map(f32::to_bits));
                return if event.link && (self.platform.link_survives_motion() || unmoved) {
                    Dispatch::OpenLink
                } else {
                    Dispatch::Ignore
                };
            }
            if let Some(press) = self.held_back.take() {
                return if in_cell(event, press.cell) {
                    Dispatch::Click(press)
                } else {
                    Dispatch::Ignore
                };
            }
            if let Some((unit, _)) = self.selecting.take() {
                return Dispatch::Select(SelectStep::Finish {
                    unit,
                    copy: self.platform.copies_on_release(),
                });
            }
        }
        if let Some(i) = self.held.iter().position(|&b| b == Some(button)) {
            self.held[i..].rotate_left(1);
            self.held[2] = None;
            Dispatch::Release(button)
        } else {
            Dispatch::Ignore
        }
    }

    fn press(&mut self, button: Button, time: u32, event: MouseEvent, report: bool) -> Dispatch {
        let last = self.last_click.take();
        if report {
            if self.held.contains(&Some(button)) {
                Dispatch::Ignore
            } else {
                Dispatch::Press(button)
            }
        } else if button == Button::Left && event.over_grid {
            let unit =
                match last.filter(|&(at, _, _, _)| time.wrapping_sub(at) <= self.click_interval) {
                    Some((_, _, _, SelectionUnit::Cell)) => SelectionUnit::Word,
                    Some((_, _, _, SelectionUnit::Word)) => SelectionUnit::Line,
                    _ => SelectionUnit::Cell,
                };
            self.last_click = Some((time, event.cell, event.modifiers, unit));
            self.selecting = Some((unit, event.cell));
            Dispatch::Select(SelectStep::Begin(unit))
        } else {
            Dispatch::Ignore
        }
    }

    fn drag(&mut self, event: MouseEvent) -> Dispatch {
        let cell = event.cell;
        if let Some(press) = self.held_back {
            if in_cell(event, press.cell) {
                return Dispatch::Ignore;
            }
            self.held_back = None;
            self.selecting = Some((SelectionUnit::Cell, cell));
            return Dispatch::Select(SelectStep::DragFrom(press.cell));
        }
        if let Some((origin, pixel)) = &mut self.link_press {
            let on_link = if self.platform.link_survives_motion() {
                event.link
            } else {
                cell == *origin
            };
            if on_link {
                if *pixel != Some(event.pixel.map(f32::to_bits)) {
                    *pixel = None;
                }
                return Dispatch::Ignore;
            }
            let origin = *origin;
            self.link_press = None;
            self.selecting = Some((SelectionUnit::Cell, cell));
            return Dispatch::Select(SelectStep::DragFrom(origin));
        }
        let (unit, previous) = self.selecting.unwrap();
        if cell == previous {
            return Dispatch::Ignore;
        }
        self.selecting = Some((unit, cell));
        Dispatch::Select(SelectStep::Drag(unit))
    }
}

/// Whether the pointer is in `cell` itself, not in the padding a clamped position shares
/// with an edge cell.
fn in_cell(event: MouseEvent, cell: CellPosition) -> bool {
    event.over_grid && event.cell == cell
}

#[cfg(test)]
mod tests;
