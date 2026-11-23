use std::{
    cell::Cell,
    fmt, io,
    os::fd::AsFd as _,
    rc::Rc,
    time::{Duration, Instant},
};

use calloop::{Interest, Mode, PostAction, generic::Generic};
use sumie_config::{Config, Error, Watch};
use sumie_wayland::SourceHandle;

use crate::Result;

const DEBOUNCE: Duration = Duration::from_millis(100);

pub(crate) struct Reload {
    watch: Option<Watch>,
    ready: Rc<Cell<bool>>,
    pub(crate) deadline: Option<Instant>,
    pub(crate) banner: Option<String>,
}

impl Reload {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            watch: sumie_config::user_config_path()
                .map(Watch::new)
                .transpose()?,
            ready: Rc::default(),
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

    pub(crate) fn register(&self, handle: &SourceHandle) -> Result<()> {
        if let Some(watch) = &self.watch {
            let ready = Rc::clone(&self.ready);
            let source = Generic::new(
                watch.as_fd().try_clone_to_owned()?,
                Interest::READ,
                Mode::Level,
            );
            handle.insert(source, move |_, _| {
                ready.set(true);
                Ok(PostAction::Continue)
            })?;
        }
        Ok(())
    }

    pub(crate) fn pending(&mut self) -> Result<Option<Result<Config, Error>>> {
        let Some(watch) = &self.watch else {
            return Ok(None);
        };
        if self.ready.take() && watch.changed()? {
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
