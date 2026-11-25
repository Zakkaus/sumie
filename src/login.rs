//! Builds the login shell a new pane starts: its environment and its command line.
//!
//! The environment is the requester's plus the terminal identity. On macOS it also names the
//! resolved login shell and, when the requester's is not UTF-8, a character locale. Shell
//! integration then adjusts it for supported shells; nothing here edits a profile.

#[path = "shell_integration.rs"]
mod shell_integration;
#[path = "terminfo.rs"]
mod terminfo;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

use sumie_pty::Command;

use crate::Result;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A login shell's complete environment and arguments.
pub(crate) struct Login {
    pub(crate) env: BTreeMap<OsString, OsString>,
    args: Option<[OsString; 3]>,
}

impl Login {
    /// Resolve the login shell for `requester`'s environment. The first call can block on
    /// the bundled terminfo and shell integration installs (file locks, filesystem, `tic`).
    ///
    /// # Errors
    /// On macOS, returns an error when neither `SHELL` nor the account names a usable shell.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::unnecessary_wraps,
            reason = "only macOS shell resolution can fail"
        )
    )]
    pub(crate) fn new(
        requester: impl IntoIterator<Item = (OsString, OsString)>,
        integration: bool,
    ) -> Result<Self> {
        let env = terminal_environment(requester.into_iter().collect());
        #[cfg(target_os = "macos")]
        let env = crate::macos::login::apply(env)?;
        let mut login = Self { env, args: None };
        if integration {
            login.args = shell_integration::inject(&mut login.env);
        }
        Ok(login)
    }

    #[cfg_attr(
        all(target_os = "macos", not(test)),
        expect(dead_code, reason = "the terminal driver spawns macOS panes")
    )]
    pub(crate) fn command(&self) -> Command<'_> {
        self.args
            .as_ref()
            .map_or(Command::LoginShell, |args| Command::Argv {
                program: &self.env[OsStr::new("SHELL")],
                args,
            })
    }
}

/// This process's environment plus the terminal identity, for commands other than the shell.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn request_environment() -> BTreeMap<OsString, OsString> {
    terminal_environment(std::env::vars_os().collect())
}

/// Install the bundled terminfo entry and the shell integration scripts.
#[cfg(target_os = "macos")]
pub(crate) fn install() {
    terminfo::directory();
    shell_integration::directory();
}

fn terminal_environment(mut env: BTreeMap<OsString, OsString>) -> BTreeMap<OsString, OsString> {
    let term = if let Some(dir) = terminfo::directory() {
        env.insert("TERMINFO".into(), dir.as_os_str().into());
        terminfo::NAME
    } else {
        env.remove(OsStr::new("TERMINFO"));
        "xterm-256color"
    };
    env.insert("TERM".into(), term.into());
    env.insert("COLORTERM".into(), "truecolor".into());
    env.insert("TERM_PROGRAM".into(), "sumie".into());
    env.insert("TERM_PROGRAM_VERSION".into(), VERSION.into());
    // Node's supports-hyperlinks (Claude Code, npm) only trusts terminals it knows by name.
    // Respect a value the user already set, including 0.
    env.entry("FORCE_HYPERLINK".into())
        .or_insert_with(|| "1".into());
    env
}
