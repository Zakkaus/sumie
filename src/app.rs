mod animation;
pub(crate) use animation::Wake;
mod directory;
mod frame_log;
mod frame_steps;
pub(crate) use frame_steps::App;
#[cfg(target_os = "macos")]
pub(crate) use window_events::show_error;
mod highlights;
mod lifetime;
mod links;
mod notices;
mod pane_input;
mod pane_layout;
mod scroll;
mod search;
mod tabs;
mod viewer;
mod window_events;

use std::time::{Duration, Instant};

use sumie_core::{
    CellPosition, CellRange, Pane, PaneCommand, PaneEffect, Selection, WindowTabs,
    mouse::{SelectStep, SelectionUnit},
    notify::{Gate, Progress},
    splits::{Layout, Rect, SplitTree},
};
use sumie_render::{SurfaceError, SurfaceFrame};
use sumie_vt::Snapshot;

use crate::{
    Result, capture,
    gpu::Gpu,
    input,
    pane::LocalPane,
    platform::{PaneWatches, WindowOps},
    view::{FontSettings, PaneFocus, View},
};

const FRAME_RETRY_DELAY: Duration = Duration::from_millis(50);

pub(crate) struct Session {
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
    closing: Vec<(u64, sumie_pty::Pty)>,
    pointer: tabs::BarPointer,
    pane_pointer: pane_input::Pointer,
    link_pointer: links::Pointer,
    clock: scroll::FrameClock,
    smooth_scroll: bool,
    notices: Gate,
    frame_log: frame_log::FrameLog,
}

struct PaneTab {
    tree: SplitTree,
    layout: Layout,
    layout_revision: u64,
    panes: Vec<PaneState>,
    activity: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Launch {
    Shell,
    Settings,
}

struct PaneState {
    id: u64,
    rect: Rect,
    launch: Launch,
    cell: [u32; 2],
    viewport_px: [u16; 2],
    exited: bool,
    paste_recipient: Option<u64>,
    search: Option<search::Bar>,
    viewer: Option<viewer::Viewer>,
    selection: Selection,
    selected_text: String,
    pane: LocalPane,
    directory: directory::Directory,
    title: String,
    snapshot: Option<Snapshot>,
    snapshot_dirty: bool,
    snapshot_pace: tabs::SnapshotPace,
    frame_dirty: bool,
    activity_after: Option<Instant>,
    hover: links::Hover,
    scroll: scroll::PaneScroll,
    /// The latest OSC 9;4 report and when it arrived.
    progress: Option<(Progress, Instant)>,
}

impl Session {
    fn new(
        scale: u32,
        logical: [u32; 2],
        device: [u32; 2],
        watches: &mut impl PaneWatches,
        spawn: tabs::PaneHost,
        host: String,
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
        let first = spawn.open(
            (0, view.content_rect()),
            &view,
            config,
            &home,
            Launch::Shell,
            watches,
        )?;
        let mut routing = input::Routing::default();
        routing.set_bindings(&config.keybinds);
        Ok(Self {
            tabs: WindowTabs::new(PaneTab::new(first)),
            routing,
            next_id: 1,
            view,
            frame_retry: None,
            cursor_hidden: false,
            spawn,
            home,
            host,
            shell: directory::shell_name(),
            closing: Vec::new(),
            pointer: tabs::BarPointer::default(),
            pane_pointer: pane_input::Pointer::default(),
            link_pointer: links::Pointer::default(),
            clock: scroll::FrameClock::new(),
            smooth_scroll: config.scroll.smooth(),
            notices: Gate::default(),
            frame_log: frame_log::FrameLog::from_env(),
        })
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
    fn copy_selection(&mut self, window: &mut impl WindowOps) -> Result<()> {
        self.update_snapshot()?;
        if let Some(range) = self.selection.range()
            && let PaneEffect::Copied(text) = self.pane.command(PaneCommand::Copy(range))?
        {
            window.write_clipboard(text);
        }
        Ok(())
    }

    /// Returns true only on release of a completed selection that the platform copies.
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
            SelectStep::DragFrom(origin) => {
                self.selection.press(origin, None, false);
                self.selection.drag(hit, |from, toward| {
                    resolve(from, toward, SelectionUnit::Cell)
                })?;
            }
            SelectStep::Drag(unit) | SelectStep::Finish { unit, .. } => {
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
        let SelectStep::Finish { copy, .. } = step else {
            return Ok(false);
        };
        Ok(self.selection.release().is_some() && copy)
    }

    fn clear_selection(&mut self) {
        self.selection = Selection::default();
        self.selected_text.clear();
    }
}

impl Session {
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
                scroll: pane.scroll.offset(),
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
        self.show_scroll();
        self.draw_tab_strip();
        gpu.upload_atlas(&self.view.atlas());
        self.view.atlas_uploaded();
        Ok(())
    }

    fn draw<W: WindowOps>(
        &mut self,
        gpu: &mut Gpu,
        window: &mut W,
        options: &capture::Options,
        banner: Option<&str>,
        focused: bool,
    ) -> Result<bool> {
        self.frame_retry = None;
        let tab = self.tabs.active().unwrap();
        let cursor_hidden = window.cursor_hidden(tab.focused().id);
        let dirty = !self.pane_pointer.layout_is_presented(tab)
            || tab
                .layout
                .panes
                .iter()
                .any(|&(id, _)| tab.get(id).frame_dirty)
            || self.scroll_pending();
        if !dirty
            && self.cursor_hidden == cursor_hidden
            && !self.view.thumbnail_hover_dirty()
            && !window.background_commit_needed()
        {
            return Ok(false);
        }
        if dirty {
            self.rebuild(gpu, banner, focused)?;
        }
        let tab = self.tabs.active_mut().unwrap();
        let panes = tab.layout.panes.iter();
        self.frame_log
            .painted(|| panes.filter(|&&(id, _)| tab.get(id).frame_dirty).count());
        let active = tab.focused();
        let final_frame = options
            .frames
            .is_some_and(|count| count == gpu.present_count() + 1);
        let capture = final_frame && options.screenshot.is_some();
        let sources =
            |id| Some(&tab.get(id).pane.terminal as &dyn sumie_core::graphics::ImageSource);
        let submitted = || self.frame_log.mark(frame_log::Stage::Submitted);
        let frame = gpu.draw(
            window,
            &self.view.frame(cursor_hidden, sources),
            capture,
            submitted,
        );
        if dirty
            && let Some(rect) = self.view.ime_rectangle(
                active.snapshot.as_ref().unwrap().cursor,
                active.rect,
                W::logical_rect,
            )
        {
            window.set_cursor_rectangle(rect);
        }
        match frame {
            Ok((SurfaceFrame::Presented { suboptimal }, readback)) => {
                window.cursor_rectangle_presented();
                self.view.frame_presented();
                self.view.images_presented(gpu.presented_images());
                self.pane_pointer.present_layout(tab);
                self.frame_log
                    .presented(self.view.repainted_panes().filter(|_| dirty));
                self.panes_presented(gpu, window)?;
                self.cursor_hidden = cursor_hidden;
                gpu.log_present();
                if suboptimal {
                    gpu.resize(self.view.device);
                }
                if let (Some(readback), Some(path)) = (readback, &options.screenshot) {
                    readback?.write(gpu.device(), path)?;
                }
                return Ok(final_frame);
            }
            Ok((SurfaceFrame::Suspended, _)) => return Ok(false),
            Ok((SurfaceFrame::Timeout, _)) if window.hold_after_timeout() => return Ok(false),
            Ok((SurfaceFrame::Timeout | SurfaceFrame::Occluded, _)) => (),
            Err(SurfaceError::Outdated) => gpu.resize(self.view.device),
            Err(error) => return Err(error.into()),
        }
        self.frame_retry = Some(Instant::now() + FRAME_RETRY_DELAY);
        Ok(false)
    }
}
