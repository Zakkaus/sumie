use std::time::Instant;

use super::{Session, tabs};
use crate::{
    Result, capture,
    frame_window::FrameWindow as _,
    gpu::{self, Gpu},
    linux::LinuxWindow,
    pane::{set_colors, set_cursor_defaults},
    platform::{Event, WindowOps},
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
    let mut app = App {
        gpu,
        capture,
        config,
        reload,
        session: None,
        focused: false,
        suspended: false,
    };
    let result = app.drive(&mut window);
    let close = app
        .session
        .as_mut()
        .map_or(Ok(()), |session| session.close_all(&mut window));
    let reaper =
        crate::linux::sources::Reaper::new(app.session.iter().flat_map(Session::closing_fds));
    drop(app.gpu);
    drop(window);
    let reap = reaper.and_then(|reaper| {
        if let Some((session, mut reaper)) = app.session.as_mut().zip(reaper) {
            while !session.closing.is_empty() {
                reaper.dispatch(session.closing_deadline())?;
                session.service_closing(Instant::now(), &mut reaper)?;
            }
        }
        Ok(())
    });
    result.and(close).and(reap)
}

struct App {
    gpu: Gpu,
    capture: capture::Options,
    config: sumie_config::Config,
    reload: Reload,
    session: Option<Session>,
    focused: bool,
    suspended: bool,
}

impl App {
    fn drive(&mut self, window: &mut LinuxWindow) -> Result<()> {
        let mut animation = super::animation::Wake::new(window.watches.animation_timer());
        loop {
            let retry = self
                .session
                .as_ref()
                .and_then(|session| session.frame_retry);
            let timeout = retry
                .into_iter()
                .chain(self.reload.deadline)
                .chain(self.session.as_ref().and_then(Session::closing_deadline))
                .chain(
                    self.session
                        .as_ref()
                        .filter(|s| s.active().search_pending())
                        .map(|_| Instant::now()),
                )
                .min()
                .map(|at| at.saturating_duration_since(Instant::now()));
            window.dispatch(timeout)?;
            let mut exited = false;
            if let Some(session) = &mut self.session {
                let now = Instant::now();
                session.service_closing(now, window)?;
                exited = session.service_pty(now, window)?;
            }
            // Each event takes effect before the next; child exits cannot retarget input.
            if window.deliver_events(|event, window| self.handle_event(event, window))? {
                return Ok(());
            }
            if exited
                && let Some(session) = &mut self.session
                && session.close_exited(window)?
            {
                return Ok(());
            }
            self.reload_config(window)?;
            if let Some(session) = &mut self.session {
                session.active_mut().advance_search()?;
                session.sync_input(window)?;
                let gpu = &mut self.gpu;
                if !self.suspended && session.pane_pointer.take_frame_recovery() {
                    window.recover_frame_slot();
                }
                let ready = !self.suspended
                    && window.frame_slot_available()
                    && session.frame_retry.is_none_or(|at| Instant::now() >= at);
                let next = if ready {
                    if session.view.apply_pending_resize()? {
                        session.resize_grid()?;
                        gpu.resize(session.view.device);
                    }
                    if session.frame_retry.is_none() {
                        session.step_scroll(Instant::now())?;
                    }
                    let now = session.frame_retry.is_none().then(|| animation.now_ms());
                    session.refresh_visible(now, window)?
                } else {
                    None
                };
                let banner = self.reload.banner.as_deref();
                session.update_link_hover(window, banner.is_some(), self.focused)?;
                session.watch_blink(window, self.focused && !self.suspended, banner)?;
                if ready && session.draw(gpu, window, &self.capture, banner, self.focused)? {
                    return Ok(());
                }
                animation.arm(
                    next.filter(|_| session.frame_retry.is_none() && window.frame_slot_available()),
                )?;
            }
        }
    }

    fn reload_config(&mut self, window: &mut LinuxWindow) -> Result<()> {
        let Some(next) = self.reload.pending(window.watches.take_config_ready())? else {
            return Ok(());
        };
        let next = match next {
            Ok(next) => next,
            Err(error) => {
                if self.reload.load_error(&error)
                    && let Some(session) = &mut self.session
                {
                    session.active_mut().frame_dirty = true;
                }
                return Ok(());
            }
        };
        let changes = self.config.changes(&next);
        if let Some(session) = &mut self.session {
            let mut resize_grid = changes.padding;
            if changes.font {
                match session.view.set_font(next.font.clone()) {
                    Ok(resized) => resize_grid |= resized,
                    Err(error) => {
                        session.active_mut().frame_dirty |=
                            self.reload.show_error(format!("sumie.toml:1:1: {error}"));
                        return Ok(());
                    }
                }
            }
            if changes.padding {
                session.view.set_padding(next.window.padding());
            }
            if resize_grid {
                session.resize_grid()?;
            }
            for tab in session.all_panes_mut() {
                if changes.scrollback_limit {
                    tab.pane
                        .terminal
                        .set_scrollback_bytes(next.scrollback_limit())?;
                    tab.snapshot_dirty = true;
                    if let Some(bar) = &mut tab.search {
                        bar.output(&tab.pane.terminal)?;
                        tab.frame_dirty = true;
                    }
                }
                if changes.thumbnail_rows {
                    tab.pane
                        .terminal
                        .set_thumbnail_rows(next.images.thumbnail_rows());
                }
                if changes.terminal_colors {
                    set_colors(&mut tab.pane.terminal, &next.colors)?;
                    tab.snapshot_dirty = true;
                }
                if changes.cursor {
                    set_cursor_defaults(&mut tab.pane.terminal, next.cursor)?;
                    tab.snapshot_dirty = true;
                }
            }
            session.set_smooth_scroll(next.scroll.smooth());
            if changes.selection_colors {
                session.view.set_selection_colors(&next.colors);
            }
            if changes.background {
                let (alpha, effects_changed) = set_background(&mut self.gpu, window, &next.window);
                let opacity_changed = session.view.set_background_alpha(alpha);
                session.active_mut().frame_dirty |= opacity_changed || effects_changed;
            }
            session.active_mut().frame_dirty |= changes.selection_colors
                || changes.minimum_contrast
                || self.reload.banner.is_some();
        }
        if changes.minimum_contrast {
            self.gpu.set_minimum_contrast(next.minimum_contrast());
        }
        self.reload.banner = None;
        self.config = next;
        Ok(())
    }

    fn handle_event(&mut self, event: Event, window: &mut LinuxWindow) -> Result<bool> {
        match event {
            Event::CloseRequested => return Ok(true),
            Event::Configured {
                activated,
                suspended,
            } => {
                if !suspended && (self.suspended || activated && !self.focused) {
                    window.recover_frame_slot();
                }
                self.suspended = suspended;
                if suspended && let Some(session) = &mut self.session {
                    session.frame_retry = None;
                }
                if self.focused != activated {
                    self.focused = activated;
                    if let Some(session) = &mut self.session {
                        session.active_mut().frame_dirty = true;
                        if !activated {
                            session.link_pointer.clear_link();
                            session.view.set_tab_pointer(None);
                        }
                    }
                }
            }
            Event::Resized {
                device: (width, height),
                scale,
                logical,
            } => {
                let logical = logical.into();
                let device = [width, height];
                if let Some(session) = &mut self.session {
                    session.view.request_resize(scale, logical, device);
                    let tab = session.tabs.active_mut().unwrap();
                    tab.layout_revision = tab.layout_revision.wrapping_add(1);
                } else {
                    let gpu = &mut self.gpu;
                    let mut session = Session::new(
                        scale,
                        logical,
                        device,
                        window,
                        tabs::PaneHost::new(
                            gpu.image_dimension_limit(),
                            LinuxWindow::click_interval(),
                        ),
                        LinuxWindow::hostname()?,
                        &self.config,
                    )?;
                    let (alpha, _) = set_background(gpu, window, &self.config.window);
                    session.view.set_background_alpha(alpha);
                    gpu.resize(device);
                    self.session = Some(session);
                }
            }
            Event::Presented { at, refresh } => {
                if let Some(session) = &mut self.session {
                    session.clock.presented(at, refresh);
                }
            }
            event => {
                if let Some(session) = &mut self.session
                    && let Some(action) =
                        session.input(event, window, self.reload.banner.is_some())?
                {
                    session.apply(action, window, &self.config)?;
                    return Ok(session.tabs.active().is_none());
                }
            }
        }
        Ok(false)
    }
}

fn set_background(
    gpu: &mut Gpu,
    window: &mut impl WindowOps,
    settings: &sumie_config::Window,
) -> (u8, bool) {
    let alpha = gpu::background_alpha(settings.background_opacity());
    let transparent = gpu.set_transparent(alpha != u8::MAX);
    let changed =
        window.set_background_effects(transparent, transparent && settings.background_blur());
    (if transparent { alpha } else { u8::MAX }, changed)
}
