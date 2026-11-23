use std::{env, process::Command};

use smithay_client_toolkit::{
    reexports::client::{
        Connection, Proxy, QueueHandle,
        protocol::{wl_pointer::WlPointer, wl_seat::WlSeat},
    },
    seat::pointer::{
        AxisScroll, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, PointerEvent, PointerEventKind, PointerHandler,
    },
};

use crate::{Event, Modifiers, PointerButton, PointerPhase, keyboard::Keyboard, platform::State};

const VALUE120_PER_NOTCH: f64 = 120.0;
const LOGICAL_UNITS_PER_NOTCH: f64 = 10.0;

/// Read the desktop's double-click setting before dispatch starts. Wayland only
/// provides event times, not a double-click interval.
#[must_use]
pub fn double_click_interval() -> Option<u32> {
    let desktop = env::var("XDG_CURRENT_DESKTOP").ok()?;
    let output = desktop.split(':').find_map(|name| match name {
        "KDE" => Command::new("kreadconfig6")
            .args([
                "--file",
                "kdeglobals",
                "--group",
                "KDE",
                "--key",
                "DoubleClickInterval",
            ])
            .output()
            .ok(),
        "GNOME" => Command::new("gsettings")
            .args(["get", "org.gnome.desktop.peripherals.mouse", "double-click"])
            .output()
            .ok(),
        _ => None,
    })?;
    if !output.status.success() {
        return None;
    }
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|&ms| ms > 0)
}

pub(crate) struct Pointer {
    proxy: WlPointer,
    position: Option<(f64, f64)>,
}

impl Pointer {
    fn track(
        &mut self,
        event: &PointerEvent,
        modifiers: Modifiers,
        wheel: &mut Option<f64>,
    ) -> Option<Event> {
        let (phase, serial) = match event.kind {
            PointerEventKind::Enter { .. } => {
                self.position = Some(event.position);
                return None;
            }
            PointerEventKind::Leave { .. } => return self.cancel(modifiers),
            PointerEventKind::Motion { .. } => (PointerPhase::Motion, 0),
            PointerEventKind::Press {
                button,
                serial,
                time,
            } => (PointerPhase::Press(pointer_button(button)?, time), serial),
            PointerEventKind::Release { button, serial, .. } => {
                (PointerPhase::Release(pointer_button(button)?), serial)
            }
            PointerEventKind::Axis { ref vertical, .. } if has_vertical_delta(vertical) => {
                (PointerPhase::Wheel(wheel.take()?), 0)
            }
            PointerEventKind::Axis { .. } => return None,
        };
        self.position = Some(event.position);
        Some(Event::Pointer {
            phase,
            position: event.position,
            serial,
            modifiers,
        })
    }

    fn cancel(&mut self, modifiers: Modifiers) -> Option<Event> {
        Some(Event::Pointer {
            phase: PointerPhase::Cancel,
            position: self.position.take()?,
            serial: 0,
            modifiers,
        })
    }
}

fn pointer_button(button: u32) -> Option<PointerButton> {
    match button {
        BTN_LEFT => Some(PointerButton::Left),
        BTN_MIDDLE => Some(PointerButton::Middle),
        BTN_RIGHT => Some(PointerButton::Right),
        _ => None,
    }
}

fn has_vertical_delta(axis: &AxisScroll) -> bool {
    axis.value120 != 0 || axis.discrete != 0 || axis.absolute != 0.0
}

fn wheel_delta<'a>(events: impl Iterator<Item = &'a PointerEvent>) -> Option<f64> {
    // SCTK only merges adjacent axis events. The discrete and absolute forms
    // can be separated by another event in the same frame.
    let mut value120 = None;
    let mut discrete = None;
    let mut absolute = 0.0;
    for event in events {
        if let PointerEventKind::Axis { ref vertical, .. } = event.kind {
            if vertical.value120 != 0 {
                *value120.get_or_insert(0.0) += f64::from(vertical.value120);
            }
            if vertical.discrete != 0 {
                *discrete.get_or_insert(0.0) += f64::from(vertical.discrete);
            }
            absolute += vertical.absolute;
        }
    }
    let delta = value120
        .or_else(|| discrete.map(|steps| steps * VALUE120_PER_NOTCH))
        .unwrap_or(absolute * (VALUE120_PER_NOTCH / LOGICAL_UNITS_PER_NOTCH));
    (delta != 0.0).then_some(delta)
}

impl Drop for Pointer {
    fn drop(&mut self) {
        if self.proxy.version() >= 3 {
            self.proxy.release();
        }
    }
}

impl State {
    pub(crate) fn start_pointer(&mut self, qh: &QueueHandle<Self>, seat: &WlSeat) {
        match self.seats.get_pointer(qh, seat) {
            Ok(proxy) => {
                self.pointer = Some(Pointer {
                    proxy,
                    position: None,
                });
            }
            Err(error) => tracing::warn!(%error, "could not bind pointer"),
        }
    }

    pub(crate) fn stop_pointer(&mut self) {
        if let Some(mut pointer) = self.pointer.take() {
            let modifiers = self.pointer_modifiers();
            self.events.extend(pointer.cancel(modifiers));
        }
    }

    fn pointer_modifiers(&self) -> Modifiers {
        self.keyboard
            .as_ref()
            .map_or_else(Modifiers::default, Keyboard::modifiers)
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &WlPointer,
        events: &[PointerEvent],
    ) {
        let surface = self.window.wl_surface();
        let modifiers = self.pointer_modifiers();
        let Some(pointer) = self
            .pointer
            .as_mut()
            .filter(|pointer| &pointer.proxy == proxy)
        else {
            return;
        };
        let events = events.iter().filter(|event| &event.surface == surface);
        let mut wheel = wheel_delta(events.clone());
        for event in events {
            if let PointerEventKind::Press { serial, .. } | PointerEventKind::Release { serial, .. } =
                event.kind
                && let Some(keyboard) = &mut self.keyboard
                && keyboard.selection_serial.is_some()
            {
                keyboard.selection_serial = Some(serial);
            }
            self.events
                .extend(pointer.track(event, modifiers, &mut wheel));
        }
    }
}
