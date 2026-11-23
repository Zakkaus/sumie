use sumie_core::{KeyAction, KeyModifiers, Pane, PaneCommand, PaneEffect, Shortcut, Shortcuts};
use sumie_wayland::Event;

use crate::{Error, Result};

pub(crate) enum Effect {
    None,
    Preedit(String),
    ReadClipboard,
    Copy(u32),
    ViewportChanged,
    Search(SearchAction),
}

pub(crate) enum SearchAction {
    Open,
    Close,
    Backspace,
    Insert(String),
    Navigate { newer: bool },
}

pub(crate) fn route(
    pane: &mut impl Pane<Error = Error>,
    shortcuts: &mut Shortcuts,
    searching: bool,
    event: Event,
) -> Result<Effect> {
    let key_text;
    let command = match event {
        Event::ImePreedit { text, .. } => return Ok(Effect::Preedit(text)),
        Event::ImeCommit { text } | Event::ClipboardText(text) if searching => {
            return Ok(Effect::Search(SearchAction::Insert(text)));
        }
        Event::ImeCommit { ref text } => PaneCommand::Text(text.as_bytes()),
        Event::ClipboardText(text) => PaneCommand::Paste(text),
        Event::Key {
            serial,
            physical,
            keysym,
            utf8,
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
            let shortcut = shortcuts.match_key(physical, symbol, modifiers, action);
            if searching {
                if !pressed && !repeat {
                    return Ok(Effect::None);
                }
                shortcuts.capture(physical);
                return Ok(search_key(keysym, modifiers, utf8));
            }
            match shortcut {
                Some(Shortcut::Copy) => return Ok(Effect::Copy(serial)),
                Some(Shortcut::Paste) => return Ok(Effect::ReadClipboard),
                Some(Shortcut::Search) => return Ok(Effect::Search(SearchAction::Open)),
                Some(Shortcut::Consume) => return Ok(Effect::None),
                None => (),
            }
            key_text = utf8;
            PaneCommand::Key {
                physical,
                modifiers,
                text: key_text
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

fn search_key(keysym: u32, modifiers: KeyModifiers, text: Option<String>) -> Effect {
    use xkeysym::key;
    let action = match keysym {
        key::Escape => SearchAction::Close,
        key::BackSpace => SearchAction::Backspace,
        key::Return | key::KP_Enter | key::F3 => SearchAction::Navigate {
            newer: modifiers.shift,
        },
        _ if !modifiers.control && !modifiers.super_key => {
            let Some(text) = text else {
                return Effect::None;
            };
            SearchAction::Insert(text)
        }
        _ => return Effect::None,
    };
    Effect::Search(action)
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
