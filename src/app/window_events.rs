//! Apply window events and reloaded settings to the session, GPU and window.

use sumie_config::Config;

use super::{App, Session, tabs};
use crate::{
    Result,
    gpu::{self, Gpu},
    pane::{set_colors, set_cursor_defaults},
    platform::{Event, WindowOps},
};

impl App {
    /// Apply a settings result. A diagnostic shows in the banner and keeps the active settings;
    /// any settings, even identical ones, remove the banner.
    pub(crate) fn apply_config(
        &mut self,
        next: Result<Config, String>,
        window: &mut impl WindowOps,
    ) -> Result<()> {
        let next = match next {
            Ok(next) => next,
            Err(message) => {
                if show_error(&mut self.banner, message)
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
                            show_error(&mut self.banner, format!("sumie.toml:1:1: {error}"));
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
            session.active_mut().frame_dirty |=
                changes.selection_colors || changes.minimum_contrast || self.banner.is_some();
        }
        if changes.minimum_contrast {
            self.gpu.set_minimum_contrast(next.minimum_contrast());
        }
        self.banner = None;
        self.config = next;
        Ok(())
    }

    /// Apply one window event. Returns whether the window closes.
    pub(crate) fn handle_event<W: WindowOps>(
        &mut self,
        event: Event,
        window: &mut W,
    ) -> Result<bool> {
        match event {
            Event::CloseRequested => return Ok(true),
            Event::Exposed => {
                if let Some(session) = &mut self.session {
                    session.active_mut().frame_dirty = true;
                }
            }
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
                    self.start_session(scale, logical, device, window)?;
                }
            }
            Event::KeyboardFocus(focused) => {
                if let Some(session) = &mut self.session {
                    session.keyboard_focus(focused)?;
                }
            }
            Event::Presented { at, refresh } => {
                if let Some(session) = &mut self.session {
                    session.clock.presented(at, refresh);
                }
            }
            event => {
                if let Some(session) = &mut self.session
                    && let Some(action) = session.input(event, window, self.banner.is_some())?
                {
                    session.apply(action, window, &self.config)?;
                    return Ok(session.tabs.active().is_none());
                }
            }
        }
        Ok(false)
    }

    /// Start the session at the window's first size.
    fn start_session<W: WindowOps>(
        &mut self,
        scale: u32,
        logical: [u32; 2],
        device: [u32; 2],
        window: &mut W,
    ) -> Result<()> {
        let gpu = &mut self.gpu;
        let environment = self.environment.take().ok_or("the session started twice")?;
        let mut session = Session::new(
            scale,
            logical,
            device,
            window,
            tabs::PaneHost::new(
                environment,
                gpu.image_dimension_limit(),
                W::click_interval(),
            ),
            W::hostname()?,
            &self.config,
        )?;
        let (alpha, _) = set_background(gpu, window, &self.config.window);
        session.view.set_background_alpha(alpha);
        gpu.resize(device);
        self.session = Some(session);
        Ok(())
    }
}

/// Show a settings diagnostic. Returns whether the banner changed.
pub(crate) fn show_error(banner: &mut Option<String>, message: String) -> bool {
    if banner.as_ref() == Some(&message) {
        return false;
    }
    tracing::warn!(error = %message, "invalid configuration");
    *banner = Some(message);
    true
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
