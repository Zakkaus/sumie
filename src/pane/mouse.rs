use sumie_core::{
    KeyCode, LinkUri, PaneEffect,
    mouse::{Button, Dispatch, MouseEvent, MouseModes, MousePhase, MouseState, WheelRoute},
};
use sumie_vt::{
    KeyAction, KeyEvent, MouseAction, MouseButton, MouseEncoder, MouseReport, Terminal,
};

use super::{LocalPane, modifier_bits};
use crate::Result;

pub(super) struct Mouse {
    pub(super) state: MouseState,
    encoder: MouseEncoder,
    modes: MouseModes,
    last_report: Option<MouseEvent>,
    link_press: Option<LinkUri>,
    grid: [u32; 2],
    cell: [u32; 2],
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
            last_report: None,
            link_press: None,
            grid: grid.map(u32::from),
            cell,
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
        if self.link_press.take().is_some() {
            self.state.reset_gestures();
        } else {
            self.state.reset_clicks();
        }
    }

    fn report(
        &self,
        event: MouseEvent,
        action: MouseAction,
        button: Option<MouseButton>,
    ) -> MouseReport {
        MouseReport {
            action,
            button,
            modifiers: modifier_bits(event.modifiers),
            position: event.pixel,
            cell: self.cell,
            grid: self.grid,
            any_button: action == MouseAction::Press || self.state.any_button(),
        }
    }
}

impl LocalPane {
    pub(crate) fn mouse_tracking(&self) -> bool {
        self.mouse.modes.tracking
    }

    pub(crate) fn set_click_interval(&mut self, millis: u32) {
        self.mouse.state.set_click_interval(millis);
    }

    pub(crate) fn pointer_modifiers_changed(&mut self, modifiers: sumie_core::KeyModifiers) {
        self.mouse.state.modifiers_changed(modifiers);
    }

    pub(crate) fn capture_link(&mut self, uri: Option<&LinkUri>) {
        self.mouse.link_press = uri.cloned();
    }

    pub(crate) fn take_pressed_link(&mut self) -> Option<LinkUri> {
        self.mouse.link_press.take()
    }

    pub(crate) fn reset_pointer_gestures(&mut self) {
        self.mouse.state.reset_gestures();
        self.mouse.link_press = None;
    }

    pub(crate) fn leave_tab(&mut self) -> Result<()> {
        self.reset_pointer_gestures();
        if self.mouse.state.any_button()
            && let Some(event) = self.mouse.last_report
        {
            for button in [Button::Left, Button::Middle, Button::Right] {
                self.search_pointer(MouseEvent {
                    phase: MousePhase::Release(button),
                    ..event
                })?;
            }
        }
        Ok(())
    }

    pub(crate) fn search_pointer(&mut self, event: MouseEvent) -> Result<PaneEffect> {
        if matches!(event.phase, MousePhase::Press(..) | MousePhase::Motion) {
            return Ok(PaneEffect::None);
        }
        let history_only = MouseModes {
            tracking: false,
            alternate: false,
            alternate_scroll: false,
        };
        match self.pointer_with_modes(event, history_only) {
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock) =>
            {
                tracing::warn!("terminal input queue is full; dropped mouse release");
                Ok(PaneEffect::None)
            }
            result => result,
        }
    }

    pub(super) fn pointer(&mut self, event: MouseEvent) -> Result<PaneEffect> {
        self.pointer_with_modes(event, self.mouse.modes)
    }

    fn pointer_with_modes(&mut self, event: MouseEvent, modes: MouseModes) -> Result<PaneEffect> {
        let dispatch = self.mouse.state.dispatch(event, modes);
        if self.mouse.link_press.is_some() && !self.mouse.state.awaiting_link_release() {
            self.mouse.link_press = None;
        }
        let (action, button) = match dispatch {
            Dispatch::Ignore => return Ok(PaneEffect::None),
            Dispatch::Select(step) => return Ok(PaneEffect::Select(step)),
            Dispatch::OpenLink => return Ok(PaneEffect::OpenLink),
            Dispatch::Press(b) => (MouseAction::Press, Some(b)),
            Dispatch::Release(b) => (MouseAction::Release, Some(b)),
            Dispatch::Motion(b) => (MouseAction::Motion, b),
            Dispatch::Wheel(route, steps) => return self.wheel(event, route, steps),
        };
        let report = self.mouse.report(event, action, button.map(native_button));
        let bytes = self.mouse.encoder.encode(report)?;
        if !bytes.is_empty() {
            self.pty.enqueue_input(bytes)?;
            self.mouse.last_report = Some(event);
            if let (MouseAction::Press, Some(button)) = (action, button) {
                self.mouse.state.press_reported(button);
            }
        }
        Ok(PaneEffect::None)
    }

    fn wheel(
        &mut self,
        mut event: MouseEvent,
        route: WheelRoute,
        steps: i32,
    ) -> Result<PaneEffect> {
        let bytes = match route {
            WheelRoute::History => return self.scroll_history(steps),
            WheelRoute::Arrows => {
                let key = if steps < 0 {
                    KeyCode::ArrowUp
                } else {
                    KeyCode::ArrowDown
                };
                self.encoder.encode(
                    &self.terminal,
                    &KeyEvent {
                        key: key as u8,
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
                event.modifiers.alt = false;
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
        self.mouse.state.reset_clicks();
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
