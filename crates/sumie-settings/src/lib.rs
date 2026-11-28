#![forbid(unsafe_code)]
#![cfg(any(target_os = "linux", target_os = "macos"))]

//! Edit sumie settings in the caller's Unix terminal. No GUI or IPC is involved.

mod input;
mod log;
mod model;
mod screen;
mod terminal;
mod value;

use std::io::{self, IsTerminal as _};

use sumie_config::{Language, user_config_path};
use sumie_locale::Locale;

use crate::{log::Log, model::Model};

/// Run one settings session on the calling thread, with exclusive use of stdin and stdout.
/// Blocks on terminal input and on config reads and writes. Call before starting other threads.
///
/// # Errors
/// Returns a localized error when the settings directory or terminal is unavailable.
pub fn run() -> Result<(), &'static str> {
    let auto = Locale::from_language(Language::Auto);
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(auto.text("ui.no-terminal"));
    }
    let main = user_config_path().ok_or_else(|| auto.text("ui.no-config-path"))?;
    let mut model = Model::new(main, Log::from_environment());
    terminal::run(&mut model).map_err(|error| {
        model.log.error(&error);
        model.locale.text("ui.terminal-error")
    })
}

#[cfg(test)]
mod tests;
