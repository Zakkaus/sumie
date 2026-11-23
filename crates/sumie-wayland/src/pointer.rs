use smithay_client_toolkit::{
    reexports::client::{
        Connection, Proxy, QueueHandle,
        protocol::{wl_pointer::WlPointer, wl_seat::WlSeat},
    },
    seat::pointer::{BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler},
};

use crate::{Event, PointerPhase, platform::State};

pub(crate) struct Pointer {
    proxy: WlPointer,
    drag_position: Option<(f64, f64)>,
}

impl Pointer {
    fn track(&mut self, event: &PointerEvent) -> Option<Event> {
        let (phase, serial) = match event.kind {
            PointerEventKind::Press {
                button: BTN_LEFT,
                serial,
                ..
            } if self.drag_position.is_none() => {
                self.drag_position = Some(event.position);
                (PointerPhase::Press, serial)
            }
            PointerEventKind::Motion { .. } if self.drag_position.is_some() => {
                self.drag_position = Some(event.position);
                (PointerPhase::Motion, 0)
            }
            PointerEventKind::Release {
                button: BTN_LEFT,
                serial,
                ..
            } if self.drag_position.take().is_some() => (PointerPhase::Release, serial),
            PointerEventKind::Leave { .. } => return self.cancel(),
            _ => return None,
        };
        Some(Event::Pointer {
            phase,
            position: event.position,
            serial,
        })
    }

    fn cancel(&mut self) -> Option<Event> {
        Some(Event::Pointer {
            phase: PointerPhase::Cancel,
            position: self.drag_position.take()?,
            serial: 0,
        })
    }
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
                    drag_position: None,
                });
            }
            Err(error) => tracing::warn!(%error, "could not bind pointer"),
        }
    }

    pub(crate) fn stop_pointer(&mut self) {
        if let Some(mut pointer) = self.pointer.take() {
            self.events.extend(pointer.cancel());
        }
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
        let Some(pointer) = self
            .pointer
            .as_mut()
            .filter(|pointer| &pointer.proxy == proxy)
        else {
            return;
        };
        for event in events.iter().filter(|event| &event.surface == surface) {
            if let PointerEventKind::Press { serial, .. } | PointerEventKind::Release { serial, .. } =
                event.kind
                && let Some(keyboard) = &mut self.keyboard
                && keyboard.selection_serial.is_some()
            {
                keyboard.selection_serial = Some(serial);
            }
            self.events.extend(pointer.track(event));
        }
    }
}
