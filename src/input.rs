use sumie_core::{KeyAction, KeyModifiers, Pane, PaneCommand};
use sumie_wayland::Event;

use crate::{Error, Result};

/// Dispatch input on the pane owner thread. Return only the visual preedit to the view.
pub(crate) fn route(pane: &mut impl Pane<Error = Error>, event: Event) -> Result<Option<String>> {
    let command = match event {
        Event::ImePreedit { text, .. } => return Ok(Some(text)),
        Event::ImeCommit { ref text } => PaneCommand::Text(text.as_bytes()),
        Event::Key {
            physical,
            keysym,
            ref utf8,
            modifiers,
            pressed,
            repeat,
        } => {
            let unshifted = (!modifiers.shift)
                .then(|| xkeysym::Keysym::new(keysym).key_char())
                .flatten()
                .filter(|ch| !ch.is_control());
            PaneCommand::Key {
                physical,
                modifiers: KeyModifiers {
                    shift: modifiers.shift,
                    control: modifiers.ctrl,
                    alt: modifiers.alt,
                    super_key: modifiers.logo,
                    caps_lock: modifiers.caps_lock,
                    num_lock: modifiers.num_lock,
                },
                text: utf8
                    .as_deref()
                    .filter(|text| !text.chars().any(char::is_control))
                    .unwrap_or(""),
                unshifted,
                action: if repeat {
                    KeyAction::Repeat
                } else if pressed {
                    KeyAction::Press
                } else {
                    KeyAction::Release
                },
            }
        }
        _ => return Ok(None),
    };
    if let Err(error) = pane.command(command) {
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::WouldBlock)
        {
            tracing::warn!("terminal input queue is full; dropped an input event");
        } else {
            return Err(error);
        }
    }
    Ok(None)
}
