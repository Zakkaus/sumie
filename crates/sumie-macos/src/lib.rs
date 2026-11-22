#![forbid(unsafe_code)]
#![cfg(target_os = "macos")]

//! Owns macOS windows, input, input methods, and clipboard access. Does not
//! match shortcuts, make business decisions, or block the main loop on reads
//! from the clipboard.
