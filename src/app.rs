use std::time::{Duration, Instant};

use sumie_core::{
    Pane, PaneCommand, PaneEffect, Selection, Shortcuts,
    mouse::{MouseEvent, SelectStep},
};
use sumie_render::{SurfaceError, SurfaceFrame};
use sumie_vt::Event as TerminalEvent;
use sumie_vt::Snapshot;
use sumie_wayland::{Event, SourceHandle};

use crate::{
    Result, capture,
    gpu::Gpu,
    input,
    pane::LocalPane,
    sources::{self, Sources},
    view::{FontSettings, View, clear_color},
};

const FRAME_RETRY_DELAY: Duration = Duration::from_millis(50);

pub(crate) fn run(capture: capture::Options, font: FontSettings) -> Result<()> {
    let gpu = Gpu::new(capture.screenshot.is_some())?;
    let mut app = App::new(gpu, capture, font);
    let result = app.drive();
    let close = app.close();
    let reap = app
        .session
        .as_mut()
        .map_or(Ok(()), |session| sources::reap(&mut session.pane.pty));
    result.and(close).and(reap)
}

struct App {
    gpu: Option<Gpu>,
    capture: capture::Options,
    font: Option<FontSettings>,
    session: Option<Session>,
    events: Vec<Event>,
}

struct Session {
    shortcuts: Shortcuts,
    selection: Selection,
    pane: LocalPane,
    sources: Option<Sources>,
    view: View,
    snapshot: Option<Snapshot>,
    snapshot_dirty: bool,
    frame_dirty: bool,
    frame_retry: Option<Instant>,
}

impl App {
    fn new(gpu: Gpu, capture: capture::Options, font: FontSettings) -> Self {
        Self {
            gpu: Some(gpu),
            capture,
            font: Some(font),
            session: None,
            events: Vec::new(),
        }
    }

    fn drive(&mut self) -> Result<()> {
        let handle = self.gpu.as_ref().unwrap().loop_handle();
        loop {
            let retry = self
                .session
                .as_ref()
                .and_then(|session| session.frame_retry);
            let timeout = retry.map(|at| at.saturating_duration_since(Instant::now()));
            self.gpu
                .as_mut()
                .unwrap()
                .dispatch(&mut self.events, timeout)?;
            if let Some(session) = &mut self.session
                && session.service_pty(self.gpu.as_mut().unwrap())?
            {
                return Ok(());
            }
            let mut events = std::mem::take(&mut self.events);
            for event in events.drain(..) {
                if self.handle_event(event, &handle)? {
                    return Ok(());
                }
            }
            self.events = events;
            if let Some(session) = &mut self.session {
                session.watch_input()?;
                let gpu = self.gpu.as_mut().unwrap();
                if gpu.frame_slot_available()
                    && retry.is_none_or(|at| Instant::now() >= at)
                    && session.draw(gpu, &self.capture)?
                {
                    return Ok(());
                }
            }
        }
    }

    fn handle_event(&mut self, event: Event, handle: &SourceHandle) -> Result<bool> {
        match event {
            Event::CloseRequested => return Ok(true),
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
                    let font = self
                        .font
                        .take()
                        .expect("font settings before the first resize");
                    self.session =
                        Some(Session::new(scale, logical, device, handle.clone(), font)?);
                }
                self.gpu.as_mut().unwrap().resize(device);
            }
            event => {
                if let Some(session) = &mut self.session {
                    session.input(event, self.gpu.as_mut().unwrap())?;
                }
            }
        }
        Ok(false)
    }

    fn close(&mut self) -> Result<()> {
        let close = self.session.as_mut().map_or(Ok(()), |session| {
            drop(session.sources.take());
            session.pane.command(PaneCommand::Close).map(|_| ())
        });
        drop(self.gpu.take());
        close
    }
}

impl Session {
    fn new(
        scale: u32,
        logical: [u32; 2],
        device: [u32; 2],
        handle: SourceHandle,
        font: FontSettings,
    ) -> Result<Self> {
        let view = View::new(scale, logical, device, font)?;
        let pane = LocalPane::login(view.size(), view.cell)?;
        let sources = Sources::new(handle, &pane.pty)?;
        Ok(Self {
            shortcuts: Shortcuts::default(),
            selection: Selection::default(),
            pane,
            sources: Some(sources),
            view,
            snapshot: None,
            snapshot_dirty: true,
            frame_dirty: true,
            frame_retry: None,
        })
    }

    fn resize(&mut self, scale: u32, logical: [u32; 2], device: [u32; 2]) -> Result<()> {
        self.view.resize(scale, logical, device)?;
        self.selection = Selection::default();
        let size = self.view.size();
        self.pane.command(PaneCommand::Resize {
            cols: size.cols,
            rows: size.rows,
            px: [size.width_px, size.height_px],
            cell_px: self.view.cell,
        })?;
        self.snapshot_dirty = true;
        self.frame_dirty = true;
        Ok(())
    }

    fn input(&mut self, event: Event, gpu: &mut Gpu) -> Result<()> {
        let effect = if let Event::Pointer {
            phase,
            position,
            serial,
            modifiers,
        } = event
        {
            let (cell, pixel) = self.view.pointer_position(position);
            let mouse = MouseEvent {
                phase: input::mouse_phase(phase),
                cell,
                pixel,
                modifiers: input::key_modifiers(modifiers),
            };
            match input::send(&mut self.pane, PaneCommand::Mouse(mouse))? {
                PaneEffect::ViewportChanged => input::Effect::ViewportChanged,
                PaneEffect::Select(step) if self.select(step, cell)? => input::Effect::Copy(serial),
                _ => return Ok(()),
            }
        } else {
            input::route(&mut self.pane, &mut self.shortcuts, event)?
        };
        match effect {
            input::Effect::Copy(serial) => {
                self.update_snapshot()?;
                if let Some(range) = self.selection.range()
                    && let PaneEffect::Copied(text) = self.pane.command(PaneCommand::Copy(range))?
                {
                    gpu.write_clipboard(serial, text);
                }
            }
            input::Effect::ViewportChanged => {
                self.frame_dirty |= self.selection.range().is_some();
                self.selection = Selection::default();
                self.snapshot_dirty = true;
            }
            input::Effect::ReadClipboard => gpu.read_clipboard(),
            input::Effect::Preedit(text) if text != self.view.preedit => {
                self.view.preedit = text;
                self.frame_dirty = true;
            }
            _ => (),
        }
        Ok(())
    }

    /// Returns true only for the release that completes a drag.
    fn select(&mut self, step: SelectStep, hit: sumie_core::CellPosition) -> Result<bool> {
        self.update_snapshot()?;
        let snapshot = self.snapshot.as_ref().expect("current pointer snapshot");
        let width = snapshot.rows[usize::from(hit.row)][usize::from(hit.column)].width;
        let old = self.selection.range();
        match step {
            SelectStep::Begin => self.selection.press(hit, width),
            SelectStep::Drag | SelectStep::Finish => self.selection.drag(hit, width),
            SelectStep::Clear => self.selection = Selection::default(),
        }
        self.frame_dirty |= old != self.selection.range();
        Ok(step == SelectStep::Finish && self.selection.release().is_some())
    }

    fn service_pty(&mut self, gpu: &mut Gpu) -> Result<bool> {
        let sources = self.sources.as_mut().unwrap();
        let ready = sources.take_ready();
        if ready.exit() {
            self.pane.pty.service_child()?;
            if self.pane.pty.exit_fd().is_none() {
                return Ok(true);
            }
        }
        if ready.output() {
            let (dirty, eof) = self.pane.drain_output()?;
            self.snapshot_dirty |= dirty;
            for event in self.pane.terminal.drain_events() {
                match event {
                    TerminalEvent::Clipboard(bytes) => gpu.write_terminal_clipboard(bytes),
                    TerminalEvent::Title(title) => {
                        gpu.queue_title(title);
                        self.frame_dirty = true;
                    }
                }
            }
            if eof {
                sources.remove_output();
            }
        }
        Ok(ready.input() && !self.pane.pty.flush_input()?)
    }

    fn watch_input(&mut self) -> Result<()> {
        self.sources
            .as_mut()
            .unwrap()
            .watch_input(self.pane.pty.input_fd())
    }

    fn update_snapshot(&mut self) -> Result<()> {
        if self.snapshot_dirty {
            self.snapshot_dirty = false;
            let next = self.pane.snapshot()?;
            if self.snapshot.as_ref() != Some(&next) {
                if let Some(rows) = self.selection.affected_rows()
                    && self
                        .snapshot
                        .as_ref()
                        .is_some_and(|prev| prev.rows[rows.clone()] != next.rows[rows])
                {
                    self.selection = Selection::default();
                }
                self.snapshot = Some(next);
                self.frame_dirty = true;
            }
        }
        Ok(())
    }

    fn draw(&mut self, gpu: &mut Gpu, options: &capture::Options) -> Result<bool> {
        self.frame_retry = None;
        self.update_snapshot()?;
        if !self.frame_dirty {
            return Ok(false);
        }
        let snapshot = self.snapshot.as_ref().expect("initial snapshot");
        self.view.build(snapshot, self.selection.range());
        gpu.upload_atlas(&self.view.atlas());
        self.view.atlas_uploaded();
        let final_frame = options
            .frames
            .is_some_and(|count| count == gpu.present_count() + 1);
        let frame = gpu.draw(
            &self.view.draw_list(),
            clear_color(snapshot.background),
            final_frame && options.screenshot.is_some(),
        );
        if let Some(cursor) = snapshot.cursor {
            gpu.set_cursor_rectangle(self.view.ime_rectangle(cursor));
        }
        match frame {
            Ok((SurfaceFrame::Presented { suboptimal }, readback)) => {
                self.frame_dirty = false;
                tracing::debug!(count = gpu.present_count(), "presented terminal frame");
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
