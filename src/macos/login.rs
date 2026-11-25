//! Resolves the macOS login shell and character locale for a pane's environment.
//!
//! The shell is the requester's `SHELL` when that is an absolute path to an executable file,
//! else the account's shell from the user database; with neither, no pane can start. The
//! requester's locale stays unless it names no valid character locale: then the child gets a
//! UTF-8 `LC_CTYPE` that exists on this system. Everything else passes through unchanged,
//! including agent sockets and a `ZDOTDIR`.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use nix::unistd::{AccessFlags, User, access, getuid};

use crate::{Result, login::Login};

/// Darwin's locale database; `setlocale` reads `<name>/LC_CTYPE` below it.
const LOCALES: &str = "/usr/share/locale";
/// `en_US.UTF-8` shares the `UTF-8` character table and is also known to Linux hosts over SSH.
const UTF8_LOCALES: [&str; 2] = ["en_US.UTF-8", "UTF-8"];

/// Resolve the login shell and install terminfo and shell integration, before the window
/// opens: these can wait on the user database, file locks and `tic`.
///
/// # Errors
/// Returns an error when no usable login shell exists.
pub(crate) fn prepare() -> Result<Login> {
    crate::login::install();
    Login::new(std::env::vars_os(), true)
}

/// Name the login shell and, if needed, a character locale in a child environment.
///
/// # Errors
/// Returns an error when neither `SHELL` nor the account names an executable shell.
pub(crate) fn apply(mut env: BTreeMap<OsString, OsString>) -> Result<BTreeMap<OsString, OsString>> {
    let shell = shell(env.get(OsStr::new("SHELL")), account_shell)?;
    env.insert("SHELL".into(), shell.into());
    if let Some(locale) = missing_locale(&env, locale_exists) {
        env.insert("LC_CTYPE".into(), locale.into());
    }
    Ok(env)
}

fn shell(
    requested: Option<&OsString>,
    account: impl FnOnce() -> Result<Option<PathBuf>>,
) -> Result<PathBuf> {
    if let Some(shell) = requested.map(PathBuf::from).filter(|path| executable(path)) {
        return Ok(shell);
    }
    let account = account()?;
    if let Some(shell) = account.as_deref().filter(|path| executable(path)) {
        if requested.is_some() {
            tracing::warn!("SHELL is not an absolute executable path; using the account's shell");
        }
        return Ok(shell.to_owned());
    }
    Err("no login shell: SHELL is not an absolute executable path and the account has none".into())
}

fn executable(path: &Path) -> bool {
    path.is_absolute() && path.is_file() && access(path, AccessFlags::X_OK).is_ok()
}

fn account_shell() -> Result<Option<PathBuf>> {
    Ok(User::from_uid(getuid())?.map(|user| user.shell))
}

/// The locale to set as `LC_CTYPE`, or `None` if the requester named a valid one.
/// `LC_ALL` overrides every category, so a requester who set it keeps it.
fn missing_locale(
    env: &BTreeMap<OsString, OsString>,
    exists: impl Fn(&str) -> bool,
) -> Option<&'static str> {
    let named = |key: &str| env.get(OsStr::new(key)).filter(|value| !value.is_empty());
    if named("LC_ALL").is_some() {
        return None;
    }
    let requested = named("LC_CTYPE").or_else(|| named("LANG"));
    if requested.is_some_and(|name| name.to_str().is_some_and(|name| valid(name, &exists))) {
        return None;
    }
    UTF8_LOCALES.into_iter().find(|name| exists(name))
}

fn valid(name: &str, exists: impl Fn(&str) -> bool) -> bool {
    matches!(name, "C" | "POSIX") || (!name.contains('/') && !name.starts_with('.') && exists(name))
}

fn locale_exists(name: &str) -> bool {
    Path::new(LOCALES).join(name).join("LC_CTYPE").is_file()
}

#[cfg(test)]
mod tests;
