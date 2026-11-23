use sumie_core::{
    PaneEffect,
    mouse::{
        Button, Dispatch, MouseEvent, MouseModes, MousePhase, MouseState, SelectStep, WheelRoute,
    },
};
use sumie_vt::{
    KeyAction, KeyEvent, MouseAction, MouseButton, MouseEncoder, MouseReport, Terminal,
};

use super::{LocalPane, modifier_bits};
use crate::{Result, keycodes};

pub(super) struct Mouse {
    pub(super) state: MouseState,
    encoder: MouseEncoder,
    modes: MouseModes,
    grid: [u32; 2],
    cell: [u32; 2],
    click: Vec<u8>,
}

fn modes(terminal: &Terminal) -> Result<MouseModes> {
    let (tracking, alternate, alternate_scroll) = terminal.mouse_modes()?;
    Ok(MouseModes {
        tracking,
        alternate,
        alternate_scroll,
    })
}

impl Mouse {
    pub(super) fn new(terminal: &Terminal, grid: [u16; 2], cell: [u32; 2]) -> Result<Self> {
        let mut encoder = MouseEncoder::new()?;
        encoder.sync(terminal)?;
        Ok(Self {
            state: MouseState::default(),
            encoder,
            modes: modes(terminal)?,
            grid: grid.map(u32::from),
            cell,
            click: Vec::new(),
        })
    }

    pub(super) fn sync(&mut self, terminal: &Terminal) -> Result<()> {
        self.modes = modes(terminal)?;
        self.encoder.sync(terminal)?;
        Ok(())
    }

    pub(super) fn resize(&mut self, grid: [u16; 2], cell: [u32; 2]) {
        self.grid = grid.map(u32::from);
        self.cell = cell;
        self.state.cancel_selection();
    }

    fn report(
        &self,
        event: MouseEvent,
        action: MouseAction,
        button: Option<MouseButton>,
    ) -> MouseReport {
        let mut modifiers = event.modifiers;
        modifiers.alt = false;
        MouseReport {
            action,
            button,
            modifiers: modifier_bits(modifiers),
            position: event.pixel,
            cell: self.cell,
            grid: self.grid,
            any_button: action == MouseAction::Press || self.state.any_button(),
        }
    }
}

impl LocalPane {
    pub(crate) fn reset_pointer(&mut self) {
        self.mouse.state = MouseState::default();
    }

    pub(crate) fn search_pointer(&mut self, event: MouseEvent) -> Result<PaneEffect> {
        if !matches!(event.phase, MousePhase::Wheel(_)) {
            return Ok(PaneEffect::None);
        }
        let history_only = MouseModes {
            tracking: false,
            alternate: false,
            alternate_scroll: false,
        };
        if let Dispatch::Wheel(WheelRoute::History, steps) =
            self.mouse.state.dispatch(event, history_only)
        {
            return self.scroll_history(steps);
        }
        Ok(PaneEffect::None)
    }

    pub(super) fn pointer(&mut self, event: MouseEvent) -> Result<PaneEffect> {
        let (action, button) = match self.mouse.state.dispatch(event, self.mouse.modes) {
            Dispatch::Ignore => return Ok(PaneEffect::None),
            Dispatch::Select(step) => return Ok(PaneEffect::Select(step)),
            Dispatch::Click(press) => {
                // A full PTY queue must not admit just the press of a deferred click.
                self.mouse.click.clear();
                for action in [MouseAction::Press, MouseAction::Release] {
                    let report = self.mouse.report(press, action, Some(MouseButton::Left));
                    self.mouse
                        .click
                        .extend_from_slice(self.mouse.encoder.encode(report)?);
                }
                self.pty.enqueue_input(&self.mouse.click)?;
                return Ok(PaneEffect::Select(SelectStep::Clear));
            }
            Dispatch::Press(b) => (MouseAction::Press, Some(native_button(b))),
            Dispatch::Release(b) => (MouseAction::Release, Some(native_button(b))),
            Dispatch::Motion(b) => (MouseAction::Motion, b.map(native_button)),
            Dispatch::Wheel(route, steps) => return self.wheel(event, route, steps),
        };
        let report = self.mouse.report(event, action, button);
        self.pty.enqueue_input(self.mouse.encoder.encode(report)?)?;
        Ok(PaneEffect::None)
    }

    fn wheel(&mut self, event: MouseEvent, route: WheelRoute, steps: i32) -> Result<PaneEffect> {
        let bytes = match route {
            WheelRoute::History => return self.scroll_history(steps),
            WheelRoute::Arrows => {
                let evdev = if steps < 0 { 103 } else { 108 };
                self.encoder.encode(
                    &self.terminal,
                    &KeyEvent {
                        key: keycodes::ghostty_key(evdev),
                        action: KeyAction::Press,
                        modifiers: 0,
                        consumed_modifiers: 0,
                        unshifted: None,
                        text: "",
                    },
                )?
            }
            WheelRoute::Report => {
                let button = if steps < 0 {
                    MouseButton::WheelUp
                } else {
                    MouseButton::WheelDown
                };
                let report = self.mouse.report(event, MouseAction::Press, Some(button));
                self.mouse.encoder.encode(report)?
            }
        };
        if !bytes.is_empty() {
            for _ in 0..steps.unsigned_abs() {
                self.pty.enqueue_input(bytes)?;
            }
        }
        Ok(PaneEffect::None)
    }

    fn scroll_history(&mut self, steps: i32) -> Result<PaneEffect> {
        self.terminal.scroll_rows(steps)?;
        self.mouse.state.cancel_selection();
        Ok(PaneEffect::ViewportChanged)
    }
}

fn native_button(button: Button) -> MouseButton {
    match button {
        Button::Left => MouseButton::Left,
        Button::Middle => MouseButton::Middle,
        Button::Right => MouseButton::Right,
    }
}
