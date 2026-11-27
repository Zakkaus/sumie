use sumie_core::mouse::{Button, MouseEvent, MousePhase, SelectStep};
use sumie_core::{
    FontZoom, KeyAction, KeyModifiers, Pane, PaneAction, PaneCommand, PaneEffect, PhysicalKey,
    Shortcut, Shortcuts, TabAction,
};

use crate::{
    Error, Result,
    pane::LocalPane,
    platform::{Event, LogicalKey},
};

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) enum Effect {
    None,
    Preedit(String),
    ReadClipboard,
    Copy,
    Select(SelectStep),
    OpenLink,
    ViewportChanged,
    Search(SearchAction),
    Tab(TabAction),
    Pane(PaneAction),
    Zoom(FontZoom),
}

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) enum SearchAction {
    Open,
    Close,
    SelectAll,
    Home,
    End,
    Move { right: bool },
    Backspace { word: bool },
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

pub(crate) struct HeldKey {
    physical: PhysicalKey,
    unshifted: Option<char>,
    pub(crate) pane: u64,
    generation: u64,
    reported: bool,
}

pub(crate) struct Routing {
    shortcuts: Shortcuts,
    pub(crate) generation: u64,
    terminal_presses: Vec<HeldKey>,
    repeating_search_key: Option<PhysicalKey>,
    search_presses: [Option<SearchTarget>; 3],
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            shortcuts: Shortcuts::new(crate::platform::POLICY),
            generation: 0,
            terminal_presses: Vec::new(),
            repeating_search_key: None,
            search_presses: [None; 3],
        }
    }
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

    pub(crate) fn reset_pointer(&mut self) {
        self.search_presses.fill(None);
    }

    pub(crate) fn release_owner(&self, event: &Event) -> Option<u64> {
        let Event::Key {
            physical,
            action: KeyAction::Release,
            ..
        } = event
        else {
            return None;
        };
        self.terminal_presses
            .iter()
            .find(|p| p.physical == *physical)
            .map(|p| p.pane)
    }

    pub(crate) fn forget_pane(&mut self, id: u64) {
        self.terminal_presses.retain(|p| p.pane != id);
    }

    /// End reported presses before giving the keyboard exclusively to the image viewer.
    pub(crate) fn release_pane_keys(
        &mut self,
        id: u64,
        pane: &mut LocalPane,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        for key in self.terminal_presses.extract_if(.., |key| key.pane == id) {
            if key.reported {
                release(pane, &key, modifiers)?;
            }
        }
        Ok(())
    }

    /// The window lost the keyboard, which takes its held keys along: no repeat or release
    /// from before reaches a PTY. Returns the reported presses; each still owes its own pane a
    /// release ([`release`]).
    pub(crate) fn keyboard_left(&mut self) -> Vec<HeldKey> {
        self.repeating_search_key = None;
        self.terminal_presses
            .drain(..)
            .filter(|key| key.reported)
            .collect()
    }

    fn terminal_key(&mut self, physical: PhysicalKey, id: u64, action: KeyAction) -> bool {
        let held = self
            .terminal_presses
            .iter()
            .position(|p| p.physical == physical);
        match (action, held) {
            (KeyAction::Press, Some(i)) => {
                self.terminal_presses.swap_remove(i);
            }
            (KeyAction::Press, None) => (),
            (KeyAction::Repeat, Some(i)) => {
                let press = &self.terminal_presses[i];
                return press.pane == id && press.generation == self.generation;
            }
            (KeyAction::Release, Some(i)) if self.terminal_presses[i].pane == id => {
                let press = self.terminal_presses.swap_remove(i);
                return press.generation == self.generation || press.reported;
            }
            _ => return false,
        }
        true
    }

    fn search_key(
        &mut self,
        physical: PhysicalKey,
        logical: LogicalKey,
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
                    None => {
                        if let Some(effect) = search_shortcut(logical, modifiers) {
                            return effect;
                        }
                        let effect = search_key(logical, modifiers, text);
                        if matches!(&effect, Effect::Search(_)) {
                            self.repeating_search_key = Some(physical);
                        }
                        effect
                    }
                    Some(_) => Effect::None,
                }
            }
            KeyAction::Repeat if self.repeating_search_key == Some(physical) => {
                search_key(logical, modifiers, text)
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
    (id, recipient): (u64, u64),
    pane: &mut impl Pane<Error = Error>,
    routing: &mut Routing,
    searching: bool,
    event: Event,
    has_splits: bool,
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
    let search_owns_key = searching && routing.release_owner(&event) != Some(id);
    let command = match event {
        Event::ImePreedit { text, .. } => return Ok(Effect::Preedit(text)),
        Event::ImeCommit { text, .. } | Event::ClipboardText { text, .. } if searching => {
            return Ok(Effect::Search(SearchAction::Insert(text)));
        }
        Event::ImeCommit { ref text, .. } => PaneCommand::Text(text.as_bytes()),
        Event::ClipboardText { text, .. } => PaneCommand::Paste(text),
        Event::Key {
            physical,
            logical,
            utf8,
            modifiers,
            action,
        } => {
            let symbol = logical.character();
            let shortcut = routing.shortcuts.match_key(
                physical,
                symbol,
                modifiers,
                action,
                has_splits,
                || pane.primary_screen(),
            )?;
            match shortcut {
                Some(Shortcut::Tab(action)) => return Ok(Effect::Tab(action)),
                Some(Shortcut::Pane(action)) => return Ok(Effect::Pane(action)),
                Some(Shortcut::Zoom(action)) => return Ok(Effect::Zoom(action)),
                shortcut if search_owns_key => {
                    return Ok(
                        routing.search_key(physical, logical, modifiers, utf8, action, shortcut)
                    );
                }
                Some(Shortcut::Copy) => return Ok(Effect::Copy),
                Some(Shortcut::Paste) => return Ok(Effect::ReadClipboard),
                Some(Shortcut::Search) => return Ok(Effect::Search(SearchAction::Open)),
                Some(Shortcut::ScrollPage { up }) => {
                    return send_input(pane, routing, id, PaneCommand::ScrollPage { up });
                }
                Some(Shortcut::Consume) => return Ok(Effect::None),
                None if !routing.terminal_key(physical, id, action) => return Ok(Effect::None),
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
    send_input(pane, routing, id, command)
}

fn send_input(
    pane: &mut impl Pane<Error = Error>,
    routing: &mut Routing,
    id: u64,
    command: PaneCommand<'_>,
) -> Result<Effect> {
    let press = match command {
        PaneCommand::Key {
            physical,
            unshifted,
            action: KeyAction::Press,
            ..
        } => Some((physical, unshifted)),
        _ => None,
    };
    let effect = send(pane, command)?;
    if let Some((physical, unshifted)) = press {
        routing.terminal_presses.push(HeldKey {
            physical,
            unshifted,
            pane: id,
            generation: routing.generation,
            reported: matches!(effect, PaneEffect::ReportedPress { .. }),
        });
    }
    match effect {
        PaneEffect::ViewportChanged
        | PaneEffect::ReportedPress {
            viewport_changed: true,
        } => Ok(Effect::ViewportChanged),
        _ => Ok(Effect::None),
    }
}

fn search_shortcut(logical: LogicalKey, mods: KeyModifiers) -> Option<Effect> {
    if mods.alt || mods.super_key {
        return None;
    }
    match logical {
        LogicalKey::Character('a' | 'A') if mods.control => {
            Some(Effect::Search(SearchAction::SelectAll))
        }
        LogicalKey::Character('v' | 'V') if mods.control => Some(Effect::ReadClipboard),
        LogicalKey::Insert if mods.shift && !mods.control => Some(Effect::ReadClipboard),
        _ => None,
    }
}

fn search_key(logical: LogicalKey, modifiers: KeyModifiers, text: Option<String>) -> Effect {
    let ctrl = modifiers.control && !modifiers.alt && !modifiers.super_key;
    let action = match logical {
        LogicalKey::Escape => SearchAction::Close,
        LogicalKey::Home => SearchAction::Home,
        LogicalKey::End => SearchAction::End,
        LogicalKey::Left => SearchAction::Move { right: false },
        LogicalKey::Right => SearchAction::Move { right: true },
        LogicalKey::Backspace => SearchAction::Backspace { word: ctrl },
        LogicalKey::Enter | LogicalKey::NumpadEnter | LogicalKey::F3 => SearchAction::Navigate {
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

/// End a reported press in the pane that received it.
pub(crate) fn release(
    pane: &mut impl Pane<Error = Error>,
    key: &HeldKey,
    modifiers: KeyModifiers,
) -> Result<()> {
    send(
        pane,
        PaneCommand::Key {
            physical: key.physical,
            unshifted: key.unshifted,
            action: KeyAction::Release,
            modifiers,
            text: "",
        },
    )
    .map(drop)
}

/// Tell the pane that the window gained or lost the keyboard; only mode 1004 reports it.
pub(crate) fn report_focus(pane: &mut impl Pane<Error = Error>, focused: bool) -> Result<()> {
    send(pane, PaneCommand::Focus(focused)).map(drop)
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

pub(crate) fn mouse_event(
    phase: MousePhase,
    modifiers: KeyModifiers,
    (cell, pixel, over_grid): (sumie_core::CellPosition, [f32; 2], bool),
) -> MouseEvent {
    MouseEvent {
        phase,
        cell,
        pixel,
        over_grid,
        modifiers,
        link: false,
    }
}

#[cfg(test)]
mod tests;
