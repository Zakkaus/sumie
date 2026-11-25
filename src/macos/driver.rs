//! Drives the terminal session in the `AppKit` window. Callbacks queue native events; winit's
//! `about_to_wait`, which fires before the run loop sleeps in every mode (live resize, menu
//! tracking, modal panels), runs one turn: the batch (ready PTYs, effects, retirement), then
//! a frame at a free slot.

use std::{
    cell::Cell,
    ops::ControlFlow,
    rc::Rc,
    time::{Duration, Instant},
};

use sumie_macos::{
    Event, Platform, WindowContext,
    readiness::{Interest, Key, Readiness},
};

use super::{
    sources::{Timers, Watches},
    surface,
    window::MacWindow,
};
use crate::{
    Result,
    app::{App, Session, Wake},
    capture,
    frame_window::FrameWindow as _,
    reload::Reload,
};

/// The session's own deadlines: reload, retries, closing children, search.
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
    /// The window is gone; children that are still closing keep their exit watches.
    Reaping(Box<(Option<Session>, Watches)>),
}

/// Fields drop in order: the GPU (renderer, then surface) before the window owner, also when
/// Quit ends the loop with the window open.
pub(super) struct Open {
    pub(super) app: App,
    pub(super) window: MacWindow,
    animation: Wake<Timers>,
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

    pub(super) fn open(&mut self, readiness: &Readiness, platform: Platform) -> Result<()> {
        let capture = self.capture.take().ok_or("the window opened twice")?;
        let mut reload = Reload::new()?;
        let config = reload.load()?.unwrap_or_else(|error| {
            reload.load_error(&error);
            sumie_config::Config::default()
        });
        let gpu = surface::create_gpu(
            &platform,
            capture.screenshot.is_some(),
            config.minimum_contrast(),
        )?;
        let window = MacWindow::new(platform, readiness.clone());
        tracing::debug!(refresh = ?window.refresh(), "paced the frame slot to the screen");
        let animation = Wake::new(window.watches.animation_timer());
        self.state = State::Open(Box::new(Open {
            app: App::new(gpu, capture, config, reload),
            window,
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
                if open.turn(readiness)? {
                    self.hide(context)?;
                }
            }
            (State::Open(open) | State::Hiding(open, _), Event::Ready) => {
                open.window.watches.dispatch()?;
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

    /// Retire every pane, then take the window off screen outside the handler.
    fn hide(&mut self, context: &WindowContext) -> Result<()> {
        let State::Open(mut open) = std::mem::replace(&mut self.state, State::Starting) else {
            unreachable!("only an open window hides");
        };
        let close = open.app.close_all(&mut open.window);
        let hidden = Rc::new(Cell::new(false));
        let done = Rc::clone(&hidden);
        open.window
            .platform()
            .order_out(context, move || done.set(true));
        self.state = State::Hiding(open, hidden);
        close
    }

    /// After `orderOut:`: wait for an empty GPU queue, then drop the renderer and the surface,
    /// then the window owner. Closing children stay watched.
    fn drop_window(&mut self, readiness: &Readiness) -> Result<ControlFlow<()>> {
        let State::Hiding(open, _) = std::mem::replace(&mut self.state, State::Starting) else {
            unreachable!("only a hidden window drops");
        };
        let Open { app, window, .. } = *open;
        if !app.gpu().wait_idle(DRAIN) {
            tracing::warn!("GPU work was still pending when the surface dropped");
        }
        let session = app.into_session();
        self.state = State::Reaping(Box::new((session, window.into_watches())));
        self.reap(readiness)
    }

    fn reap(&mut self, readiness: &Readiness) -> Result<ControlFlow<()>> {
        let State::Reaping(reaping) = &mut self.state else {
            unreachable!("only a closed window reaps");
        };
        let (session, watches) = &mut **reaping;
        watches.dispatch()?;
        if let Some(session) = session {
            session.service_closing(Instant::now(), watches)?;
            if session.has_closing() {
                wake_at(readiness, session.closing_deadline())?;
                return Ok(ControlFlow::Continue(()));
            }
        }
        readiness.remove(DEADLINE);
        Ok(ControlFlow::Break(()))
    }
}

impl Open {
    /// One batch, then a frame. Returns whether the window closes.
    pub(super) fn turn(&mut self, readiness: &Readiness) -> Result<bool> {
        let Self {
            app,
            window,
            animation,
        } = self;
        let closed = app.service_batch(window, |app, window| {
            window.deliver_events(|event, window| app.handle_event(event, window))
        })?;
        if closed {
            return Ok(true);
        }
        app.reload_config(false, window)?;
        if app.frame(window, animation)? {
            return Ok(true);
        }
        wake_at(readiness, app.wake_deadline(window.frame_slot_available()))?;
        Ok(false)
    }
}

/// Wake the loop at the session's next deadline, replacing the previous one.
fn wake_at(readiness: &Readiness, at: Option<Instant>) -> Result<()> {
    match at {
        Some(at) => readiness.set_deadline(DEADLINE, at)?,
        None => readiness.remove(DEADLINE),
    }
    Ok(())
}
