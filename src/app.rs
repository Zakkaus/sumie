mod directory;
mod highlights;
mod links;
mod pane_input;
mod pane_layout;
mod search;
mod tabs;
mod viewer;

use std::time::{Duration, Instant};

use sumie_core::{
    CellPosition, CellRange, Pane, PaneCommand, PaneEffect, Selection, WindowTabs,
    mouse::{SelectStep, SelectionUnit},
    splits::{Layout, Rect, SplitTree},
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
    view::{FontSettings, PaneFocus, View},
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
    tabs: WindowTabs<PaneTab>,
    routing: input::Routing,
    view: View,
    frame_retry: Option<Instant>,
    cursor_hidden: bool,
    spawn: tabs::PaneHost,
    next_id: u64,
    home: std::path::PathBuf,
    host: String,
    shell: String,
    closing: Vec<(Sources, sumie_pty::Pty)>,
    pointer: tabs::BarPointer,
    pane_pointer: pane_input::Pointer,
    link_pointer: links::Pointer,
}

struct PaneTab {
    tree: SplitTree,
    layout: Layout,
    layout_revision: u64,
    panes: Vec<PaneState>,
    activity: bool,
}

struct PaneState {
    id: u64,
    rect: Rect,
    cell: [u32; 2],
    viewport_px: [u16; 2],
    exited: bool,
    paste_recipient: Option<u64>,
    search: Option<search::Bar>,
    viewer: Option<viewer::Viewer>,
    selection: Selection,
    selected_text: String,
    pane: LocalPane,
    sources: Sources,
    directory: directory::Directory,
    title: String,
    snapshot: Option<Snapshot>,
    snapshot_dirty: bool,
    frame_dirty: bool,
    activity_after: Option<Instant>,
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
                for tab in session.all_panes_mut() {
                    tab.sources.watch_input(tab.pane.pty.input_fd())?;
                }
                let gpu = self.gpu.as_mut().unwrap();
                if !self.suspended && session.pane_pointer.take_frame_recovery() {
                    gpu.recover_frame_slot();
                }
                let ready = !self.suspended
                    && gpu.frame_slot_available()
                    && session.frame_retry.is_none_or(|at| Instant::now() >= at);
                if ready {
                    session.tabs.active_mut().unwrap().refresh_visible()?;
                }
                let banner = self.reload.banner.as_deref();
                session.update_link_hover(gpu, banner.is_some(), self.focused)?;
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
            if changes.selection_colors {
                session.view.set_selection_colors(&next.colors);
            }
            if changes.background {
                let (alpha, effects_changed) =
                    self.gpu.as_mut().unwrap().set_background(&next.window);
                let opacity_changed = session.view.set_background_alpha(alpha);
                session.active_mut().frame_dirty |= opacity_changed || effects_changed;
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
                if !suspended && (self.suspended || activated && !self.focused) {
                    self.gpu.as_mut().unwrap().recover_frame_slot();
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
                    session.resize(scale, logical, device)?;
                } else {
                    let gpu = self.gpu.as_mut().unwrap();
                    let mut session = Session::new(
                        scale,
                        logical,
                        device,
                        handle.clone(),
                        gpu.image_dimension_limit(),
                        &self.config,
                    )?;
                    let (alpha, _) = gpu.set_background(&self.config.window);
                    session.view.set_background_alpha(alpha);
                    self.session = Some(session);
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
        image_dimension_limit: u32,
        config: &sumie_config::Config,
    ) -> Result<Self> {
        let font = FontSettings::from_config(config.font.clone())?;
        let view = View::new(
            scale,
            logical,
            device,
            font,
            &config.colors,
            config.window.padding(),
        )?;
        let home = std::env::home_dir().ok_or("no home directory for login shell")?;
        let spawn = tabs::PaneHost::new(handle, image_dimension_limit);
        let first = spawn.open(0, view.content_rect(), &view, config, &home)?;
        Ok(Self {
            tabs: WindowTabs::new(PaneTab::new(first)),
            routing: input::Routing::default(),
            next_id: 1,
            view,
            frame_retry: None,
            cursor_hidden: false,
            spawn,
            home,
            host: std::fs::read_to_string("/proc/sys/kernel/hostname")?
                .trim_end()
                .to_owned(),
            shell: directory::shell_name(),
            closing: Vec::new(),
            pointer: tabs::BarPointer::default(),
            pane_pointer: pane_input::Pointer::default(),
            link_pointer: links::Pointer::default(),
        })
    }

    fn resize(&mut self, scale: u32, logical: [u32; 2], device: [u32; 2]) -> Result<()> {
        self.view.resize(scale, logical, device)?;
        self.resize_grid()
    }

    fn resize_grid(&mut self) -> Result<()> {
        self.link_pointer.clear_link();
        for tab in self.tabs.as_mut_slice() {
            tab.resize_grid(&self.view)?;
        }
        self.active_mut().frame_dirty = true;
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
}

impl PaneState {
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
            SelectStep::DragFromLink(origin) => {
                self.selection.press(origin, None, false);
                self.selection.drag(hit, |from, toward| {
                    resolve(from, toward, SelectionUnit::Cell)
                })?;
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
}

impl Session {
    fn watch_blink(&mut self, eligible: bool, banner: Option<&str>) -> Result<()> {
        let tab = self.active();
        let cursor = tab.snapshot.as_ref().and_then(|snapshot| snapshot.cursor);
        let enabled = eligible
            && tab.viewer.is_none()
            && tab.search.is_none()
            && self.view.preedit.is_empty()
            && cursor.is_some_and(|cursor| {
                cursor.visible && cursor.blink && (cursor.row != 0 || banner.is_none())
            });
        self.active_mut().sources.watch_blink(enabled)
    }

    fn rebuild(&mut self, gpu: &mut Gpu, banner: Option<&str>, focused: bool) -> Result<()> {
        let tab = self.tabs.active().unwrap();
        let active = tab.focused();
        active.prepare_search_view(&mut self.view)?;
        self.view.begin_frame(
            tab.layout.dividers.iter().map(|d| d.rect),
            active.snapshot.as_ref().unwrap().background,
        );
        for &(id, rect) in &tab.layout.panes {
            let pane = tab.get(id);
            let focus = PaneFocus {
                id,
                dirty: pane.frame_dirty,
                active: id == active.id,
                window_focused: focused,
                viewer: pane.viewer.as_ref().map(|viewer| &viewer.placement),
            };
            self.view.build_pane(
                rect,
                pane.snapshot.as_ref().expect("visible pane snapshot"),
                pane.selection.range(),
                pane.hover.highlight(),
                if focus.active { banner } else { None },
                focus,
            );
        }
        self.view.end_frame();
        self.draw_tab_strip();
        gpu.upload_atlas(&self.view.atlas());
        self.view.atlas_uploaded();
        Ok(())
    }

    fn draw(
        &mut self,
        gpu: &mut Gpu,
        options: &capture::Options,
        banner: Option<&str>,
        focused: bool,
    ) -> Result<bool> {
        self.frame_retry = None;
        let tab = self.tabs.active().unwrap();
        let cursor_hidden = tab.focused().sources.cursor_hidden();
        let dirty = !self.pane_pointer.layout_is_presented(tab)
            || tab
                .layout
                .panes
                .iter()
                .any(|&(id, _)| tab.get(id).frame_dirty);
        if !dirty
            && self.cursor_hidden == cursor_hidden
            && !self.view.thumbnail_hover_dirty()
            && !gpu.background_commit_needed()
        {
            return Ok(false);
        }
        if dirty {
            self.rebuild(gpu, banner, focused)?;
        }
        let tab = self.tabs.active_mut().unwrap();
        let active = tab.focused();
        let final_frame = options
            .frames
            .is_some_and(|count| count == gpu.present_count() + 1);
        let capture = final_frame && options.screenshot.is_some();
        let frame = gpu.draw(&self.view.frame(cursor_hidden), capture);
        if dirty
            && let Some(rect) = self
                .view
                .ime_rectangle(active.snapshot.as_ref().unwrap().cursor, active.rect)
        {
            gpu.set_cursor_rectangle(rect);
        }
        match frame {
            Ok((SurfaceFrame::Presented { suboptimal }, readback)) => {
                self.view.frame_presented();
                self.view.images_presented(gpu.presented_thumbnails());
                self.pane_pointer.present_layout(tab);
                self.panes_presented(gpu)?;
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
