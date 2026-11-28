//! winit's keys, scrolling and positions, and finished pastes, in the shared event types.

use sumie_core::mouse::MousePhase;
use sumie_macos::{Scroll, Symbol};

use super::super::pasteboard::Pasted;
use crate::platform::{Event, LogicalKey, Position};

/// Trackpad points per wheel notch, as Wayland counts logical units.
const POINTS_PER_NOTCH: f64 = 10.0;
const VALUE120_PER_NOTCH: f64 = 120.0;

/// Command keys go to the shared shortcut table, which keeps them out of the PTY; the menu has
/// already taken its own key equivalents, such as Quit.
pub(super) fn key_event(key: sumie_macos::Key) -> Event {
    Event::Key {
        physical: key.physical,
        logical: logical_key(key.symbol),
        utf8: key.text,
        modifiers: key.modifiers,
        action: key.action,
    }
}

/// The one conversion of winit's scrolling into the shared value120 units, where positive
/// scrolls down: a line is a notch, and a trackpad moves a notch per ten points, as Wayland
/// counts logical units. Fractions stay for the core's remainder.
pub(super) fn wheel_phase(scroll: Scroll, scale: f64) -> MousePhase {
    match scroll {
        Scroll::Lines(lines) => MousePhase::Wheel(-lines * VALUE120_PER_NOTCH),
        Scroll::Pixels(pixels) => {
            MousePhase::Touch(-pixels / scale * (VALUE120_PER_NOTCH / POINTS_PER_NOTCH))
        }
        Scroll::Ended => MousePhase::TouchEnd,
    }
}

/// A finished paste keeps the recipient its request named, even if focus changed.
pub(super) fn paste_event(Pasted { text, recipient }: Pasted, current: u64) -> Event {
    if recipient != current {
        tracing::debug!("a paste arrived for a recipient that is no longer focused");
    }
    Event::ClipboardText {
        text,
        generation: recipient,
    }
}

const fn logical_key(symbol: Symbol) -> LogicalKey {
    match symbol {
        Symbol::Character(ch) => LogicalKey::Character(ch),
        Symbol::Escape => LogicalKey::Escape,
        Symbol::Enter => LogicalKey::Enter,
        Symbol::NumpadEnter => LogicalKey::NumpadEnter,
        Symbol::Backspace => LogicalKey::Backspace,
        Symbol::Insert => LogicalKey::Insert,
        Symbol::Home => LogicalKey::Home,
        Symbol::End => LogicalKey::End,
        Symbol::Left => LogicalKey::Left,
        Symbol::Right => LogicalKey::Right,
        Symbol::F3 => LogicalKey::F3,
        Symbol::Other(ch) => LogicalKey::Other(ch),
    }
}

pub(super) const fn device_px([x, y]: [f64; 2]) -> Position {
    Position::DevicePx(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_deltas_convert_once_and_keep_their_fractions() {
        for (scroll, scale, expected) in [
            (Scroll::Lines(1.0), 2.0, MousePhase::Wheel(-120.0)),
            (Scroll::Lines(-0.25), 2.0, MousePhase::Wheel(30.0)),
            // 5 points at 2x is half a notch; at 1x the same backing pixels are a full one.
            (Scroll::Pixels(-10.0), 2.0, MousePhase::Touch(60.0)),
            (Scroll::Pixels(-10.0), 1.0, MousePhase::Touch(120.0)),
            (Scroll::Pixels(1.5), 1.5, MousePhase::Touch(-12.0)),
        ] {
            let got = wheel_phase(scroll, scale);
            let same = match (got, expected) {
                (MousePhase::Wheel(a), MousePhase::Wheel(b))
                | (MousePhase::Touch(a), MousePhase::Touch(b)) => (a - b).abs() < 1e-9,
                _ => false,
            };
            assert!(same, "{scroll:?} at {scale}");
        }
        assert!(matches!(
            wheel_phase(Scroll::Ended, 2.0),
            MousePhase::TouchEnd
        ));
    }

    #[test]
    fn a_paste_keeps_its_request_generation() {
        for (recipient, current) in [(4, 4), (4, 5)] {
            let pasted = Pasted {
                text: "paste".into(),
                recipient,
            };
            assert!(
                matches!(
                    paste_event(pasted, current),
                    Event::ClipboardText { ref text, generation } if text == "paste" && generation == recipient
                ),
                "requested for {recipient}, arrived at {current}"
            );
        }
    }
}
