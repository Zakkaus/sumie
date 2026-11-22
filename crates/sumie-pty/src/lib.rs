#![forbid(unsafe_code)]

//! Owns the shared Unix PTY, child processes, and reader threads for Linux and
//! macOS. Does not touch windows or render.
