//! Watches the settings file on the event-loop thread and debounces its changes.

use std::{
    io,
    os::fd::{AsFd, BorrowedFd},
    time::{Duration, Instant},
};

use sumie_config::{Config, Watch};

use crate::Result;

const DEBOUNCE: Duration = Duration::from_millis(100);

pub(crate) struct Reload {
    watch: Option<Watch>,
    pub(crate) deadline: Option<Instant>,
}

impl Reload {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            watch: sumie_config::user_config_path()
                .map(Watch::new)
                .transpose()?,
            deadline: None,
        })
    }

    /// The settings, or a diagnostic for the banner.
    pub(crate) fn load(&mut self) -> io::Result<Result<Config, String>> {
        let loaded = self
            .watch
            .as_mut()
            .map_or_else(|| Ok(Ok(Config::default())), Watch::load)?;
        Ok(loaded.map_err(|error| super::diagnostic(&error)))
    }

    pub(crate) fn watch_fd(&self) -> Option<BorrowedFd<'_>> {
        self.watch.as_ref().map(AsFd::as_fd)
    }

    /// A debounced result after a change notification; `ready` reports one.
    pub(crate) fn pending(&mut self, ready: bool) -> Result<Option<Result<Config, String>>> {
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
}
