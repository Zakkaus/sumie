mod keycodes;

use sumie_core::KeyAction;
use sumie_wayland::{Event as Native, PointerButton, PointerPhase};

use crate::platform::{
    Event, LogicalKey, Modifiers, PointerButton as Button, PointerEvent, PointerPhase as Phase,
};

/// Translate one event, never a batch. Only the adapter dispatching this event
/// may use its serial to satisfy a user copy request.
pub(super) fn convert(native: Native) -> Option<(Event, Option<u32>)> {
    let mut copy_serial = None;
    let event = match native {
        Native::Configured {
            activated,
            suspended,
            ..
        } => Event::Configured {
            activated,
            suspended,
        },
        Native::Resized {
            logical,
            device,
            scale,
        } => Event::Resized {
            logical,
            device,
            scale,
        },
        Native::FrameReady => Event::FrameReady,
        Native::CloseRequested => Event::CloseRequested,
        Native::Presentation(feedback) => presented(feedback)?,
        Native::Key {
            physical,
            keysym,
            utf8,
            modifiers: mods,
            pressed,
            repeat,
            serial,
        } => {
            let (action, origin) = match (pressed, repeat) {
                (_, true) => (KeyAction::Repeat, None),
                (true, false) => (KeyAction::Press, Some(serial)),
                (false, false) => (KeyAction::Release, None),
            };
            copy_serial = origin;
            Event::Key {
                physical: keycodes::physical_key(physical),
                logical: logical_key(keysym),
                utf8,
                modifiers: modifiers(mods),
                action,
            }
        }
        Native::ModifiersChanged(mods) => Event::ModifiersChanged(modifiers(mods)),
        Native::PointerEntered {
            position,
            modifiers: mods,
        } => Event::PointerEntered {
            position,
            modifiers: modifiers(mods),
        },
        Native::Pointer {
            position,
            phase,
            modifiers: mods,
            serial,
        } => {
            let (phase, origin) = pointer_phase(phase, serial);
            copy_serial = origin;
            Event::Pointer(PointerEvent {
                position,
                phase,
                modifiers: modifiers(mods),
            })
        }
        Native::ClipboardText { text, generation } => Event::ClipboardText { text, generation },
        Native::ImePreedit {
            text, generation, ..
        } => Event::ImePreedit { text, generation },
        Native::ImeCommit { text, generation } => Event::ImeCommit { text, generation },
    };
    Some((event, copy_serial))
}

fn presented(feedback: sumie_wayland::PresentationFeedback) -> Option<Event> {
    let sumie_wayland::PresentationFeedback::Presented {
        timestamp,
        refresh_ns,
        flags,
        ..
    } = feedback
    else {
        return None;
    };
    tracing::debug!(?timestamp, refresh_ns, flags, "presented");
    Some(Event::Presented {
        at: timestamp,
        refresh: std::time::Duration::from_nanos(refresh_ns.into()),
    })
}

fn modifiers(mods: sumie_wayland::Modifiers) -> Modifiers {
    Modifiers {
        shift: mods.shift,
        control: mods.ctrl,
        alt: mods.alt,
        super_key: mods.logo,
        caps_lock: mods.caps_lock,
        num_lock: mods.num_lock,
    }
}

fn button(button: PointerButton) -> Button {
    match button {
        PointerButton::Left => Button::Left,
        PointerButton::Middle => Button::Middle,
        PointerButton::Right => Button::Right,
    }
}

fn logical_key(keysym: u32) -> LogicalKey {
    use xkeysym::key;
    match keysym {
        key::Escape => LogicalKey::Escape,
        key::Return => LogicalKey::Enter,
        key::KP_Enter => LogicalKey::NumpadEnter,
        key::BackSpace => LogicalKey::Backspace,
        key::Insert | key::KP_Insert => LogicalKey::Insert,
        key::Home | key::KP_Home => LogicalKey::Home,
        key::End | key::KP_End => LogicalKey::End,
        key::Left | key::KP_Left => LogicalKey::Left,
        key::Right | key::KP_Right => LogicalKey::Right,
        key::F3 => LogicalKey::F3,
        _ => match xkeysym::Keysym::new(keysym).key_char() {
            // A non-character XKB key can map to ASCII (e.g. KP_Equal).
            // Keep that mapping for the terminal without turning it into a letter shortcut.
            Some(ch) if !ch.is_ascii() || keysym == u32::from(ch) => LogicalKey::Character(ch),
            ch => LogicalKey::Other(ch),
        },
    }
}

fn pointer_phase(phase: PointerPhase, serial: u32) -> (Phase, Option<u32>) {
    match phase {
        PointerPhase::Press(b, time) => (Phase::Press(button(b), time), None),
        PointerPhase::Release(b) => (Phase::Release(button(b)), Some(serial)),
        PointerPhase::Motion => (Phase::Motion, None),
        PointerPhase::Wheel(delta) => (Phase::Wheel(delta), None),
        PointerPhase::Touch(delta) => (Phase::Touch(delta), None),
        PointerPhase::TouchEnd => (Phase::TouchEnd, None),
        PointerPhase::Cancel => (Phase::Cancel, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_uses_only_the_triggering_press_or_release() {
        let key = |pressed, repeat, serial| Native::Key {
            physical: 46,
            keysym: xkeysym::key::Cyrillic_ES,
            utf8: None,
            modifiers: sumie_wayland::Modifiers::default(),
            pressed,
            repeat,
            serial,
        };
        let pointer = |phase, serial| Native::Pointer {
            position: (0.0, 0.0),
            modifiers: sumie_wayland::Modifiers::default(),
            phase,
            serial,
        };
        let left = PointerButton::Left;
        for (event, expected) in [
            (key(true, false, 31), Some(31)),
            (key(true, true, 0), None),
            (key(false, false, 32), None),
            (pointer(PointerPhase::Press(left, 42), 33), None),
            (pointer(PointerPhase::Release(left), 34), Some(34)),
            (Native::FrameReady, None),
        ] {
            let (_, serial) = convert(event).unwrap();
            assert_eq!(serial, expected);
        }
    }

    #[test]
    fn layout_symbols_do_not_replace_physical_positions() {
        use sumie_core::{KeyCode, PhysicalKey};
        use xkeysym::key;

        for (code, symbol, physical, character) in [
            (18, key::F, PhysicalKey::Code(KeyCode::KeyE), 'F'),
            (46, key::Cyrillic_ES, PhysicalKey::Code(KeyCode::KeyC), 'С'),
            (255, key::a, PhysicalKey::Unidentified(255), 'a'),
        ] {
            let native = Native::Key {
                physical: code,
                keysym: symbol,
                utf8: None,
                modifiers: sumie_wayland::Modifiers::default(),
                pressed: true,
                repeat: false,
                serial: 0,
            };
            let Some((
                Event::Key {
                    physical: translated,
                    logical,
                    utf8,
                    ..
                },
                _,
            )) = convert(native)
            else {
                panic!("expected key");
            };
            assert_eq!(translated, physical);
            assert_eq!(logical.character(), Some(character));
            assert!(utf8.is_none());
        }
    }
}
