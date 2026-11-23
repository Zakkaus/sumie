use std::collections::VecDeque;

use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop, protocol::wl_seat::WlSeat,
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3::{self, ContentHint, ContentPurpose, ZwpTextInputV3},
};

use crate::{Event, platform::State};

/// Cursor rectangle in surface-local logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Default)]
struct Batch {
    commit: Option<String>,
    preedit: String,
    cursor: Option<(u32, u32)>,
}

#[derive(Default)]
struct ImeState {
    serial: u32,
    wait_for_match: bool,
    enabled: bool,
    sent_rect: Option<CursorRect>,
    pending: Batch,
    has_preedit: bool,
}

impl ImeState {
    fn enable(&mut self) {
        self.enabled = true;
        self.wait_for_match = false;
        self.sent_rect = None;
    }

    fn disable(&mut self, events: &mut VecDeque<Event>) {
        self.enabled = false;
        if std::mem::take(&mut self.has_preedit) {
            events.push_back(Event::ImePreedit {
                text: String::new(),
                cursor: None,
            });
        }
    }

    fn next_cursor(&mut self, desired: Option<CursorRect>) -> Option<CursorRect> {
        let rect = desired?;
        if !self.enabled || self.wait_for_match || self.sent_rect == Some(rect) {
            return None;
        }
        self.sent_rect = Some(rect);
        Some(rect)
    }

    fn done(&mut self, serial: u32, events: &mut VecDeque<Event>) {
        // A stale serial defers outgoing state, never incoming text.
        self.wait_for_match = serial != self.serial;
        let Batch {
            commit,
            preedit,
            cursor,
        } = std::mem::take(&mut self.pending);
        if let Some(text) = commit {
            events.push_back(Event::ImeCommit { text });
        }
        let has_preedit = self.enabled && !preedit.is_empty();
        if std::mem::replace(&mut self.has_preedit, has_preedit) || has_preedit {
            events.push_back(Event::ImePreedit {
                text: preedit,
                cursor,
            });
        }
    }
}

pub(crate) struct TextInput {
    proxy: ZwpTextInputV3,
    state: ImeState,
    entered: bool,
}

impl TextInput {
    pub(crate) fn new(
        manager: &ZwpTextInputManagerV3,
        seat: &WlSeat,
        qh: &QueueHandle<State>,
    ) -> Self {
        Self {
            proxy: manager.get_text_input(seat, qh, ()),
            state: ImeState::default(),
            entered: false,
        }
    }

    fn commit(&mut self) -> bool {
        self.proxy.commit();
        self.state.serial = self.state.serial.wrapping_add(1);
        self.proxy.version() >= 2
    }

    fn enter(&mut self, desired: Option<CursorRect>) -> bool {
        self.entered = true;
        self.state.enable();
        self.proxy.enable();
        self.proxy
            .set_content_type(ContentHint::None, ContentPurpose::Terminal);
        self.send_cursor(desired);
        self.commit()
    }

    fn leave(&mut self, events: &mut VecDeque<Event>) {
        self.entered = false;
        self.state.pending = Batch::default();
        self.state.disable(events);
    }

    fn send_cursor(&mut self, desired: Option<CursorRect>) -> bool {
        let Some(rect) = self.state.next_cursor(desired) else {
            return false;
        };
        self.proxy
            .set_cursor_rectangle(rect.x, rect.y, rect.width, rect.height);
        true
    }

    fn update_cursor(&mut self, desired: Option<CursorRect>) -> bool {
        self.send_cursor(desired) && self.commit()
    }
}

impl Drop for TextInput {
    fn drop(&mut self) {
        self.proxy.destroy();
    }
}

impl State {
    pub(crate) fn set_cursor_rectangle(&mut self, rect: CursorRect) {
        self.cursor_rect = Some(rect);
        if let Some(ime) = self
            .keyboard
            .as_mut()
            .and_then(|keyboard| keyboard.ime.as_mut())
            && ime.update_cursor(Some(rect))
        {
            self.commit_ime_surface();
        }
    }

    fn commit_ime_surface(&mut self) {
        if self
            .presentation
            .as_mut()
            .is_none_or(|presentation| !presentation.defer_surface_commit())
        {
            self.wl_surface().commit();
        }
    }

    pub(crate) fn disable_text_input(&mut self) {
        if let Some(ime) = self
            .keyboard
            .as_mut()
            .and_then(|keyboard| keyboard.ime.as_mut())
            && ime.state.enabled
        {
            // If text-input leave arrives first, the compositor already disabled the object.
            ime.proxy.disable();
            let commit_surface = ime.commit();
            ime.state.disable(&mut self.events);
            if commit_surface {
                self.commit_ime_surface();
            }
        }
    }

    pub(crate) fn leave_text_input(&mut self) {
        if let Some(ime) = self
            .keyboard
            .as_mut()
            .and_then(|keyboard| keyboard.ime.as_mut())
        {
            ime.leave(&mut self.events);
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(ime) = state
            .keyboard
            .as_mut()
            .and_then(|keyboard| keyboard.ime.as_mut())
        else {
            return;
        };
        if &ime.proxy != proxy {
            return;
        }
        let surface = state.window.wl_surface();
        let commit_surface = match event {
            zwp_text_input_v3::Event::Enter { surface: entered } if &entered == surface => {
                ime.enter(state.cursor_rect)
            }
            zwp_text_input_v3::Event::Leave { surface: left } if &left == surface => {
                ime.leave(&mut state.events);
                false
            }
            zwp_text_input_v3::Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } if ime.entered => {
                ime.state.pending.preedit = text.unwrap_or_default();
                ime.state.pending.cursor = u32::try_from(cursor_begin)
                    .ok()
                    .zip(u32::try_from(cursor_end).ok());
                false
            }
            zwp_text_input_v3::Event::CommitString { text } if ime.entered => {
                ime.state.pending.commit = text;
                false
            }
            zwp_text_input_v3::Event::Done { serial } if ime.entered => {
                ime.state.done(serial, &mut state.events);
                ime.update_cursor(state.cursor_rect)
            }
            zwp_text_input_v3::Event::DeleteSurroundingText { .. } => {
                tracing::debug!("ignoring IME request to delete surrounding text");
                false
            }
            _ => false,
        };
        if commit_surface {
            state.commit_ime_surface();
        }
    }
}

delegate_noop!(State: ignore ZwpTextInputManagerV3);

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: CursorRect = CursorRect {
        x: 0,
        y: 0,
        width: 1,
        height: 16,
    };
    const NEXT: CursorRect = CursorRect { x: 8, ..FIRST };

    #[test]
    fn cursor_changes_only_while_focused_and_once_per_rectangle() {
        let mut ime = ImeState::default();
        let mut events = VecDeque::new();
        for (focused, rect, sent) in [
            (false, FIRST, false),
            (true, FIRST, true),
            (true, FIRST, false),
            (true, NEXT, true),
            (false, FIRST, false),
            (true, NEXT, true),
            (true, NEXT, false),
        ] {
            match (ime.enabled, focused) {
                (false, true) => ime.enable(),
                (true, false) => ime.disable(&mut events),
                _ => {}
            }
            assert_eq!(ime.next_cursor(Some(rect)), sent.then_some(rect));
        }
    }

    #[test]
    fn done_applies_text_even_when_serial_defers_outgoing_state() {
        let mut ime = ImeState {
            serial: 2,
            ..ImeState::default()
        };
        ime.enable();
        let mut events = VecDeque::new();
        for (serial, commit, preedit, sync) in [(1, Some("字"), "ㄅ", false), (2, None, "", true)]
        {
            ime.pending = Batch {
                commit: commit.map(str::to_owned),
                preedit: preedit.to_owned(),
                cursor: (!preedit.is_empty()).then_some((0, 3)),
            };
            ime.done(serial, &mut events);
            if let Some(commit) = commit {
                assert!(
                    matches!(events.pop_front(), Some(Event::ImeCommit { text }) if text == commit)
                );
            }
            assert!(
                matches!(events.pop_front(), Some(Event::ImePreedit { text, cursor })
                if text == preedit && cursor == (!preedit.is_empty()).then_some((0, 3)))
            );
            assert!(events.is_empty());
            assert_eq!(ime.next_cursor(Some(NEXT)), sync.then_some(NEXT));
        }
    }
}
