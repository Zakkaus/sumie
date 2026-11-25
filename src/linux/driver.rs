//! Drive the Wayland window and the shared session on one calloop thread.

use std::time::Instant;

use super::{LinuxWindow, sources::Reaper};
use crate::{
    Result,
    app::{App, Wake},
    capture,
    frame_window::FrameWindow as _,
    reload::Reload,
};

pub(crate) fn run(capture: capture::Options) -> Result<()> {
    let mut reload = Reload::new()?;
    let config = reload.load()?.unwrap_or_else(|error| {
        reload.load_error(&error);
        sumie_config::Config::default()
    });
    let mut window = LinuxWindow::open("sumie")?;
    let gpu = window.create_gpu(capture.screenshot.is_some(), config.minimum_contrast())?;
    window.watches.register_config(reload.watch_fd())?;
    let mut app = App::new(gpu, capture, config, reload);
    let result = drive(&mut app, &mut window);
    let close = app.close_all(&mut window);
    let reaper = Reaper::new(app.closing_fds());
    // The GPU drops before the window; the session stays to reap closing children.
    let session = app.into_session();
    drop(window);
    let reap = reaper.and_then(|reaper| {
        if let Some((mut session, mut reaper)) = session.zip(reaper) {
            while session.has_closing() {
                reaper.dispatch(session.closing_deadline())?;
                session.service_closing(Instant::now(), &mut reaper)?;
            }
        }
        Ok(())
    });
    result.and(close).and(reap)
}

fn drive(app: &mut App, window: &mut LinuxWindow) -> Result<()> {
    let mut animation = Wake::new(window.watches.animation_timer());
    loop {
        let timeout = app
            .wake_deadline(window.frame_slot_available())
            .map(|at| at.saturating_duration_since(Instant::now()));
        window.dispatch(timeout)?;
        let closed = app.service_batch(window, |app, window| {
            window.deliver_events(|event, window| app.handle_event(event, window))
        })?;
        if closed {
            return Ok(());
        }
        app.reload_config(window.watches.take_config_ready(), window)?;
        if app.frame(window, &mut animation)? {
            return Ok(());
        }
    }
}
