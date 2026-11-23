mod directory;
mod highlights;
mod links;
mod search;
mod tabs;

use std::time::{Duration, Instant};

use sumie_core::{
    CellPosition, CellRange, Pane, PaneCommand, PaneEffect, Selection, WindowTabs,
    mouse::{MouseEvent, SelectStep, SelectionUnit},
};
use sumie_render::{SurfaceError, SurfaceFrame};
use sumie_vt::Snapshot;
use sumie_wayland::{Event, SourceHandle};

use crate::{
    Result, capture,
    gpu::Gpu,
    input,
    pane::{LocalPane, set_colors, set_cursor_defaults},
    reload::Reload,
    sources::{self, Sources},
    view::{FontSettings, View, clear_color},
};

const FRAME_RETRY_DELAY: Duration = Duration::from_millis(50);

pub(crate) fn run(capture: capture::Options) -> Result<()> {
    let mut reload = Reload::new()?;
    let config = reload.load()?.unwrap_or_else(|error| {
        reload.load_error(&error);
        sumie_config::Config::default()
    });
    let gpu = Gpu::new(capture.screenshot.is_some(), config.minimum_contrast())?;
    reload.register(&gpu.loop_handle())?;
    let mut app = App {
        gpu: Some(gpu),
        capture,
        config,
        reload,
        session: None,
        focused: false,
        suspended: false,
        events: Vec::new(),
    };
    let result = app.drive();
    let close = app.session.as_mut().map_or(Ok(()), Session::close_all);
    drop(app.gpu.take());
    let reap = app.session.as_mut().map_or(Ok(()), Session::reap_closed);
    result.and(close).and(reap)
}

struct App {
    gpu: Option<Gpu>,
    capture: capture::Options,
    config: sumie_config::Config,
    reload: Reload,
    session: Option<Session>,
    focused: bool,
    suspended: bool,
    events: Vec<Event>,
}

struct Session {
    tabs: WindowTabs<Tab>,
    routing: input::Routing,
    view: View,
    frame_retry: Option<Instant>,
    cursor_hidden: bool,
    handle: SourceHandle,
    click_interval: Option<u32>,
    home: std::path::PathBuf,
    host: String,
    shell: String,
    closing: Vec<(Sources, sumie_pty::Pty)>,
    pointer: tabs::BarPointer,
    link_pointer: links::Pointer,
}

struct Tab {
    exited: bool,
    paste_recipient: Option<u64>,
    search: Option<search::Bar>,
    selection: Selection,
    selected_text: String,
    pane: LocalPane,
    sources: Sources,
    directory: directory::Directory,
    title: String,
    snapshot: Option<Snapshot>,
    snapshot_dirty: bool,
    frame_dirty: bool,
    hover: links::Hover,
}

impl App {
    fn drive(&mut self) -> Result<()> {
        let handle = self.gpu.as_ref().unwrap().loop_handle();
        loop {
            let retry = self
                .session
                .as_ref()
                .and_then(|session| session.frame_retry);
            let timeout = retry
                .into_iter()
                .chain(self.reload.deadline)
                .chain(
                    self.session
                        .iter()
                        .flat_map(|s| s.closing.iter().filter_map(|(_, pty)| pty.close_deadline())),
                )
                .chain(
                    self.session
                        .as_ref()
                        .filter(|s| s.active().search_pending())
                        .map(|_| Instant::now()),
                )
                .min()
                .map(|at| at.saturating_duration_since(Instant::now()));
            self.gpu
                .as_mut()
                .unwrap()
                .dispatch(&mut self.events, timeout)?;
            let mut exited = false;
            if let Some(session) = &mut self.session {
                exited = session.service_pty(self.gpu.as_mut().unwrap())?;
            }
            // Route this dispatch's input before child exits can select another tab.
            let mut events = std::mem::take(&mut self.events);
            for event in events.drain(..) {
                if self.handle_event(event, &handle)? {
                    return Ok(());
                }
            }
            self.events = events;
            if exited
                && let Some(session) = &mut self.session
                && session.close_exited(self.gpu.as_mut().unwrap())?
            {
                return Ok(());
            }
            self.reload_config()?;
            if let Some(session) = &mut self.session {
                session.active_mut().advance_search()?;
                for tab in session.tabs.as_mut_slice() {
                    tab.sources.watch_input(tab.pane.pty.input_fd())?;
                }
                let gpu = self.gpu.as_mut().unwrap();
                let ready = !self.suspended
                    && gpu.frame_slot_available()
                    && session.frame_retry.is_none_or(|at| Instant::now() >= at);
                if ready {
                    session.active_mut().update_snapshot()?;
                }
                let banner = self.reload.banner.as_deref();
                session.update_link_hover(gpu, banner.is_some())?;
                session.watch_blink(self.focused && !self.suspended, banner)?;
                if ready && session.draw(gpu, &self.capture, banner, self.focused)? {
                    return Ok(());
                }
            }
        }
    }

    fn reload_config(&mut self) -> Result<()> {
        let Some(next) = self.reload.pending()? else {
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
            if changes.font {
                match session.view.set_font(next.font.clone()) {
                    Ok(true) => session.resize_grid()?,
                    Ok(false) => (),
                    Err(error) => {
                        session.active_mut().frame_dirty |=
                            self.reload.show_error(format!("sumie.toml:1:1: {error}"));
                        return Ok(());
                    }
                }
            }
            if changes.scrollback_limit {
                for tab in session.tabs.as_mut_slice() {
                    tab.pane
                        .terminal
                        .set_scrollback_bytes(next.scrollback_limit())?;
                    tab.snapshot_dirty = true;
                    if let Some(bar) = &mut tab.search {
                        bar.output(&tab.pane.terminal)?;
                        tab.frame_dirty = true;
                    }
                }
            }
            for tab in session.tabs.as_mut_slice() {
                if changes.terminal_colors {
                    set_colors(&mut tab.pane.terminal, &next.colors)?;
                    tab.snapshot_dirty = true;
                }
                if changes.cursor {
                    set_cursor_defaults(&mut tab.pane.terminal, next.cursor)?;
                    tab.snapshot_dirty = true;
                }
            }
            if changes.selection_colors {
                session.view.set_selection_colors(&next.colors);
            }
            session.active_mut().frame_dirty |= changes.selection_colors
                || changes.minimum_contrast
                || self.reload.banner.is_some();
        }
        if changes.minimum_contrast {
            self.gpu
                .as_mut()
                .unwrap()
                .set_minimum_contrast(next.minimum_contrast());
        }
        self.reload.banner = None;
        self.config = next;
        Ok(())
    }

    fn handle_event(&mut self, event: Event, handle: &SourceHandle) -> Result<bool> {
        match event {
            Event::CloseRequested => return Ok(true),
            Event::Presentation(feedback) => tracing::trace!(?feedback, "presentation feedback"),
            Event::Configured {
                activated,
                suspended,
                ..
            } => {
                self.suspended = suspended;
                if suspended && let Some(session) = &mut self.session {
                    session.frame_retry = None;
                }
                if self.focused != activated {
                    self.focused = activated;
                    if let Some(session) = &mut self.session {
                        session.active_mut().frame_dirty = true;
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
                    session.resize(scale, logical, device)?;
                } else {
                    self.session = Some(Session::new(
                        scale,
                        logical,
                        device,
                        handle.clone(),
                        &self.config,
                    )?);
                }
                self.gpu.as_mut().unwrap().resize(device);
            }
            event => {
                if let Some(session) = &mut self.session {
                    let gpu = self.gpu.as_mut().unwrap();
                    if let Some(action) = session.input(event, gpu, self.reload.banner.is_some())? {
                        session.apply(action, gpu, &self.config)?;
                        return Ok(session.tabs.active().is_none());
                    }
                }
            }
        }
        Ok(false)
    }
}

impl Session {
    fn new(
        scale: u32,
        logical: [u32; 2],
        device: [u32; 2],
        handle: SourceHandle,
        config: &sumie_config::Config,
    ) -> Result<Self> {
        let font = FontSettings::from_config(config.font.clone())?;
        let view = View::new(scale, logical, device, font, &config.colors)?;
        let home = std::env::home_dir().ok_or("no home directory for login shell")?;
        let click_interval = sumie_wayland::double_click_interval();
        let first = Tab::open(&view, &handle, config, &home, click_interval)?;
        Ok(Self {
            tabs: WindowTabs::new(first),
            routing: input::Routing::default(),
            click_interval,
            view,
            frame_retry: None,
            cursor_hidden: false,
            handle,
            home,
            host: std::fs::read_to_string("/proc/sys/kernel/hostname")?
                .trim_end()
                .to_owned(),
            shell: directory::shell_name(),
            closing: Vec::new(),
            pointer: tabs::BarPointer::default(),
            link_pointer: links::Pointer::default(),
        })
    }

    fn resize(&mut self, scale: u32, logical: [u32; 2], device: [u32; 2]) -> Result<()> {
        self.view.resize(scale, logical, device)?;
        self.resize_grid()
    }

    fn resize_grid(&mut self) -> Result<()> {
        for tab in self.tabs.as_mut_slice() {
            tab.resize_grid(&self.view)?;
        }
        Ok(())
    }

    fn zoom(&mut self, action: sumie_core::FontZoom) -> Result<()> {
        match self.view.zoom(action) {
            Ok(true) => self.resize_grid(),
            Ok(false) => Ok(()),
            Err(error) => {
                tracing::warn!(%error, "could not zoom terminal font");
                Ok(())
            }
        }
    }

    fn input(&mut self, event: Event, gpu: &mut Gpu, banner: bool) -> Result<Option<tabs::Action>> {
        self.link_pointer.record(&event);
        if let std::ops::ControlFlow::Break(action) = self.bar_input(&event) {
            return Ok(action);
        }
        let Some((index, recipient)) = self.input_target(&event) else {
            return Ok(None);
        };
        let tab = &mut self.tabs.as_mut_slice()[index];
        let routing = &mut self.routing;
        if let Event::ModifiersChanged(modifiers) = event {
            tab.pane
                .pointer_modifiers_changed(input::key_modifiers(modifiers));
            return Ok(None);
        }
        if matches!(
            event,
            Event::Key { pressed: true, .. } | Event::ImeCommit { .. }
        ) {
            tab.sources.reset_blink();
        }
        let searching = tab.search.is_some();
        let effect = if let Event::Pointer {
            phase,
            position,
            serial,
            modifiers,
        } = event
        {
            let phase = input::mouse_phase(phase);
            let hit = || self.link_pointer.hit(&self.view, searching, banner);
            let link = tab.prepare_pointer_press(phase, hit)?;
            let (cell, pixel) = self.view.pointer_position(position);
            let mouse = MouseEvent {
                phase,
                cell,
                pixel,
                modifiers: input::key_modifiers(modifiers),
                link,
            };
            let hit = || self.view.search_hit(position);
            match routing.pointer(&mut tab.pane, searching, mouse, hit)? {
                input::Effect::Select(step) if tab.select(step, cell)? => {
                    input::Effect::Copy(serial)
                }
                effect => effect,
            }
        } else {
            input::route(recipient, &mut tab.pane, routing, searching, event)?
        };
        match effect {
            input::Effect::Tab(action) => return Ok(Some(tabs::Action::Key(action))),
            input::Effect::Zoom(action) => self.zoom(action)?,
            input::Effect::Search(action) => {
                if matches!(
                    action,
                    input::SearchAction::Open | input::SearchAction::Close
                ) {
                    tab.pane.reset_pointer_gestures();
                    tab.paste_recipient = None;
                    gpu.set_input_generation(routing.change_recipient());
                }
                tab.search_action(action, &mut self.view)?;
            }
            input::Effect::OpenLink => tab.hover.open(),
            input::Effect::Copy(serial) => tab.copy_selection(gpu, serial)?,
            input::Effect::ViewportChanged => {
                tab.frame_dirty |= searching || tab.selection.range().is_some();
                tab.clear_selection();
                tab.snapshot_dirty = true;
            }
            input::Effect::ReadClipboard => {
                gpu.read_clipboard(*tab.paste_recipient.get_or_insert(routing.generation));
            }
            input::Effect::Preedit(text) if text != self.view.preedit => {
                self.view.preedit = text;
                tab.frame_dirty = true;
            }
            _ => (),
        }
        Ok(None)
    }
}

impl Tab {
    fn copy_selection(&mut self, gpu: &mut Gpu, serial: u32) -> Result<()> {
        self.update_snapshot()?;
        if let Some(range) = self.selection.range()
            && let PaneEffect::Copied(text) = self.pane.command(PaneCommand::Copy(range))?
        {
            gpu.write_clipboard(serial, text);
        }
        Ok(())
    }

    /// Returns true only on release of a completed selection.
    fn select(&mut self, step: SelectStep, hit: CellPosition) -> Result<bool> {
        self.update_snapshot()?;
        let old = self.selection.range();
        let resolve = |from: CellPosition, toward, unit| -> Result<Option<CellRange>> {
            let native = self.pane.terminal.selection_group(from, toward, unit)?;
            if native.is_some() || unit == SelectionUnit::Word {
                return Ok(native);
            }
            let row = &self
                .snapshot
                .as_ref()
                .expect("current pointer snapshot")
                .rows[usize::from(from.row)];
            let mut range = CellRange::group(from, row[usize::from(from.column)].width);
            if unit == SelectionUnit::Line {
                range.start.column = 0;
                range.end.column = u16::try_from(row.len() - 1)?;
            }
            Ok(Some(range))
        };
        match step {
            SelectStep::Begin(unit) => {
                let group = if unit == SelectionUnit::Cell {
                    None
                } else {
                    resolve(hit, hit, unit)?
                };
                self.selection
                    .press(hit, group, unit != SelectionUnit::Cell);
            }
            SelectStep::Drag(unit) | SelectStep::Finish(unit) => {
                self.selection
                    .drag(hit, |from, toward| resolve(from, toward, unit))?;
            }
            SelectStep::Clear => self.clear_selection(),
        }
        let range = self.selection.range();
        if old != range {
            self.frame_dirty = true;
            if let Some(range) = range {
                self.pane
                    .terminal
                    .write_selection_text(range, &mut self.selected_text)?;
            } else {
                self.selected_text.clear();
            }
        }
        Ok(matches!(step, SelectStep::Finish(_)) && self.selection.release().is_some())
    }

    fn clear_selection(&mut self) {
        self.selection = Selection::default();
        self.selected_text.clear();
    }

    fn update_snapshot(&mut self) -> Result<()> {
        if self.snapshot_dirty {
            self.snapshot_dirty = false;
            self.hover.invalidate();
            let next = self.pane.snapshot()?;
            if let Some(range) = self.selection.range()
                && !self
                    .pane
                    .terminal
                    .selection_text_matches(range, &self.selected_text)?
            {
                self.clear_selection();
                self.frame_dirty = true;
            }
            if self.snapshot.as_ref() != Some(&next) {
                self.snapshot = Some(next);
                self.frame_dirty = true;
            }
        }
        Ok(())
    }
}

impl Session {
    fn watch_blink(&mut self, eligible: bool, banner: Option<&str>) -> Result<()> {
        let tab = self.active();
        let cursor = tab.snapshot.as_ref().and_then(|snapshot| snapshot.cursor);
        let enabled = eligible
            && tab.search.is_none()
            && self.view.preedit.is_empty()
            && cursor.is_some_and(|cursor| {
                cursor.visible && cursor.blink && (cursor.row != 0 || banner.is_none())
            });
        self.active_mut().sources.watch_blink(enabled)
    }

    fn draw(
        &mut self,
        gpu: &mut Gpu,
        options: &capture::Options,
        banner: Option<&str>,
        focused: bool,
    ) -> Result<bool> {
        self.frame_retry = None;
        let tab = self.tabs.active_mut().unwrap();
        let cursor_hidden = tab.sources.cursor_hidden();
        if !tab.frame_dirty && self.cursor_hidden == cursor_hidden {
            return Ok(false);
        }
        if tab.frame_dirty {
            tab.prepare_search_view(&mut self.view)?;
            let snapshot = tab.snapshot.as_ref().expect("initial snapshot");
            self.view.build(
                snapshot,
                tab.selection.range(),
                tab.hover.range(),
                banner,
                focused,
            );
            let background = snapshot.background;
            self.view.draw_tabs(
                self.tabs.as_slice().iter().map(|t| t.label(&self.shell)),
                self.tabs.active_index().unwrap(),
                background,
            );
            gpu.upload_atlas(&self.view.atlas());
            self.view.atlas_uploaded();
        }
        let tab = self.tabs.active_mut().unwrap();
        let snapshot = tab.snapshot.as_ref().expect("initial snapshot");
        let final_frame = options
            .frames
            .is_some_and(|count| count == gpu.present_count() + 1);
        let frame = gpu.draw(
            &self.view.draw_list(cursor_hidden),
            clear_color(snapshot.background),
            final_frame && options.screenshot.is_some(),
        );
        if tab.frame_dirty
            && let Some(rect) = self.view.ime_rectangle(snapshot.cursor)
        {
            gpu.set_cursor_rectangle(rect);
        }
        match frame {
            Ok((SurfaceFrame::Presented { suboptimal }, readback)) => {
                self.view.frame_presented();
                tab.frame_dirty = false;
                self.cursor_hidden = cursor_hidden;
                tracing::debug!(count = gpu.present_count(), "submitted terminal frame");
                if suboptimal {
                    gpu.resize(self.view.device);
                }
                if let (Some(readback), Some(path)) = (readback, &options.screenshot) {
                    readback?.write(gpu.device(), path)?;
                }
                return Ok(final_frame);
            }
            Ok((SurfaceFrame::Suspended, _)) => return Ok(false),
            Ok((SurfaceFrame::Timeout | SurfaceFrame::Occluded, _)) => (),
            Err(SurfaceError::Outdated) => gpu.resize(self.view.device),
            Err(error) => return Err(error.into()),
        }
        self.frame_retry = Some(Instant::now() + FRAME_RETRY_DELAY);
        Ok(false)
    }
}
