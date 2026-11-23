use std::ffi::c_void;

use crate::{Error, Terminal, check, effects::Effects, ffi};

impl Terminal {
    /// Set the answers to XTVERSION and the XTGETTCAP `TN` query.
    ///
    /// # Errors
    /// Returns an error if Ghostty rejects the terminfo name (maximum 128 bytes).
    pub fn set_identity(&mut self, version: &'static str, term: &[u8]) -> Result<(), Error> {
        let name = ffi::Str {
            ptr: term.as_ptr(),
            len: term.len(),
        };
        // SAFETY: The terminal and Effects box are exclusively borrowed. Ghostty copies
        // the name during this call, and the callback returns a static string.
        unsafe {
            check(ffi::ghostty_terminal_set(
                self.raw,
                ffi::TERMINAL_OPT_TERMINFO_NAME,
                (&raw const name).cast(),
            ))?;
            (*self.effects).version = version;
            check(ffi::ghostty_terminal_set(
                self.raw,
                ffi::TERMINAL_OPT_XTVERSION,
                version_reply as *const c_void,
            ))
        }
    }
}

unsafe extern "C" fn version_reply(_: ffi::Terminal, data: *mut c_void) -> ffi::Str {
    // SAFETY: The synchronous callback receives the terminal's live Effects box.
    let version = unsafe { (*data.cast::<Effects>()).version };
    ffi::Str {
        ptr: version.as_ptr(),
        len: version.len(),
    }
}
