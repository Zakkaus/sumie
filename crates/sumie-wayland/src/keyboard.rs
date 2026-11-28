use smithay_client_toolkit::{
    data_device_manager::data_device::DataDevice,
    primary_selection::device::PrimarySelectionDevice,
    reexports::client::{
        Connection, Proxy, QueueHandle,
        protocol::{wl_keyboard::WlKeyboard, wl_seat::WlSeat, wl_surface::WlSurface},
    },
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
};

use crate::{Event, ime::TextInput, platform::State};

pub(crate) struct Keyboard {
    seat: WlSeat,
    proxy: WlKeyboard,
    pub(crate) selection_serial: Option<u32>,
    modifiers: Modifiers,
    pub(crate) ime: Option<TextInput>,
    pub(crate) data_device: Option<DataDevice>,
    pub(crate) primary_selection: Option<PrimarySelectionDevice>,
}

impl Keyboard {
    pub(crate) fn modifiers(&self) -> Modifiers {
        self.modifiers
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        if self.proxy.version() >= 3 {
            self.proxy.release();
        }
    }
}

impl State {
    fn owns_seat(&self, seat: &WlSeat) -> bool {
        self.keyboard
            .as_ref()
            .is_some_and(|keyboard| &keyboard.seat == seat)
    }

    fn start_keyboard(&mut self, qh: &QueueHandle<Self>, seat: WlSeat) {
        let result = self.seats.get_keyboard_with_repeat(
            qh,
            &seat,
            None,
            self.loop_handle.clone(),
            Box::new(|state, keyboard, event| state.emit_key(keyboard, event, 0, true, true)),
        );
        match result {
            Ok(proxy) => {
                if self.seats.info(&seat).is_some_and(|info| info.has_pointer) {
                    self.start_pointer(qh, &seat);
                }
                self.keyboard = Some(Keyboard {
                    data_device: self
                        .data_device_manager
                        .as_ref()
                        .map(|manager| manager.get_data_device(qh, &seat)),
                    primary_selection: self
                        .primary_selection_manager
                        .as_ref()
                        .map(|manager| manager.get_selection_device(qh, &seat)),
                    ime: self
                        .text_input_manager
                        .as_ref()
                        .map(|manager| TextInput::new(manager, &seat, qh)),
                    seat,
                    proxy,
                    selection_serial: None,
                    modifiers: Modifiers::default(),
                });
            }
            Err(err) => tracing::warn!(%err, "could not bind keyboard"),
        }
    }

    fn remove_keyboard(&mut self, qh: &QueueHandle<Self>, seat: &WlSeat) {
        if !self.owns_seat(seat) {
            return;
        }
        self.stop_pointer();
        self.leave_text_input();
        self.cancel_primary_writes();
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

    fn emit_key(
        &mut self,
        proxy: &WlKeyboard,
        event: KeyEvent,
        serial: u32,
        pressed: bool,
        repeat: bool,
    ) {
        if let Some(keyboard) = self.keyboard_mut(proxy)
            && keyboard.selection_serial.is_some()
        {
            if !repeat {
                keyboard.selection_serial = Some(serial);
            }
            let modifiers = keyboard.modifiers;
            self.events.push_back(Event::Key {
                physical: event.raw_code,
                keysym: event.keysym.raw(),
                utf8: event.utf8,
                modifiers,
                pressed,
                repeat,
                serial,
            });
        }
    }
}

impl SeatHandler for State {
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
        match capability {
            Capability::Keyboard if self.keyboard.is_none() => self.start_keyboard(qh, seat),
            Capability::Pointer if self.owns_seat(&seat) => self.start_pointer(qh, &seat),
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard => self.remove_keyboard(qh, &seat),
            Capability::Pointer if self.owns_seat(&seat) => self.stop_pointer(),
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        self.remove_keyboard(qh, &seat);
    }
}

impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &WlKeyboard,
        surface: &WlSurface,
        serial: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        let focused = surface == self.wl_surface();
        if let Some(keyboard) = self.keyboard_mut(proxy) {
            keyboard.selection_serial = focused.then_some(serial);
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
            keyboard.selection_serial = None;
            keyboard.modifiers = Modifiers::default();
            self.disable_text_input();
            self.events
                .push_back(Event::ModifiersChanged(Modifiers::default()));
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.emit_key(proxy, event, serial, true, false);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.emit_key(proxy, event, serial, true, true);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.emit_key(proxy, event, serial, false, false);
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
            self.events.push_back(Event::ModifiersChanged(modifiers));
        }
    }
}
