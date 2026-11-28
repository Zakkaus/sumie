use std::ffi::c_int;
use std::ptr::{from_mut, null};

use crate::{Error, Terminal, check, ffi};

const DATA_CURSOR_AT_PROMPT: c_int = 39;
const DATA_PWD: c_int = 13;

impl Terminal {
    /// `None` until the primary screen has received an OSC 133 prompt or input mark.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the cursor state.
    pub fn semantic_prompt(&self) -> Result<Option<bool>, Error> {
        // SAFETY: The bridge only reads the live terminal's primary screen.
        if unsafe { ffi::sumie_semantic_prompt_seen(self.raw) } {
            self.cursor_at_prompt().map(Some)
        } else {
            Ok(None)
        }
    }

    /// Whether the cursor is at an OSC 133 prompt or input area on the primary screen.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the cursor state.
    pub fn cursor_at_prompt(&self) -> Result<bool, Error> {
        let mut prompt = false;
        // SAFETY: The live terminal writes a C bool for CURSOR_AT_PROMPT.
        unsafe {
            check(ffi::ghostty_terminal_get(
                self.raw,
                DATA_CURSOR_AT_PROMPT,
                from_mut(&mut prompt).cast(),
            ))?;
        }
        Ok(prompt)
    }

    /// Inspect the last OSC 7 URI before the next terminal mutation. An empty
    /// slice means the directory was never reported or was cleared.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the working directory.
    pub fn with_pwd<T>(&self, inspect: impl FnOnce(&[u8]) -> T) -> Result<T, Error> {
        let mut pwd = ffi::Str {
            ptr: null(),
            len: 0,
        };
        // SAFETY: PWD writes a GhosttyString valid until the next mutating call.
        // The closure cannot retain the native slice; an empty string may be null.
        unsafe {
            check(ffi::ghostty_terminal_get(
                self.raw,
                DATA_PWD,
                from_mut(&mut pwd).cast(),
            ))?;
            Ok(inspect(if pwd.len == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(pwd.ptr, pwd.len)
            }))
        }
    }
}
