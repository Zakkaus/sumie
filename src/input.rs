use sumie_core::mouse::{Button, MouseEvent, MousePhase, SelectStep};
use sumie_core::{
    FontZoom, KeyAction, KeyModifiers, Pane, PaneCommand, PaneEffect, Shortcut, Shortcuts,
    TabAction,
};
use sumie_wayland::Event;

use crate::{Error, Result, pane::LocalPane};

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) enum Effect {
    None,
    Preedit(String),
    ReadClipboard,
    Copy(u32),
    Select(SelectStep),
    OpenLink,
    ViewportChanged,
    Search(SearchAction),
    Tab(TabAction),
    Zoom(FontZoom),
}

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) enum SearchAction {
    Open,
    Close,
    Backspace,
    Insert(String),
    Navigate { newer: bool },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchTarget {
    Field,
    Previous,
    Next,
    Close,
}

#[derive(Default)]
pub(crate) struct Routing {
    shortcuts: Shortcuts,
    pub(crate) generation: u64,
    repeating_search_key: Option<u32>,
    search_presses: [Option<SearchTarget>; 3],
}

impl Routing {
    pub(crate) fn change_recipient(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.repeating_search_key = None;
        // Keep ownership until release, but never activate a button in a reopened search.
        for pressed in self.search_presses.iter_mut().flatten() {
            *pressed = SearchTarget::Field;
        }
        self.generation
    }

    fn search_key(
        &mut self,
        physical: u32,
        keysym: u32,
        modifiers: KeyModifiers,
        text: Option<String>,
        action: KeyAction,
        shortcut: Option<Shortcut>,
    ) -> Effect {
        match action {
            KeyAction::Press => {
                self.shortcuts.capture(physical);
                match shortcut {
                    Some(Shortcut::Paste) => Effect::ReadClipboard,
                    Some(_) => Effect::None,
                    None => {
                        let effect = search_key(keysym, modifiers, text);
                        if matches!(effect, Effect::Search(_)) {
                            self.repeating_search_key = Some(physical);
                        }
                        effect
                    }
                }
            }
            KeyAction::Repeat if self.repeating_search_key == Some(physical) => {
                search_key(keysym, modifiers, text)
            }
            KeyAction::Release if self.repeating_search_key == Some(physical) => {
                self.repeating_search_key = None;
                Effect::None
            }
            _ => Effect::None,
        }
    }
}

pub(crate) fn route(
    recipient: u64,
    pane: &mut impl Pane<Error = Error>,
    routing: &mut Routing,
    searching: bool,
    event: Event,
) -> Result<Effect> {
    if matches!(
        &event,
        Event::ImePreedit { generation, .. }
            | Event::ImeCommit { generation, .. }
            | Event::ClipboardText { generation, .. }
            if *generation != recipient
    ) {
        return Ok(Effect::None);
    }
    let key_text;
    let command = match event {
        Event::ImePreedit { text, .. } => return Ok(Effect::Preedit(text)),
        Event::ImeCommit { text, .. } | Event::ClipboardText { text, .. } if searching => {
            return Ok(Effect::Search(SearchAction::Insert(text)));
        }
        Event::ImeCommit { ref text, .. } => PaneCommand::Text(text.as_bytes()),
        Event::ClipboardText { text, .. } => PaneCommand::Paste(text),
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
            let shortcut = routing
                .shortcuts
                .match_key(physical, symbol, modifiers, action);
            match shortcut {
                Some(Shortcut::Tab(action)) => return Ok(Effect::Tab(action)),
                Some(Shortcut::Zoom(action)) => return Ok(Effect::Zoom(action)),
                shortcut if searching => {
                    return Ok(
                        routing.search_key(physical, keysym, modifiers, utf8, action, shortcut)
                    );
                }
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

impl Routing {
    pub(crate) fn pointer(
        &mut self,
        pane: &mut LocalPane,
        searching: bool,
        event: MouseEvent,
        hit: impl FnOnce() -> Option<SearchTarget>,
    ) -> Result<Effect> {
        match event.phase {
            MousePhase::Press(button, _) if searching => {
                self.search_presses[button as usize] = hit();
            }
            MousePhase::Motion if self.search_presses.iter().any(Option::is_some) => {
                return Ok(Effect::None);
            }
            MousePhase::Release(button)
                if let Some(target) = self.search_presses[button as usize].take() =>
            {
                return Ok(
                    if searching && button == Button::Left && hit() == Some(target) {
                        match target {
                            SearchTarget::Field => Effect::None,
                            SearchTarget::Close => Effect::Search(SearchAction::Close),
                            SearchTarget::Previous | SearchTarget::Next => {
                                Effect::Search(SearchAction::Navigate {
                                    newer: target == SearchTarget::Next,
                                })
                            }
                        }
                    } else {
                        Effect::None
                    },
                );
            }
            MousePhase::Cancel => self.search_presses.fill(None),
            _ => (),
        }
        Ok(match send_pointer(pane, searching, event)? {
            PaneEffect::ViewportChanged => Effect::ViewportChanged,
            PaneEffect::Select(step) => Effect::Select(step),
            PaneEffect::OpenLink => Effect::OpenLink,
            _ => Effect::None,
        })
    }
}

fn send_pointer(pane: &mut LocalPane, searching: bool, event: MouseEvent) -> Result<PaneEffect> {
    if searching {
        return pane.search_pointer(event);
    }
    send(pane, PaneCommand::Mouse(event))
}

fn send(pane: &mut impl Pane<Error = Error>, command: PaneCommand<'_>) -> Result<PaneEffect> {
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

pub(crate) fn mouse_event(
    phase: sumie_wayland::PointerPhase,
    modifiers: sumie_wayland::Modifiers,
    (cell, pixel): (sumie_core::CellPosition, [f32; 2]),
) -> MouseEvent {
    use sumie_wayland::{PointerButton, PointerPhase};
    let button = |b| match b {
        PointerButton::Left => Button::Left,
        PointerButton::Middle => Button::Middle,
        PointerButton::Right => Button::Right,
    };
    let phase = match phase {
        PointerPhase::Press(b, time) => MousePhase::Press(button(b), time),
        PointerPhase::Release(b) => MousePhase::Release(button(b)),
        PointerPhase::Motion => MousePhase::Motion,
        PointerPhase::Wheel(delta) => MousePhase::Wheel(delta),
        PointerPhase::Cancel => MousePhase::Cancel,
    };
    MouseEvent {
        phase,
        cell,
        pixel,
        modifiers: key_modifiers(modifiers),
        link: false,
    }
}
