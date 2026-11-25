#[cfg(target_os = "linux")]
use std::os::fd::{AsFd, BorrowedFd};
use std::{
    fmt, io,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use sumie_config::Watch;
use sumie_config::{Config, Error};

use crate::Result;

#[cfg(target_os = "macos")]
pub(crate) mod frame;

const DEBOUNCE: Duration = Duration::from_millis(100);

/// macOS reads the settings once at startup; nothing reports a change yet. Errors mirror
/// the Linux watch's errno.
#[cfg(target_os = "macos")]
struct Watch(std::path::PathBuf);

#[cfg(target_os = "macos")]
#[expect(clippy::unnecessary_wraps, reason = "the Linux watch can fail")]
impl Watch {
    fn new(path: std::path::PathBuf) -> Result<Self, nix::errno::Errno> {
        Ok(Self(path))
    }

    fn load(&mut self) -> Result<Result<Config, Error>, nix::errno::Errno> {
        Ok(sumie_config::load(&self.0))
    }

    #[expect(clippy::unused_self, reason = "the Linux watch drains its descriptor")]
    fn changed(&self) -> Result<bool, nix::errno::Errno> {
        Ok(false)
    }
}

pub(crate) struct Reload {
    watch: Option<Watch>,
    pub(crate) deadline: Option<Instant>,
    pub(crate) banner: Option<String>,
}

impl Reload {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            watch: sumie_config::user_config_path()
                .map(Watch::new)
                .transpose()?,
            deadline: None,
            banner: None,
        })
    }

    pub(crate) fn load(&mut self) -> io::Result<Result<Config, Error>> {
        self.watch
            .as_mut()
            .map_or_else(|| Ok(Ok(Config::default())), Watch::load)
            .map_err(io::Error::from)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn watch_fd(&self) -> Option<BorrowedFd<'_>> {
        self.watch.as_ref().map(AsFd::as_fd)
    }

    pub(crate) fn pending(&mut self, ready: bool) -> Result<Option<Result<Config, Error>>> {
        let Some(watch) = &self.watch else {
            return Ok(None);
        };
        if ready && watch.changed()? {
            self.deadline = Some(Instant::now() + DEBOUNCE);
        }
        if self.deadline.is_some_and(|at| at <= Instant::now()) {
            self.deadline = None;
            return Ok(Some(self.load()?));
        }
        Ok(None)
    }

    pub(crate) fn load_error(&mut self, error: &Error) -> bool {
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
        self.show_error(format!("{file}:{line}:{column}: {message}"))
    }

    pub(crate) fn show_error(&mut self, banner: String) -> bool {
        if self.banner.as_ref() == Some(&banner) {
            return false;
        }
        tracing::warn!(error = %banner, "invalid configuration");
        self.banner = Some(banner);
        true
    }
}
