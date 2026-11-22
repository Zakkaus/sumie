use smithay_client_toolkit::{
    reexports::client::{
        Connection, Proxy, QueueHandle,
        protocol::{wl_keyboard::WlKeyboard, wl_seat::WlSeat, wl_surface::WlSurface},
    },
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
};

use crate::{Event, Platform, ime::TextInput};

pub(crate) struct Keyboard {
    seat: WlSeat,
    proxy: WlKeyboard,
    focused: bool,
    modifiers: Modifiers,
    pub(crate) ime: Option<TextInput>,
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        if self.proxy.version() >= 3 {
            self.proxy.release();
        }
    }
}

impl Platform {
    fn start_keyboard(&mut self, qh: &QueueHandle<Self>, seat: WlSeat) {
        let result = self.seats.get_keyboard_with_repeat(
            qh,
            &seat,
            None,
            self.loop_handle.clone(),
            Box::new(|state, keyboard, event| state.emit_key(keyboard, event, true, true)),
        );
        match result {
            Ok(proxy) => {
                self.keyboard = Some(Keyboard {
                    ime: self
                        .text_input_manager
                        .as_ref()
                        .map(|manager| TextInput::new(manager, &seat, qh)),
                    seat,
                    proxy,
                    focused: false,
                    modifiers: Modifiers::default(),
                });
            }
            Err(err) => tracing::warn!(%err, "could not bind keyboard"),
        }
    }

    fn remove_keyboard(&mut self, qh: &QueueHandle<Self>, seat: &WlSeat) {
        if self
            .keyboard
            .as_ref()
            .is_none_or(|keyboard| &keyboard.seat != seat)
        {
            return;
        }
        self.leave_text_input();
        drop(self.keyboard.take());
        if let Some(replacement) = self.seats.seats().find(|candidate| {
            candidate != seat
                && self
                    .seats
                    .info(candidate)
                    .is_some_and(|info| info.has_keyboard)
        }) {
            self.start_keyboard(qh, replacement);
        }
    }

    fn keyboard_mut(&mut self, proxy: &WlKeyboard) -> Option<&mut Keyboard> {
        self.keyboard
            .as_mut()
            .filter(|keyboard| &keyboard.proxy == proxy)
    }

    fn emit_key(&mut self, proxy: &WlKeyboard, event: KeyEvent, pressed: bool, repeat: bool) {
        if let Some(keyboard) = &self.keyboard
            && &keyboard.proxy == proxy
            && keyboard.focused
        {
            self.events.push_back(Event::Key {
                physical: event.raw_code,
                keysym: event.keysym.raw(),
                utf8: event.utf8,
                modifiers: keyboard.modifiers,
                pressed,
                repeat,
            });
        }
    }
}

impl SeatHandler for Platform {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.start_keyboard(qh, seat);
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            self.remove_keyboard(qh, &seat);
        }
    }

    fn remove_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        self.remove_keyboard(qh, &seat);
    }
}

impl KeyboardHandler for Platform {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &WlKeyboard,
        surface: &WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        let focused = surface == self.wl_surface();
        if let Some(keyboard) = self.keyboard_mut(proxy) {
            keyboard.focused = focused;
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &WlKeyboard,
        _: &WlSurface,
        _: u32,
    ) {
        if let Some(keyboard) = self.keyboard_mut(proxy) {
            keyboard.focused = false;
            keyboard.modifiers = Modifiers::default();
            self.disable_text_input();
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.emit_key(proxy, event, true, false);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.emit_key(proxy, event, true, true);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.emit_key(proxy, event, false, false);
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        if let Some(keyboard) = self.keyboard_mut(proxy) {
            keyboard.modifiers = modifiers;
        }
    }
}
