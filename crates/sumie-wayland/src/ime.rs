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
    enabled_before_leave: Option<u32>,
}

#[derive(Default)]
struct ImeState {
    serial: u32,
    wait_for_match: bool,
    enabled_at: Option<u32>,
    sent_rect: Option<CursorRect>,
    pending: Batch,
    has_preedit: bool,
}

impl ImeState {
    fn enable(&mut self) {
        self.enabled_at = Some(self.serial.wrapping_add(1));
        self.wait_for_match = false;
        self.sent_rect = None;
        self.pending = Batch::default();
        self.has_preedit = false;
    }

    fn disable(&mut self, generation: u64, events: &mut VecDeque<Event>) {
        self.enabled_at = None;
        self.pending = Batch::default();
        if std::mem::take(&mut self.has_preedit) {
            events.push_back(Event::ImePreedit {
                text: String::new(),
                cursor: None,
                generation,
            });
        }
    }

    fn keyboard_leave(&mut self, generation: u64, events: &mut VecDeque<Event>) {
        // Keyboard leave can precede done for an already queued commit.
        let commit = self.pending.commit.take();
        let enabled_before_leave = self.enabled_at;
        self.disable(generation, events);
        self.pending.commit = commit;
        self.pending.enabled_before_leave = enabled_before_leave;
    }

    /// Keyboard leave can arrive before the IME's last commit; keep accepting text
    /// until text-input leave disables the object.
    fn accepts_text(&self) -> bool {
        self.enabled_at.is_some() || self.pending.enabled_before_leave.is_some()
    }

    fn next_cursor(&mut self, desired: Option<CursorRect>) -> Option<CursorRect> {
        let rect = desired?;
        if self.enabled_at.is_none() || self.wait_for_match || self.sent_rect == Some(rect) {
            return None;
        }
        self.sent_rect = Some(rect);
        Some(rect)
    }

    fn done(&mut self, serial: u32, generation: u64, events: &mut VecDeque<Event>) {
        let Batch {
            commit,
            preedit,
            cursor,
            enabled_before_leave,
        } = std::mem::take(&mut self.pending);
        let Some(enabled_at) = self.enabled_at.or(enabled_before_leave) else {
            return;
        };
        // Only a serial from this recipient may apply text. A mismatch within
        // that interval still applies the batch, but defers outgoing state.
        if self.serial.wrapping_sub(serial) > self.serial.wrapping_sub(enabled_at) {
            return;
        }
        self.wait_for_match = serial != self.serial;
        if let Some(text) = commit {
            events.push_back(Event::ImeCommit { text, generation });
        }
        let has_preedit = !preedit.is_empty();
        if std::mem::replace(&mut self.has_preedit, has_preedit) || has_preedit {
            events.push_back(Event::ImePreedit {
                text: preedit,
                cursor,
                generation,
            });
        }
    }
}

pub(crate) struct TextInput {
    proxy: ZwpTextInputV3,
    state: ImeState,
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
        }
    }

    fn commit(&mut self) -> bool {
        self.proxy.commit();
        self.state.serial = self.state.serial.wrapping_add(1);
        self.proxy.version() >= 2
    }

    fn enable(&mut self, desired: Option<CursorRect>) -> bool {
        self.state.enable();
        self.proxy.enable();
        self.proxy
            .set_content_type(ContentHint::None, ContentPurpose::Terminal);
        self.send_cursor(desired);
        self.commit()
    }

    fn disable(&mut self) -> bool {
        self.proxy.disable();
        self.commit()
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
    pub(crate) fn set_input_generation(&mut self, generation: u64) {
        if self.input_generation == generation {
            return;
        }
        self.input_generation = generation;
        self.cursor_rect = None;
        let Some(ime) = self
            .keyboard
            .as_mut()
            .and_then(|keyboard| keyboard.ime.as_mut())
        else {
            return;
        };
        if ime.state.enabled_at.is_none() {
            ime.state.pending = Batch::default();
            return;
        }
        ime.disable();
        if ime.enable(None) {
            self.commit_ime_surface();
        }
    }

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
            && ime.state.enabled_at.is_some()
        {
            // If text-input leave arrives first, the compositor already disabled the object.
            ime.state
                .keyboard_leave(self.input_generation, &mut self.events);
            if ime.disable() {
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
            ime.state.disable(self.input_generation, &mut self.events);
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
                ime.enable(state.cursor_rect)
            }
            zwp_text_input_v3::Event::Leave { surface: left } if &left == surface => {
                ime.state.disable(state.input_generation, &mut state.events);
                false
            }
            zwp_text_input_v3::Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } if ime.state.accepts_text() => {
                ime.state.pending.preedit = text.unwrap_or_default();
                ime.state.pending.cursor = u32::try_from(cursor_begin)
                    .ok()
                    .zip(u32::try_from(cursor_end).ok());
                false
            }
            zwp_text_input_v3::Event::CommitString { text } if ime.state.accepts_text() => {
                ime.state.pending.commit = text;
                false
            }
            zwp_text_input_v3::Event::Done { serial } => {
                ime.state
                    .done(serial, state.input_generation, &mut state.events);
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
            match (ime.enabled_at.is_some(), focused) {
                (false, true) => ime.enable(),
                (true, false) => ime.disable(0, &mut events),
                _ => {}
            }
            assert_eq!(ime.next_cursor(Some(rect)), sent.then_some(rect));
        }
    }

    #[test]
    fn done_applies_text_even_when_serial_defers_outgoing_state() {
        let mut ime = ImeState::default();
        ime.enable();
        ime.serial = 2;
        let mut events = VecDeque::new();
        let generation = 1;
        for (serial, commit, preedit, sync, keyboard_left) in [
            (1, Some("字"), "ㄅ", false, false),
            (2, None, "", true, false),
            (2, Some("水"), "ㄅ", false, true),
        ] {
            ime.pending = Batch {
                commit: commit.map(str::to_owned),
                preedit: preedit.to_owned(),
                cursor: (!preedit.is_empty()).then_some((0, 3)),
                ..Batch::default()
            };
            if keyboard_left {
                ime.keyboard_leave(generation, &mut events);
                ime.serial = ime.serial.wrapping_add(1);
            }
            ime.done(serial, generation, &mut events);
            if let Some(commit) = commit {
                assert!(matches!(
                    events.pop_front(),
                    Some(Event::ImeCommit { text, generation: g }) if text == commit && g == generation
                ));
            }
            if !keyboard_left {
                assert!(
                    matches!(events.pop_front(), Some(Event::ImePreedit { text, cursor, .. })
                    if text == preedit && cursor == (!preedit.is_empty()).then_some((0, 3)))
                );
            }
            assert!(events.is_empty());
            let rect = if keyboard_left { FIRST } else { NEXT };
            assert_eq!(ime.next_cursor(Some(rect)), sync.then_some(rect));
        }
    }

    #[test]
    fn commit_after_keyboard_leave_is_delivered() {
        let mut ime = ImeState::default();
        ime.enable();
        let mut events = VecDeque::new();
        ime.keyboard_leave(0, &mut events);
        assert!(ime.accepts_text());
        ime.pending.commit = Some("水".to_owned());
        ime.done(ime.serial, 0, &mut events);
        assert!(matches!(events.pop_front(), Some(Event::ImeCommit { text, .. }) if text == "水"));
    }

    #[test]
    fn reenable_discards_pending_and_old_serials_across_wrap() {
        let mut ime = ImeState::default();
        let mut events = VecDeque::new();
        for (generation, previous, enabled) in [(1, u32::MAX - 1, 0), (2, 0, 2)] {
            let disabled = previous.wrapping_add(1);
            ime.serial = disabled;
            ime.pending.commit = Some("partial".into());
            ime.enable();
            ime.serial = enabled;
            ime.done(enabled, generation, &mut events);
            assert!(events.is_empty());

            for serial in [previous, disabled, enabled] {
                ime.pending.commit = Some("水".into());
                ime.done(serial, generation, &mut events);
                if serial == enabled {
                    assert!(matches!(
                        events.pop_front(),
                        Some(Event::ImeCommit { text, generation: g }) if g == generation && text == "水"
                    ));
                }
                assert!(events.is_empty());
            }
        }
    }
}
