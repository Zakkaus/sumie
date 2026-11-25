//! Settings results: resolved settings, or a located diagnostic for the banner. Linux watches
//! the file on the event-loop thread (`watch`); macOS watches it in a child process
//! (`macos::reload`) that sends each result as a `frame`.

use std::fmt;

use sumie_config::Error;

#[cfg(target_os = "macos")]
pub(crate) mod frame;
#[cfg(target_os = "linux")]
mod watch;
#[cfg(target_os = "linux")]
pub(crate) use watch::Reload;

/// A load error as the banner shows it: `file:line:column: message`.
pub(crate) fn diagnostic(error: &Error) -> String {
    let (path, line, column, message): (_, _, _, &dyn fmt::Display) = match error {
        Error::Read { path, source } => (path, 1, 1, source),
        Error::Invalid {
            path,
            line,
            column,
            message,
        } => (path, *line, *column, message),
    };
    let file = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    format!("{file}:{line}:{column}: {message}")
}
