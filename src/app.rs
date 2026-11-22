use std::time::{Duration, Instant};

use sumie_core::{Pane, PaneCommand};
use sumie_render::{SurfaceError, SurfaceFrame};
use sumie_vt::Snapshot;
use sumie_wayland::{Event, SourceHandle};

use crate::{
    Result, capture,
    gpu::Gpu,
    input,
    pane::LocalPane,
    sources::{self, Sources},
    view::{View, clear_color},
};

const FRAME_RETRY_DELAY: Duration = Duration::from_millis(50);

pub(crate) fn run(capture: capture::Options) -> Result<()> {
    let gpu = Gpu::new(capture.screenshot.is_some())?;
    let mut app = App::new(gpu, capture);
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
    session: Option<Session>,
    events: Vec<Event>,
}

struct Session {
    pane: LocalPane,
    sources: Option<Sources>,
    view: View,
    snapshot: Option<Snapshot>,
    snapshot_dirty: bool,
    frame_dirty: bool,
    frame_retry: Option<Instant>,
}

impl App {
    fn new(gpu: Gpu, capture: capture::Options) -> Self {
        Self {
            gpu: Some(gpu),
            capture,
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
                && session.service_pty()?
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
                ..
            } => {
                let device = [width, height];
                if let Some(session) = &mut self.session {
                    session.resize(scale, device)?;
                } else {
                    self.session = Some(Session::new(scale, device, handle.clone())?);
                }
                self.gpu.as_mut().unwrap().resize(device);
            }
            event => {
                if let Some(session) = &mut self.session
                    && let Some(preedit) = input::route(&mut session.pane, event)?
                    && preedit != session.view.preedit
                {
                    session.view.preedit = preedit;
                    session.frame_dirty = true;
                }
            }
        }
        Ok(false)
    }

    fn close(&mut self) -> Result<()> {
        let close = self.session.as_mut().map_or(Ok(()), |session| {
            drop(session.sources.take());
            session.pane.command(PaneCommand::Close)
        });
        drop(self.gpu.take());
        close
    }
}

impl Session {
    fn new(scale: u32, device: [u32; 2], handle: SourceHandle) -> Result<Self> {
        let view = View::new(scale, device)?;
        let pane = LocalPane::login(view.size(), view.cell)?;
        let sources = Sources::new(handle, &pane.pty)?;
        Ok(Self {
            pane,
            sources: Some(sources),
            view,
            snapshot: None,
            snapshot_dirty: true,
            frame_dirty: true,
            frame_retry: None,
        })
    }

    fn resize(&mut self, scale: u32, device: [u32; 2]) -> Result<()> {
        self.view.resize(scale, device)?;
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

    fn service_pty(&mut self) -> Result<bool> {
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
        self.view.build(snapshot);
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
