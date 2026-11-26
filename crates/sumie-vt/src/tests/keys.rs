use crate::{KeyAction, KeyEncoder, KeyEvent, Terminal};

/// Terminal output that sets a mode, the key, its modifier bits, its unshifted symbol, and the
/// expected bytes.
type KeyCase<'a> = (&'a [u8], sumie_core::KeyCode, u16, Option<char>, &'a [u8]);

/// Encode each press with the text a platform passes: none with Control, and the unshifted
/// symbol only without Shift.
fn assert_key_bytes(cases: &[KeyCase<'_>]) {
    let mut encoder = KeyEncoder::new().unwrap();
    for &(mode, key, modifiers, unshifted, expected) in cases {
        let mut terminal = Terminal::new(10, 2).unwrap();
        terminal.write(mode);
        let event = KeyEvent {
            key: key as u8,
            action: KeyAction::Press,
            modifiers,
            consumed_modifiers: 0,
            unshifted,
            text: "",
        };
        let bytes = encoder.encode(&terminal, &event).unwrap();
        assert_eq!(
            bytes.escape_ascii().to_string(),
            expected.escape_ascii().to_string(),
            "{key:?} with modifiers {modifiers} after {}",
            mode.escape_ascii()
        );
    }
}

const SHIFT: u16 = 1;
const CTRL: u16 = 2;
const ALT: u16 = 4;

#[test]
fn modified_enter_is_cr_until_a_mode_asks_for_more() {
    use sumie_core::KeyCode::Enter;

    assert_key_bytes(&[
        (b"", Enter, SHIFT, None, b"\r"),
        (b"", Enter, CTRL, None, b"\r"),
        (b"", Enter, CTRL | SHIFT, None, b"\r"),
        (b"", Enter, ALT, None, b"\x1b\r"),
        (b"\x1b[>4;2m", Enter, SHIFT, None, b"\x1b[27;2;13~"),
        (b"\x1b[>4;2m", Enter, CTRL, None, b"\x1b[27;5;13~"),
        (b"\x1b[>1u", Enter, SHIFT, None, b"\x1b[13;2u"),
    ]);
}

#[test]
fn legacy_control_keys_send_their_c0_byte() {
    use sumie_core::KeyCode as K;

    assert_key_bytes(&[
        (b"", K::BracketLeft, CTRL, Some('['), b"\x1b"),
        (b"", K::KeyI, CTRL, Some('i'), b"\t"),
        (b"", K::KeyM, CTRL, Some('m'), b"\r"),
        (b"", K::KeyC, CTRL, Some('c'), b"\x03"),
        (b"\x1b[>1u", K::BracketLeft, CTRL, Some('['), b"\x1b[91;5u"),
    ]);
}
