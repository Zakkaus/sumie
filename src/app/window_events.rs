//! Apply window events and reloaded settings to the session, GPU and window.

use sumie_config::{Config, Language};
use sumie_core::KeyAction;
use sumie_locale::Locale;

use super::{App, PaneState, Session, tabs};
use crate::{
    Result,
    gpu::{self, Gpu},
    pane::{set_colors, set_cursor_defaults},
    platform::{Event, LogicalKey, WindowOps},
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
            Event::CloseRequested => {
                return self.session.as_mut().map_or(Ok(true), |session| {
                    session.request_window_close(&self.config)
                });
            }
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
            event => return self.handle_input(event, window),
        }
        Ok(false)
    }

    fn handle_input(&mut self, event: Event, window: &mut impl WindowOps) -> Result<bool> {
        let Some(session) = &mut self.session else {
            return Ok(false);
        };
        if session.confirmation.is_none() && self.hint_drawn && self.banner.is_none() {
            let consumed = super::first_run::consume(&mut self.hint, &event);
            if self.hint.is_none() {
                self.hint_drawn = false;
                session.active_mut().frame_dirty = true;
            }
            if consumed {
                return Ok(false);
            }
        }
        let banner = self.banner.is_some() || self.hint.is_some();
        session.dispatch_input(event, window, banner, &self.config)
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
        let primary_selection = window.has_primary_selection();
        let mut session = Session::new(
            scale,
            logical,
            device,
            window,
            tabs::PaneHost::new(
                environment,
                gpu.image_dimension_limit(),
                W::click_interval(),
                primary_selection,
            ),
            W::hostname()?,
            &self.config,
        )?;
        let (alpha, _) = set_background(gpu, window, &self.config.window);
        session.view.set_background_alpha(alpha);
        gpu.resize(device);
        self.hint = super::first_run::hint(self.config.language());
        self.session = Some(session);
        Ok(())
    }
}

pub(super) enum Confirmation {
    Window,
    Tab(u64),
    Pane(u64),
    Paste { text: String, generation: u64 },
}

impl Confirmation {
    pub(super) fn text(&self, locale: Locale) -> &'static str {
        locale.text(match self {
            Self::Window => "confirm.window",
            Self::Tab(_) => "confirm.tab",
            Self::Pane(_) => "confirm.pane",
            Self::Paste { .. } => "confirm.paste",
        })
    }

    pub(super) fn targets(&self, pane: &PaneState) -> bool {
        match *self {
            Self::Window => false,
            Self::Tab(id) | Self::Pane(id) => id == pane.id,
            Self::Paste { generation, .. } => pane.text_recipient == Some(generation),
        }
    }
}

pub(super) fn running(panes: &[PaneState]) -> Result<bool> {
    for pane in panes {
        let local = &pane.pane;
        if !pane.exited
            && local.pty.child_pid().is_some()
            && match local.terminal.semantic_prompt()? {
                Some(at_prompt) => !at_prompt,
                None => local.pty.has_foreground_job(),
            }
        {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Session {
    pub(super) fn request_window_close(&mut self, config: &Config) -> Result<bool> {
        if config.window.confirm_close() {
            for tab in self.tabs.as_slice() {
                if running(&tab.panes)? {
                    if !matches!(self.confirmation, Some((Confirmation::Window, _))) {
                        self.ask(Confirmation::Window, config.language());
                    }
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    pub(super) fn confirmation_text(&self) -> Option<&'static str> {
        self.confirmation.as_ref().map(|&(_, text)| text)
    }

    pub(super) fn ask(&mut self, confirmation: Confirmation, language: Language) {
        let text = confirmation.text(Locale::from_language(language));
        self.confirmation = Some((confirmation, text));
        self.confirmation_presented = false;
        self.active_mut().frame_dirty = true;
    }

    pub(super) fn dismiss_confirmation(&mut self) -> Option<Confirmation> {
        if let Some(tab) = self.tabs.active_mut() {
            tab.focused_mut().frame_dirty = true;
        }
        self.confirmation_presented = false;
        self.confirmation
            .take()
            .map(|(confirmation, _)| confirmation)
    }

    pub(super) fn dispatch_input(
        &mut self,
        event: Event,
        window: &mut impl WindowOps,
        banner: bool,
        config: &Config,
    ) -> Result<bool> {
        if self.confirmation.is_some() {
            return self.answer_confirmation(event, window);
        }
        if let Some(action) = self.input(event, window, banner, Some(config.language()))? {
            self.apply(action, window, config)?;
        }
        Ok(self.tabs.active().is_none())
    }

    fn answer_confirmation(&mut self, event: Event, window: &mut impl WindowOps) -> Result<bool> {
        if self.routing.release_owner(&event).is_some() {
            self.input(event, window, true, None)?;
            return Ok(false);
        }
        let Event::Key {
            logical,
            action: KeyAction::Press,
            modifiers,
            ..
        } = event
        else {
            return Ok(false);
        };
        if modifiers.control || modifiers.alt || modifiers.right_alt || modifiers.super_key {
            return Ok(false);
        }
        let enter = matches!(logical, LogicalKey::Enter | LogicalKey::NumpadEnter);
        let paste = matches!(self.confirmation, Some((Confirmation::Paste { .. }, _)));
        if matches!(
            logical,
            LogicalKey::Escape | LogicalKey::Character('n' | 'N')
        ) || (enter && !paste)
        {
            self.dismiss_confirmation();
            return Ok(false);
        }
        if !self.confirmation_presented
            || !(enter || matches!(logical, LogicalKey::Character('y' | 'Y')))
        {
            return Ok(false);
        }
        match self.dismiss_confirmation().expect("pending confirmation") {
            Confirmation::Window => return Ok(true),
            Confirmation::Paste { text, generation } => {
                self.input(
                    Event::ClipboardText { text, generation },
                    window,
                    false,
                    None,
                )?;
            }
            close @ (Confirmation::Tab(id) | Confirmation::Pane(id)) => {
                if let Some(index) = self
                    .tabs
                    .as_slice()
                    .iter()
                    .position(|tab| tab.panes.binary_search_by_key(&id, |pane| pane.id).is_ok())
                {
                    if matches!(close, Confirmation::Tab(_)) {
                        self.close_tab(index, window)?;
                    } else {
                        self.close_pane(index, id, window)?;
                    }
                }
            }
        }
        Ok(self.tabs.active().is_none())
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
