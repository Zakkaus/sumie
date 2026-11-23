//! Bridges kqueue readiness to winit redraws without polling the main thread.

use std::{
    fmt, io,
    io::{Read as _, Write as _},
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use sumie_config::{Config, Error, Watch, WatchEvent, user_config_path};
use sumie_macos::Platform;

const DEBOUNCE: Duration = Duration::from_millis(100);
const BLINK: Duration = Duration::from_millis(600);

pub(super) enum Update {
    Settings(Result<Config, Error>),
    Blink,
    Failed(io::Error),
}

pub(super) struct Reload {
    control: Option<UnixStream>,
    worker: Option<JoinHandle<()>>,
    updates: mpsc::Receiver<Update>,
    pub(super) banner: Option<String>,
}

impl Reload {
    pub(super) fn new(window: &Platform) -> io::Result<(Self, Config)> {
        let (control, remote) = UnixStream::pair()?;
        let mut watch = Watch::new(user_config_path(), &remote)?;
        let (updates, receiver) = mpsc::channel();
        let notify = {
            let sink = Arc::new((
                Arc::downgrade(&window.window_owner()),
                updates,
                AtomicBool::new(false),
            ));
            move |update| {
                let sink = Arc::clone(&sink);
                // Winit's macOS request_redraw waits for the main thread when called off-thread.
                Watch::notify_main(move || {
                    let (window, updates, was_focused) = &*sink;
                    let Some(window) = window.upgrade() else {
                        return;
                    };
                    if matches!(update, Update::Blink) {
                        let focused = window.has_focus();
                        if !was_focused.swap(focused, Ordering::Relaxed) && !focused {
                            return;
                        }
                    }
                    if updates.send(update).is_ok() {
                        window.request_redraw();
                    }
                });
            }
        };
        let mut reload = Self {
            control: Some(control),
            worker: None,
            updates: receiver,
            banner: None,
        };
        let config = watch.load()?.unwrap_or_else(|error| {
            reload.load_error(&error);
            Config::default()
        });
        let blink = config.cursor.blink();
        reload.worker = Some(thread::Builder::new().name("sumie-config".into()).spawn(
            move || {
                if let Err(error) = drive(watch, remote, blink, &notify) {
                    notify(Update::Failed(error));
                }
            },
        )?);
        Ok((reload, config))
    }

    pub(super) fn set_blink(&mut self, blink: bool) -> io::Result<()> {
        self.control.as_mut().unwrap().write_all(&[u8::from(blink)])
    }

    pub(super) fn next(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    pub(super) fn load_error(&mut self, error: &Error) -> bool {
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

    pub(super) fn show_error(&mut self, message: String) -> bool {
        if self.banner.as_ref() == Some(&message) {
            return false;
        }
        eprintln!("sumie: invalid configuration: {message}");
        self.banner = Some(message);
        true
    }
}

impl Drop for Reload {
    fn drop(&mut self) {
        drop(self.control.take());
        if let Some(worker) = self.worker.take() {
            worker.join().expect("config watcher panicked");
        }
    }
}

fn drive(
    mut watch: Watch,
    mut control: UnixStream,
    blink: bool,
    notify: impl Fn(Update),
) -> io::Result<()> {
    let mut reload_at = None;
    let mut blink_at = blink.then(|| Instant::now() + BLINK);
    loop {
        let deadline = reload_at.into_iter().chain(blink_at).min();
        match watch.wait(deadline)? {
            WatchEvent::Changed => reload_at = Some(Instant::now() + DEBOUNCE),
            WatchEvent::Control => {
                let mut blink = [0];
                if control.read(&mut blink)? == 0 {
                    return Ok(());
                }
                blink_at = (blink[0] != 0).then(|| Instant::now() + BLINK);
            }
            WatchEvent::Deadline => {
                if reload_at.is_some_and(|at| at <= Instant::now()) {
                    reload_at = None;
                    notify(Update::Settings(watch.load()?));
                }
            }
        }
        let now = Instant::now();
        if blink_at.is_some_and(|at| at <= now) {
            blink_at = Some(now + BLINK);
            notify(Update::Blink);
        }
    }
}
