use sumie_core::{KeyAction, KeyCode, KeyModifiers, PhysicalKey};

use super::{Composition, Ime, Input};
use crate::{Key, Symbol};

const PINYIN: &str = "com.apple.inputmethod.TCIM.Pinyin";

fn key(code: KeyCode, ch: char, action: KeyAction) -> Key {
    Key {
        physical: PhysicalKey::Code(code),
        symbol: Symbol::Character(ch),
        text: (!matches!(action, KeyAction::Release)).then(|| ch.to_string()),
        modifiers: KeyModifiers::default(),
        action,
    }
}

fn preedit(text: &str) -> Ime {
    Ime::Preedit {
        text: text.into(),
        cursor: Some((text.len(), text.len())),
    }
}

fn empty() -> Ime {
    Ime::Preedit {
        text: String::new(),
        cursor: None,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Release(PhysicalKey, KeyModifiers),
    Preedit(String, u64),
    Commit(String, u64),
}

fn seen(inputs: Vec<Input>) -> Vec<Seen> {
    inputs
        .into_iter()
        .map(|input| match input {
            Input::Key(key) => {
                assert!(matches!(key.action, KeyAction::Release));
                assert!(key.text.is_none());
                Seen::Release(key.physical, key.modifiers)
            }
            Input::Preedit {
                text, generation, ..
            } => Seen::Preedit(text, generation),
            Input::Commit { text, generation } => Seen::Commit(text, generation),
        })
        .collect()
}

fn feed(composition: &mut Composition, ime: Ime) -> Vec<Seen> {
    let mut out = Vec::new();
    composition.ime(ime, Some(PINYIN.into()), &mut out);
    seen(out)
}

#[test]
fn a_composition_keeps_its_owner_through_the_empty_preedit_and_commits_once() {
    let mut composition = Composition::default();
    composition.set_generation(3);
    assert_eq!(
        feed(&mut composition, preedit("ㄅ")),
        [Seen::Preedit("ㄅ".into(), 3)]
    );
    assert_eq!(
        feed(&mut composition, empty()),
        [Seen::Preedit(String::new(), 3)]
    );
    assert_eq!(
        feed(&mut composition, Ime::Commit("把".into())),
        [Seen::Commit("把".into(), 3)]
    );
    // A second commit without a new composition has no owner.
    assert_eq!(feed(&mut composition, Ime::Commit("把".into())), []);
    // Nor does an empty preedit after it.
    assert_eq!(feed(&mut composition, empty()), []);
}

#[test]
fn a_new_generation_fences_the_old_composition() {
    let mut composition = Composition::default();
    composition.set_generation(1);
    feed(&mut composition, preedit("ㄅ"));
    assert!(composition.set_generation(2));
    assert!(!composition.set_generation(2));
    assert_eq!(feed(&mut composition, empty()), []);
    assert_eq!(feed(&mut composition, Ime::Commit("把".into())), []);
    // The next composition belongs to the new recipient.
    assert_eq!(
        feed(&mut composition, preedit("ㄆ")),
        [Seen::Preedit("ㄆ".into(), 2)]
    );
    assert_eq!(
        feed(&mut composition, Ime::Commit("葡".into())),
        [Seen::Commit("葡".into(), 2)]
    );
}

#[test]
fn cancelling_clears_the_overlay_once_and_drops_the_late_commit() {
    let mut composition = Composition::default();
    feed(&mut composition, preedit("ni"));
    assert!(matches!(
        composition.cancel(),
        Some(Input::Preedit { text, generation: 0, .. }) if text.is_empty()
    ));
    assert!(composition.cancel().is_none());
    assert_eq!(feed(&mut composition, Ime::Commit("你".into())), []);
    feed(&mut composition, preedit("ni"));
    assert_eq!(
        feed(&mut composition, Ime::Disabled),
        [Seen::Preedit(String::new(), 0)]
    );
    assert_eq!(feed(&mut composition, Ime::Commit("你".into())), []);
}

#[test]
fn a_commit_from_another_input_source_is_dropped() {
    let mut composition = Composition::default();
    feed(&mut composition, preedit("ni"));
    let mut out = Vec::new();
    composition.ime(
        Ime::Commit("ni".into()),
        Some("com.apple.keylayout.ABC".into()),
        &mut out,
    );
    assert_eq!(seen(out), [Seen::Preedit(String::new(), 0)]);
    // A composition in the new source starts its own owner.
    let mut out = Vec::new();
    composition.ime(
        preedit("a"),
        Some("com.apple.keylayout.ABC".into()),
        &mut out,
    );
    composition.ime(
        Ime::Commit("á".into()),
        Some("com.apple.keylayout.ABC".into()),
        &mut out,
    );
    assert_eq!(
        seen(out),
        [Seen::Preedit("a".into(), 0), Seen::Commit("á".into(), 0)]
    );
}

#[test]
fn the_composition_releases_held_presses_once() {
    let mut composition = Composition::default();
    let a = PhysicalKey::Code(KeyCode::KeyA);
    let alt = KeyModifiers {
        alt: true,
        right_alt: true,
        ..KeyModifiers::default()
    };
    composition.key(&key(KeyCode::KeyA, 'a', KeyAction::Press));
    composition.set_modifiers(alt);
    assert_eq!(
        feed(&mut composition, preedit("´")),
        [Seen::Release(a, alt), Seen::Preedit("´".into(), 0)]
    );
    // Later preedits release nothing more.
    composition.key(&key(KeyCode::KeyA, 'a', KeyAction::Repeat));
    assert_eq!(
        feed(&mut composition, preedit("´´")),
        [Seen::Preedit("´´".into(), 0)]
    );
    // A press released before the composition took over is not released again.
    composition.key(&key(KeyCode::KeyB, 'b', KeyAction::Press));
    composition.key(&key(KeyCode::KeyB, 'b', KeyAction::Release));
    assert_eq!(
        feed(&mut composition, preedit("ㄅ")),
        [Seen::Preedit("ㄅ".into(), 0)]
    );
}

#[test]
fn a_preedit_cursor_off_a_character_boundary_is_hidden() {
    let mut composition = Composition::default();
    let mut out = Vec::new();
    for cursor in [(0, 3), (1, 3), (3, 1), (0, 4)] {
        composition.ime(
            Ime::Preedit {
                text: "ㄅa".into(),
                cursor: Some(cursor),
            },
            Some(PINYIN.into()),
            &mut out,
        );
    }
    let cursors: Vec<_> = out
        .into_iter()
        .map(|input| match input {
            Input::Preedit { cursor, .. } => cursor,
            _ => panic!("only preedits"),
        })
        .collect();
    assert_eq!(cursors, [Some((0, 3)), None, None, Some((0, 4))]);
}
