#![forbid(unsafe_code)]
#![cfg(target_os = "linux")]

//! Owns the Wayland connection, windows, input, input methods, clipboard, and
//! fractional scaling. Does not match shortcuts, make business decisions, or
//! block the main loop on reads from the clipboard.
