use sumie_core::{KeyAction, KeyModifiers, Pane, PaneCommand, Shortcut, Shortcuts};
use sumie_wayland::Event;

use crate::{Error, Result};

pub(crate) enum Effect {
    None,
    Preedit(String),
    ReadClipboard,
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
            physical,
            keysym,
            ref utf8,
            modifiers,
            pressed,
            repeat,
        } => {
            let modifiers = KeyModifiers {
                shift: modifiers.shift,
                control: modifiers.ctrl,
                alt: modifiers.alt,
                super_key: modifiers.logo,
                caps_lock: modifiers.caps_lock,
                num_lock: modifiers.num_lock,
            };
            let action = if repeat {
                KeyAction::Repeat
            } else if pressed {
                KeyAction::Press
            } else {
                KeyAction::Release
            };
            let symbol = xkeysym::Keysym::new(keysym).key_char();
            match shortcuts.match_key(physical, symbol, modifiers, action) {
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
    let paste_len = match &command {
        PaneCommand::Paste(text) => Some(text.len()),
        _ => None,
    };
    if let Err(error) = pane.command(command) {
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::WouldBlock)
        {
            tracing::warn!(
                paste_bytes = paste_len,
                "terminal input queue is full; dropped input"
            );
        } else {
            return Err(error);
        }
    }
    Ok(Effect::None)
}
