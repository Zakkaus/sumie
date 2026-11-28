//! Drives the terminal session in the `AppKit` window. Callbacks queue native events; winit's
//! `about_to_wait`, which fires before the run loop sleeps in every mode (live resize, menu
//! tracking, modal panels), runs one turn: the batch (ready PTYs, effects, retirement), then
//! a frame at a free slot.
//!
//! Every pane starts the login shell that `macos::login::prepare` resolved before the window
//! opened.
//!
//! Settings come from the configuration child (`macos::reload`). The session starts from its
//! first result, under the banner if that is a diagnostic, or from the defaults under a
//! loading banner if none arrived by [`FIRST_RESULT`]; later results apply through
//! `App::apply_config`. Closing stops the child, and the driver exits once it and every
//! closing pane child are reaped.
//!
//! Close, Quit and a fatal error all drain the same way: retire every pane (SIGHUP), stop the
//! configuration child, take the window off screen, release the GPU, then reap; the stages go
//! to `sumie_macos::termination`, whose clock starts at the first of them. A system quit ends
//! the reaping at its reply deadline, and a fatal error ends the loop with that error once
//! drained.

use std::{
    cell::Cell,
    ops::ControlFlow,
    rc::Rc,
    time::{Duration, Instant},
};

use sumie_config::{Config, user_config_path};
use sumie_macos::{
    Event, Platform, WindowContext,
    readiness::{Interest, Key, Readiness},
    termination::{Quit, Stage, Termination},
};

use super::{
    login,
    pasteboard::Pasteboard,
    reload::{self, Reload, Update},
    sources::{Timers, Watches},
    surface,
    window::MacWindow,
};
use crate::{
    Error, Result,
    app::{App, Session, Wake},
    capture,
    frame_window::FrameWindow as _,
    login::Environment,
};

/// The session's own deadlines: retries, closing children, search.
const DEADLINE: Key = Key::new(u64::MAX - 1, Interest::Timer);
/// How long the window stays blank for the configuration child's first result. A normal start
/// presents its settings 80-100 ms after the window opens (debug build), so it never flashes
/// the defaults; a FIFO or stalled include never answers, and half a second of blank window is
/// still short of looking hung.
const FIRST_RESULT: Duration = Duration::from_millis(500);
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
    /// Built from the configuration child's first result or at `first_by`; nothing presents
    /// before it.
    pub(super) app: Option<App>,
    first_by: Instant,
    pub(super) window: MacWindow,
    reload: Reload,
    animation: Wake<Timers>,
}

pub(super) struct Reaping {
    session: Option<Session>,
    reload: Reload,
    pasteboard: Pasteboard,
    watches: Watches,
}

pub(super) struct Driver {
    /// Taken when the session starts.
    start: Option<Start>,
    pub(super) state: State,
    /// Entered at the first Close, Quit or fatal error.
    pub(super) termination: Termination,
    /// The fatal error the loop ends with once draining is done.
    failure: Option<Error>,
}

/// What the session starts with besides its first settings.
pub(super) struct Start {
    capture: capture::Options,
    environment: Environment,
}

/// Run the terminal driver until its window closes and its children are reaped.
pub(super) fn run(capture: capture::Options) -> Result<()> {
    let mut driver = Driver::new(capture)?;
    sumie_macos::run(|context, readiness, event| {
        if let (State::Starting, Event::Resumed) = (&driver.state, &event) {
            driver.open(readiness, Platform::create(context, "sumie", SIZE)?)?;
            if let State::Open(open) = &mut driver.state {
                open.window.set_ime(true);
            }
            return Ok(ControlFlow::Continue(()));
        }
        driver.handle(context, readiness, event)
    })
}

impl Driver {
    /// Resolve the login shell; call before the window opens.
    pub(super) fn new(capture: capture::Options) -> Result<Self> {
        let environment = login::prepare()?;
        Ok(Self {
            start: Some(Start {
                capture,
                environment,
            }),
            state: State::Starting,
            termination: Termination::default(),
            failure: None,
        })
    }

    /// Show the window and start the configuration child; the session waits for its result
    /// until [`FIRST_RESULT`].
    pub(super) fn open(&mut self, readiness: &Readiness, platform: Platform) -> Result<()> {
        let mut window = MacWindow::new(platform, readiness.clone())?;
        tracing::debug!(refresh = ?window.refresh(), "paced the frame slot to the screen");
        let reload = Reload::spawn(reload::command()?, &mut window.watches)?;
        let animation = Wake::new(window.watches.animation_timer());
        let first_by = Instant::now() + FIRST_RESULT;
        wake_at(readiness, Some(first_by))?;
        self.state = State::Open(Box::new(Open {
            app: None,
            first_by,
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
        let result = self.step(context, readiness, event);
        self.settle(context, readiness, result)
    }

    /// A fatal error drains like Close; the loop then ends with that error. An error while
    /// draining after one ends the loop at once, with the first.
    pub(super) fn settle(
        &mut self,
        context: &WindowContext,
        readiness: &Readiness,
        result: Result<ControlFlow<()>>,
    ) -> Result<ControlFlow<()>> {
        let flow = match result {
            Ok(flow) => flow,
            Err(error) => self.fail(context, readiness, error)?,
        };
        match (flow, self.failure.take()) {
            (ControlFlow::Break(()), Some(error)) => Err(error),
            (flow, failure) => {
                self.failure = failure;
                Ok(flow)
            }
        }
    }

    fn fail(
        &mut self,
        context: &WindowContext,
        readiness: &Readiness,
        error: Error,
    ) -> Result<ControlFlow<()>> {
        if let Some(first) = self.failure.take() {
            tracing::error!(%error, "a second error ended draining");
            return Err(first);
        }
        tracing::error!(%error, "draining after a fatal error");
        self.failure = Some(error);
        self.drain(context, readiness, Quit::Ordinary)
            .or_else(|error| self.fail(context, readiness, error))
    }

    /// Enter draining, or upgrade it to a system quit, from whatever stage the window is in.
    fn drain(
        &mut self,
        context: &WindowContext,
        readiness: &Readiness,
        quit: Quit,
    ) -> Result<ControlFlow<()>> {
        self.termination.begin(quit)?;
        match &self.state {
            State::Starting => return Ok(ControlFlow::Break(())),
            State::Open(_) => self.hide(context)?,
            State::Hiding(..) => (),
            State::Reaping(_) => return self.reap(readiness),
        }
        Ok(ControlFlow::Continue(()))
    }

    fn step(
        &mut self,
        context: &WindowContext,
        readiness: &Readiness,
        event: Event,
    ) -> Result<ControlFlow<()>> {
        match (&mut self.state, event) {
            (State::Open(open), Event::Quit(Quit::Ordinary)) if open.app.is_some() => {
                open.window.queue(Event::CloseRequested);
            }
            (State::Hiding(..) | State::Reaping(_), Event::Quit(Quit::Ordinary)) => (),
            (_, Event::Quit(quit)) => return self.drain(context, readiness, quit),
            (State::Open(open), Event::AboutToWait) => {
                if open.turn(readiness, &mut self.start)? {
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
        self.termination.begin(Quit::Ordinary)?;
        let State::Open(mut open) = std::mem::replace(&mut self.state, State::Starting) else {
            unreachable!("only an open window hides");
        };
        let close = match &mut open.app {
            Some(app) => app.close_all(&mut open.window),
            None => Ok(()),
        };
        let stop = open
            .reload
            .close(&mut open.window.watches)
            .and(open.window.close_pasteboard());
        let hidden = Rc::new(Cell::new(false));
        let done = Rc::clone(&hidden);
        open.window
            .platform()
            .order_out(context, move || done.set(true));
        self.state = State::Hiding(open, hidden);
        self.termination.stage(Stage::Hiding, 1);
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
        self.termination.stage(Stage::Releasing, 1);
        if let Some(app) = &app
            && !app.gpu().wait_idle(DRAIN)
        {
            tracing::warn!("GPU work was still pending when the surface dropped");
        }
        let (watches, pasteboard) = window.into_parts();
        self.state = State::Reaping(Box::new(Reaping {
            session: app.and_then(App::into_session),
            reload,
            pasteboard,
            watches,
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
            pasteboard,
            watches,
        } = &mut **reaping;
        watches.dispatch()?;
        // Results that arrive after the close no longer apply.
        reload.service(watches)?;
        pasteboard.service(watches)?;
        let now = Instant::now();
        let reply_by = self.termination.reply_by();
        // The configuration child's exit watch and kill timer report through `Event::Ready`.
        let mut open = usize::from(!reload.reaped()) + pasteboard.children();
        let mut deadline = reply_by;
        if let Some(session) = session {
            session.service_closing(now, watches)?;
            open += session.closing_count();
            deadline = deadline.into_iter().chain(session.closing_deadline()).min();
        }
        if open == 0 || reply_by.is_some_and(|at| at <= now) {
            if open > 0 {
                tracing::warn!(
                    open,
                    "replying to the system quit; launchd inherits the rest"
                );
            }
            self.termination.stage(Stage::Exiting, open);
            readiness.remove(DEADLINE);
            return Ok(ControlFlow::Break(()));
        }
        self.termination.stage(Stage::Reaping, open);
        wake_at(readiness, deadline)?;
        Ok(ControlFlow::Continue(()))
    }
}

impl Open {
    /// Take the configuration child's results, then one batch, then a frame. Returns whether
    /// the window closes.
    pub(super) fn turn(
        &mut self,
        readiness: &Readiness,
        pending: &mut Option<Start>,
    ) -> Result<bool> {
        let Self {
            app,
            first_by,
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
            let first = match results.next() {
                Some(first) => first,
                None if Instant::now() >= *first_by => Err(still_loading()),
                None => return Ok(false),
            };
            let pending = pending.take().ok_or("the session started twice")?;
            app.insert(start(window, pending, first)?)
        };
        window.service_pasteboard()?;
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
    Start {
        capture,
        environment,
    }: Start,
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
    Ok(App::new(gpu, capture, config, banner, environment))
}

/// The banner while the first result is late; the result replaces it when it arrives.
fn still_loading() -> String {
    user_config_path().map_or_else(
        || "settings still loading".to_owned(),
        |path| format!("settings still loading: {}", path.display()),
    )
}

/// Wake the loop at the session's next deadline, replacing the previous one.
fn wake_at(readiness: &Readiness, at: Option<Instant>) -> Result<()> {
    match at {
        Some(at) => readiness.set_deadline(DEADLINE, at)?,
        None => readiness.remove(DEADLINE),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sumie_macos::termination::SYSTEM_KILL;
    use sumie_pty::{Command, Size, SpawnOptions};

    /// A system quit kills closing children by `SYSTEM_KILL`; panes rely on the PTY's grace
    /// period never being longer, since nothing shortens it.
    #[test]
    fn the_pane_grace_period_ends_by_the_system_kill() {
        let env = BTreeMap::new();
        let size = Size {
            cols: 80,
            rows: 24,
            width_px: 0,
            height_px: 0,
        };
        let options = SpawnOptions::new(Command::LoginShell, &env, size);
        assert!(options.close_grace <= SYSTEM_KILL);
    }
}
