use std::ffi::c_int;
use std::ptr::{null, null_mut};

use crate::{Error, Terminal, check, ffi};

#[derive(Clone, Copy)]
#[repr(i32)]
pub enum KeyAction {
    Release = 0,
    Press = 1,
    Repeat = 2,
}

/// `GHOSTTY_KEY_ENCODER_OPT_MACOS_OPTION_AS_ALT` and its `left` value. Terminal state cannot
/// supply it, and `set_from_terminal` resets it.
const MACOS_OPTION_AS_ALT: c_int = 7;
static OPTION_AS_ALT_LEFT: c_int = 2;

/// A mode-independent input event for libghostty-vt's key encoder.
pub struct KeyEvent<'a> {
    /// A Ghostty physical key code (`GhosttyKey`, 0 for unidentified).
    pub key: u8,
    pub action: KeyAction,
    /// A `GhosttyMods` mask (shift, control, alt, super, caps lock, num lock, and the side
    /// bits). On macOS only a left Alt (Option) is Meta; Ghostty ignores the side elsewhere.
    pub modifiers: u16,
    pub consumed_modifiers: u16,
    pub unshifted: Option<char>,
    /// Printable layout-dependent text, without control characters.
    pub text: &'a str,
}

/// Reusable native key encoder. Use on the terminal owner thread.
pub struct KeyEncoder {
    raw: ffi::KeyEncoder,
    event: ffi::KeyEvent,
    bytes: Vec<u8>,
}

struct ClearKeyText(ffi::KeyEvent);

impl Drop for ClearKeyText {
    fn drop(&mut self) {
        // SAFETY: The guard stays inside encode, which owns the live event for this call.
        // Ghostty accepts a null pointer with zero length and replaces the borrowed text.
        unsafe {
            ffi::ghostty_key_event_set_utf8(
                self.0,
                ffi::Str {
                    ptr: null(),
                    len: 0,
                },
            );
        }
    }
}

impl Terminal {
    /// Whether the active screen requests Kitty key releases.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the active keyboard flags.
    pub fn reports_key_events(&self) -> Result<bool, Error> {
        const KITTY_KEYBOARD_FLAGS: c_int = 8;
        const REPORT_EVENTS: u8 = 1 << 1;
        let mut flags = 0_u8;
        // SAFETY: KITTY_KEYBOARD_FLAGS writes a uint8_t to the provided output slot.
        check(unsafe {
            ffi::ghostty_terminal_get(self.raw, KITTY_KEYBOARD_FLAGS, (&raw mut flags).cast())
        })?;
        Ok(flags & REPORT_EVENTS != 0)
    }
}

impl KeyEncoder {
    /// # Errors
    /// Returns an error if Ghostty cannot allocate the encoder or event.
    pub fn new() -> Result<Self, Error> {
        let mut encoder = Self {
            raw: null_mut(),
            event: null_mut(),
            bytes: vec![0; 128],
        };
        // SAFETY: These constructors initialize the handle slots and accept the default allocator.
        unsafe {
            check(ffi::ghostty_key_encoder_new(null(), &raw mut encoder.raw))?;
            check(ffi::ghostty_key_event_new(null(), &raw mut encoder.event))?;
        }
        Ok(encoder)
    }

    /// Encode a key using the terminal's active modes. Empty output means no input was emitted.
    ///
    /// # Errors
    /// Returns an error for an invalid key code or a native encoding failure.
    pub fn encode(&mut self, terminal: &Terminal, key: &KeyEvent<'_>) -> Result<&[u8], Error> {
        let mut written = 0;
        // SAFETY: All handles are live, the option value is a static `GhosttyOptionAsAlt`, and the
        // output slice is writable for its full length.
        unsafe {
            check(ffi::ghostty_key_encoder_set_from_terminal(
                self.raw,
                terminal.raw,
            ))?;
            check(ffi::ghostty_key_encoder_set(
                self.raw,
                MACOS_OPTION_AS_ALT,
                (&raw const OPTION_AS_ALT_LEFT).cast(),
            ))?;
            check(ffi::ghostty_key_event_set_key(
                self.event,
                c_int::from(key.key),
            ))?;
            check(ffi::ghostty_key_event_set_action(
                self.event,
                key.action as c_int,
            ))?;
            check(ffi::ghostty_key_event_set_mods(self.event, key.modifiers))?;
            check(ffi::ghostty_key_event_set_consumed_mods(
                self.event,
                key.consumed_modifiers,
            ))?;
            check(ffi::ghostty_key_event_set_unshifted_codepoint(
                self.event,
                key.unshifted.map_or(0, u32::from),
            ))?;
            let _text = ClearKeyText(self.event);
            check(ffi::ghostty_key_event_set_utf8(
                self.event,
                ffi::Str {
                    ptr: key.text.as_ptr(),
                    len: key.text.len(),
                },
            ))?;
            let mut result = self.encode_into(&mut written);
            if result == ffi::OUT_OF_SPACE {
                self.bytes.resize(written, 0);
                result = self.encode_into(&mut written);
            }
            check(result)?;
        }
        Ok(&self.bytes[..written])
    }

    unsafe fn encode_into(&mut self, written: &mut usize) -> c_int {
        // SAFETY: The caller has initialized the event and keeps its input text alive.
        unsafe {
            ffi::ghostty_key_encoder_encode(
                self.raw,
                self.event,
                self.bytes.as_mut_ptr(),
                self.bytes.len(),
                written,
            )
        }
    }
}

impl Drop for KeyEncoder {
    fn drop(&mut self) {
        // SAFETY: Each handle is uniquely owned or null after partial initialization.
        unsafe {
            ffi::ghostty_key_event_free(self.event);
            ffi::ghostty_key_encoder_free(self.raw);
        }
    }
}
