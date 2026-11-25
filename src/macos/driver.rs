//! Drives the terminal session in the `AppKit` window. Callbacks queue native events; winit's
//! `about_to_wait`, which fires before the run loop sleeps in every mode (live resize, menu
//! tracking, modal panels), runs one turn: the batch (ready PTYs, effects, retirement), then
//! a frame at a free slot.
//!
//! Settings come from the configuration child (`macos::reload`). The session starts from its
//! first result, under the banner if that is a diagnostic; later results apply through
//! `App::apply_config`. Closing stops the child, and the driver exits once it and every
//! closing pane child are reaped.

use std::{
    cell::Cell,
    ops::ControlFlow,
    rc::Rc,
    time::{Duration, Instant},
};

use sumie_config::Config;
use sumie_macos::{
    Event, Platform, WindowContext,
    readiness::{Interest, Key, Readiness},
};

use super::{
    reload::{self, Reload, Update},
    sources::{Timers, Watches},
    surface,
    window::MacWindow,
};
use crate::{
    Result,
    app::{App, Session, Wake},
    capture,
    frame_window::FrameWindow as _,
};

/// The session's own deadlines: retries, closing children, search.
const DEADLINE: Key = Key::new(u64::MAX - 1, Interest::Timer);
/// The window's first size in points.
const SIZE: [u32; 2] = [800, 500];
/// How long teardown waits for submitted GPU work before dropping the surface anyway. The
/// window is already off screen; the wait blocks the main thread.
const DRAIN: Duration = Duration::from_millis(100);

pub(super) enum State {
    Starting,
    Open(Box<Open>),
    /// `orderOut:` is queued; the GPU and the window drop once it ran.
    Hiding(Box<Open>, Rc<Cell<bool>>),
    /// The window is gone; the configuration child and closing pane children keep their exit
    /// watches until they are reaped.
    Reaping(Box<Reaping>),
}

/// Fields drop in order: the GPU (renderer, then surface) before the window owner, also when
/// Quit ends the loop with the window open.
pub(super) struct Open {
    /// Built from the configuration child's first result; nothing presents before it.
    pub(super) app: Option<App>,
    pub(super) window: MacWindow,
    reload: Reload,
    animation: Wake<Timers>,
}

pub(super) struct Reaping {
    session: Option<Session>,
    reload: Reload,
    watches: Watches,
}

pub(super) struct Driver {
    capture: Option<capture::Options>,
    pub(super) state: State,
}

/// Run the terminal driver until its window closes and its children are reaped.
pub(super) fn run(capture: capture::Options) -> Result<()> {
    let mut driver = Driver::new(capture);
    sumie_macos::run(|context, readiness, event| {
        if let (State::Starting, Event::Resumed) = (&driver.state, &event) {
            driver.open(readiness, Platform::create(context, "sumie", SIZE)?)?;
            return Ok(ControlFlow::Continue(()));
        }
        driver.handle(context, readiness, event)
    })
}

impl Driver {
    pub(super) const fn new(capture: capture::Options) -> Self {
        Self {
            capture: Some(capture),
            state: State::Starting,
        }
    }

    /// Show the window and start the configuration child; the session waits for its result.
    pub(super) fn open(&mut self, readiness: &Readiness, platform: Platform) -> Result<()> {
        let mut window = MacWindow::new(platform, readiness.clone());
        tracing::debug!(refresh = ?window.refresh(), "paced the frame slot to the screen");
        let reload = Reload::spawn(reload::command()?, &mut window.watches)?;
        let animation = Wake::new(window.watches.animation_timer());
        self.state = State::Open(Box::new(Open {
            app: None,
            window,
            reload,
            animation,
        }));
        Ok(())
    }

    /// Handle every event after the window opened.
    pub(super) fn handle(
        &mut self,
        context: &WindowContext,
        readiness: &Readiness,
        event: Event,
    ) -> Result<ControlFlow<()>> {
        match (&mut self.state, event) {
            (State::Open(open), Event::AboutToWait) => {
                if open.turn(readiness, &mut self.capture)? {
                    self.hide(context)?;
                }
            }
            (State::Open(open) | State::Hiding(open, _), Event::Ready) => {
                open.window.watches.dispatch()?;
            }
            // Without a session the queued close would wait for the first settings.
            (State::Open(open), Event::CloseRequested) if open.app.is_none() => {
                self.hide(context)?;
            }
            (State::Open(open), event) => open.window.queue(event),
            (State::Hiding(_, hidden), Event::AboutToWait) if hidden.get() => {
                return self.drop_window(readiness);
            }
            (State::Reaping(..), Event::Ready | Event::AboutToWait) => return self.reap(readiness),
            _ => (),
        }
        Ok(ControlFlow::Continue(()))
    }

    /// Retire every pane and stop the configuration child, then take the window off screen
    /// outside the handler.
    fn hide(&mut self, context: &WindowContext) -> Result<()> {
        let State::Open(mut open) = std::mem::replace(&mut self.state, State::Starting) else {
            unreachable!("only an open window hides");
        };
        let close = match &mut open.app {
            Some(app) => app.close_all(&mut open.window),
            None => Ok(()),
        };
        let stop = open.reload.close(&mut open.window.watches);
        let hidden = Rc::new(Cell::new(false));
        let done = Rc::clone(&hidden);
        open.window
            .platform()
            .order_out(context, move || done.set(true));
        self.state = State::Hiding(open, hidden);
        close.and(stop)
    }

    /// After `orderOut:`: wait for an empty GPU queue, then drop the renderer and the surface,
    /// then the window owner. Closing children stay watched.
    fn drop_window(&mut self, readiness: &Readiness) -> Result<ControlFlow<()>> {
        let State::Hiding(open, _) = std::mem::replace(&mut self.state, State::Starting) else {
            unreachable!("only a hidden window drops");
        };
        let Open {
            app,
            window,
            reload,
            ..
        } = *open;
        if let Some(app) = &app
            && !app.gpu().wait_idle(DRAIN)
        {
            tracing::warn!("GPU work was still pending when the surface dropped");
        }
        self.state = State::Reaping(Box::new(Reaping {
            session: app.and_then(App::into_session),
            reload,
            watches: window.into_watches(),
        }));
        self.reap(readiness)
    }

    fn reap(&mut self, readiness: &Readiness) -> Result<ControlFlow<()>> {
        let State::Reaping(reaping) = &mut self.state else {
            unreachable!("only a closed window reaps");
        };
        let Reaping {
            session,
            reload,
            watches,
        } = &mut **reaping;
        watches.dispatch()?;
        // Results that arrive after the close no longer apply.
        reload.service(watches)?;
        if let Some(session) = session {
            session.service_closing(Instant::now(), watches)?;
            if session.has_closing() {
                wake_at(readiness, session.closing_deadline())?;
                return Ok(ControlFlow::Continue(()));
            }
        }
        readiness.remove(DEADLINE);
        // The child's exit watch and its kill timer report through `Event::Ready`.
        Ok(if reload.reaped() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        })
    }
}

impl Open {
    /// Take the configuration child's results, then one batch, then a frame. Returns whether
    /// the window closes.
    pub(super) fn turn(
        &mut self,
        readiness: &Readiness,
        capture: &mut Option<capture::Options>,
    ) -> Result<bool> {
        let Self {
            app,
            window,
            reload,
            animation,
        } = self;
        let mut results =
            reload
                .service(&mut window.watches)?
                .into_iter()
                .map(|update| match update {
                    Update::Settings(config) => Ok(config),
                    Update::Diagnostic(message) => Err(message),
                });
        let app = if let Some(app) = app {
            app
        } else {
            let Some(first) = results.next() else {
                return Ok(false);
            };
            let capture = capture.take().ok_or("the session started twice")?;
            app.insert(start(window, capture, first)?)
        };
        let closed = app.service_batch(window, |app, window| {
            window.deliver_events(|event, window| app.handle_event(event, window))
        })?;
        if closed {
            return Ok(true);
        }
        for next in results {
            app.apply_config(next, window)?;
        }
        if app.frame(window, animation)? {
            return Ok(true);
        }
        wake_at(readiness, app.wake_deadline(window.frame_slot_available()))?;
        Ok(false)
    }
}

/// Build the GPU and the app from the first settings result: a diagnostic starts from the
/// defaults under the banner.
fn start(
    window: &MacWindow,
    capture: capture::Options,
    first: Result<Config, String>,
) -> Result<App> {
    let (config, banner) = match first {
        Ok(config) => (config, None),
        Err(message) => (Config::default(), Some(message)),
    };
    let gpu = surface::create_gpu(
        window.platform(),
        capture.screenshot.is_some(),
        config.minimum_contrast(),
    )?;
    Ok(App::new(gpu, capture, config, banner))
}

/// Wake the loop at the session's next deadline, replacing the previous one.
fn wake_at(readiness: &Readiness, at: Option<Instant>) -> Result<()> {
    match at {
        Some(at) => readiness.set_deadline(DEADLINE, at)?,
        None => readiness.remove(DEADLINE),
    }
    Ok(())
}
