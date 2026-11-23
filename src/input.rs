use sumie_core::{KeyAction, KeyModifiers, Pane, PaneCommand, PaneEffect, Shortcut, Shortcuts};
use sumie_wayland::Event;

use crate::{Error, Result};

pub(crate) enum Effect {
    None,
    Preedit(String),
    ReadClipboard,
    Copy(u32),
    ViewportChanged,
}

pub(crate) fn route(
    pane: &mut impl Pane<Error = Error>,
    shortcuts: &mut Shortcuts,
    event: Event,
) -> Result<Effect> {
    let command = match event {
        Event::ImePreedit { text, .. } => return Ok(Effect::Preedit(text)),
        Event::ImeCommit { ref text } => PaneCommand::Text(text.as_bytes()),
        Event::ClipboardText(text) => PaneCommand::Paste(text),
        Event::Key {
            serial,
            physical,
            keysym,
            ref utf8,
            modifiers,
            pressed,
            repeat,
        } => {
            let modifiers = key_modifiers(modifiers);
            let action = if repeat {
                KeyAction::Repeat
            } else if pressed {
                KeyAction::Press
            } else {
                KeyAction::Release
            };
            let symbol = xkeysym::Keysym::new(keysym).key_char();
            match shortcuts.match_key(physical, symbol, modifiers, action) {
                Some(Shortcut::Copy) => return Ok(Effect::Copy(serial)),
                Some(Shortcut::Paste) => return Ok(Effect::ReadClipboard),
                Some(Shortcut::Consume) => return Ok(Effect::None),
                None => (),
            }
            PaneCommand::Key {
                physical,
                modifiers,
                text: utf8
                    .as_deref()
                    .filter(|text| !text.chars().any(char::is_control))
                    .unwrap_or(""),
                unshifted: symbol.filter(|ch| !modifiers.shift && !ch.is_control()),
                action,
            }
        }
        _ => return Ok(Effect::None),
    };
    match send(pane, command)? {
        PaneEffect::ViewportChanged => Ok(Effect::ViewportChanged),
        _ => Ok(Effect::None),
    }
}

pub(crate) fn send(
    pane: &mut impl Pane<Error = Error>,
    command: PaneCommand<'_>,
) -> Result<PaneEffect> {
    let paste_len = match &command {
        PaneCommand::Paste(text) => Some(text.len()),
        _ => None,
    };
    match pane.command(command) {
        Ok(effect) => Ok(effect),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock) =>
        {
            tracing::warn!(
                paste_bytes = paste_len,
                "terminal input queue is full; dropped input"
            );
            Ok(PaneEffect::None)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn key_modifiers(modifiers: sumie_wayland::Modifiers) -> KeyModifiers {
    KeyModifiers {
        shift: modifiers.shift,
        control: modifiers.ctrl,
        alt: modifiers.alt,
        super_key: modifiers.logo,
        caps_lock: modifiers.caps_lock,
        num_lock: modifiers.num_lock,
    }
}

pub(crate) fn mouse_phase(phase: sumie_wayland::PointerPhase) -> sumie_core::mouse::MousePhase {
    use sumie_core::mouse::{Button, MousePhase};
    use sumie_wayland::{PointerButton, PointerPhase};
    let button = |b| match b {
        PointerButton::Left => Button::Left,
        PointerButton::Middle => Button::Middle,
        PointerButton::Right => Button::Right,
    };
    match phase {
        PointerPhase::Press(b) => MousePhase::Press(button(b)),
        PointerPhase::Release(b) => MousePhase::Release(button(b)),
        PointerPhase::Motion => MousePhase::Motion,
        PointerPhase::Wheel(delta) => MousePhase::Wheel(delta),
        PointerPhase::Cancel => MousePhase::Cancel,
    }
}
