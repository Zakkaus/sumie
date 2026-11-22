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

/// A mode-independent input event for libghostty-vt's key encoder.
pub struct KeyEvent<'a> {
    /// A Ghostty physical key code (`GhosttyKey`, 0 for unidentified).
    pub key: u8,
    pub action: KeyAction,
    /// A `GhosttyMods` mask (shift, control, alt, super, caps lock, num lock).
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
        // SAFETY: All handles are live and the output slice is writable for its full length.
        unsafe {
            check(ffi::ghostty_key_encoder_set_from_terminal(
                self.raw,
                terminal.raw,
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
