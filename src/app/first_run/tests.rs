use std::io::ErrorKind;

use sumie_core::{KeyAction, KeyCode, KeyModifiers, PhysicalKey, mouse::Button};
use sumie_vt::codepoint_width;

use super::*;
use crate::platform::{LogicalKey, Position};

fn key(action: KeyAction) -> Event {
    Event::Key {
        physical: PhysicalKey::Code(KeyCode::KeyA),
        logical: LogicalKey::Character('a'),
        utf8: Some("a".into()),
        modifiers: KeyModifiers::default(),
        action,
    }
}

fn pointer(phase: MousePhase) -> Event {
    Event::Pointer(PointerEvent {
        phase,
        position: Position::SurfaceLogical(1.0, 1.0),
        modifiers: KeyModifiers::default(),
    })
}

#[test]
fn only_the_first_claim_shows_the_hint() {
    let dir = std::env::temp_dir().join(format!("sumie-first-run-{}", std::process::id()));
    assert!(claim(&dir).is_ok());
    assert_eq!(claim(&dir).unwrap_err().kind(), ErrorKind::AlreadyExists);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_press_dismisses_the_hint_and_is_consumed() {
    let mut hint = Some("hint".to_owned());
    assert!(!consume(&mut hint, &key(KeyAction::Release)));
    assert!(!consume(&mut hint, &pointer(MousePhase::Motion)));
    assert!(hint.is_some());
    assert!(consume(&mut hint, &key(KeyAction::Press)));
    assert!(hint.is_none());
    assert!(!consume(&mut hint, &key(KeyAction::Press)));

    let mut hint = Some("hint".to_owned());
    assert!(consume(
        &mut hint,
        &pointer(MousePhase::Press(Button::Left, 0))
    ));
}

#[test]
fn labels_come_from_the_shortcut_table() {
    let (prefix, tab) = letter_chord(POLICY, Shortcut::Tab(TabAction::Open)).unwrap();
    let text = text(Locale::En, POLICY, "~/.config/sumie");
    assert!(text.starts_with(&format!("{prefix}: {tab} tab")), "{text}");
    assert!(text.contains("copy/paste"), "{text}");
}

#[test]
fn the_line_fits_80_columns() {
    for locale in [Locale::En, Locale::ZhTw] {
        for platform in [Platform::Linux, Platform::MacOS] {
            let text = text(locale, platform, "~/.config/sumie");
            let width: u32 = text.chars().map(|ch| u32::from(codepoint_width(ch))).sum();
            assert!(width <= 80, "{width} columns: {text}");
        }
    }
}

#[test]
fn an_input_method_commit_dismisses_without_being_consumed() {
    let mut hint = Some("hint".to_owned());
    let commit = Event::ImeCommit {
        text: "あ".into(),
        recipient: 0,
    };
    assert!(!consume(&mut hint, &commit));
    assert!(hint.is_none());
}
