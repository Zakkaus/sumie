#![forbid(unsafe_code)]
#![cfg(any(target_os = "linux", target_os = "macos"))]

//! Edit sumie settings in the caller's Unix terminal. No GUI or IPC is involved.

mod input;
mod model;
mod screen;
mod terminal;
mod value;

use std::{error::Error, io::IsTerminal as _};

use sumie_config::{Language, user_config_path};
use sumie_locale::Locale;

use crate::model::Model;

/// Run one settings session on the calling thread, with exclusive use of stdin and stdout.
/// Blocks on terminal input and on config reads and writes. Call before starting other threads.
///
/// # Errors
/// Returns a localized error when the settings directory or terminal is unavailable, or the
/// terminal error that ended the session.
pub fn run() -> Result<(), Box<dyn Error>> {
    let auto = Locale::from_language(Language::Auto);
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(auto.text("ui.no-terminal").into());
    }
    let main = user_config_path().ok_or_else(|| auto.text("ui.no-config-path"))?;
    Ok(terminal::run(&mut Model::new(main))?)
}

#[cfg(test)]
mod tests;
