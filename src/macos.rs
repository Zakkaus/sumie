//! Runs the Metal test page and applies watched settings on the window thread.

use std::{
    ops::ControlFlow,
    time::{Duration, Instant},
};

use sumie_config::Config;
use sumie_macos::{
    Event, Platform,
    readiness::{Key, Readiness},
};
use sumie_render::{SurfaceError, SurfaceFrame, SurfaceRenderer, instance_descriptor, wgpu};

use crate::{Result, capture, frame_window::FrameWindow};

pub(crate) mod login;
mod page;
mod reload;
#[cfg(feature = "macos-ci")]
mod runner;
mod sources;
use page::Page;
pub(crate) use reload::{CHILD_ARG as CONFIG_WATCH_ARG, watch_child as run_config_watch};
use reload::{Reload, Update};
use sources::{Timers, Watches};

use crate::platform::TimerSource as _;

const BLINK: Duration = Duration::from_millis(600);

/// The test page's window size in points.
const SMOKE_SIZE: [u32; 2] = [480, 180];

impl FrameWindow for Platform {
    fn frame_slot_available(&self) -> bool {
        !self.device_size().contains(&0)
    }

    fn arm_frame_callback(&mut self) {
        self.before_present();
    }
}

pub(crate) fn run(options: &capture::Options) -> Result<()> {
    #[cfg(feature = "macos-ci")]
    if let Some(mode) = runner::Mode::from_env()? {
        return runner::run(mode, options);
    }

    // The terminal driver starts panes with this login; the test page only proves it.
    let _login = login::prepare()?;
    let mut state = None;
    sumie_macos::run(|context, readiness, event| {
        match (&mut state, event) {
            (None, Event::Resumed) => {
                let window = Platform::create(context, "sumie", SMOKE_SIZE)?;
                state = Some(Smoke::new(window, readiness, options.screenshot.is_some())?);
            }
            (Some(app), Event::Ready) => return app.ready(),
            (Some(app), Event::Resized | Event::Occluded(false)) => app.window.request_redraw(),
            (Some(app), Event::RedrawRequested) => {
                return app.draw(options);
            }
            (Some(app), Event::CloseRequested) => return app.close(),
            (None, Event::CloseRequested) => return Ok(ControlFlow::Break(())),
            _ => (),
        }
        Ok(ControlFlow::Continue(()))
    })?;
    if let Some(count) = options.frames {
        let presented = state.map_or(0, |state| state.surface.present_count());
        if presented != count {
            return Err(format!("window closed after {presented} of {count} presentations").into());
        }
    }
    Ok(())
}

struct Smoke {
    surface: SurfaceRenderer<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    size: [u32; 2],
    watches: Watches,
    reload: Reload,
    config: Config,
    /// Set when settings or the banner changed since the last composition.
    compose: bool,
    closing: bool,
    timers: Timers,
    /// The next blink phase and its timer.
    blink: Option<(Instant, Key)>,
    cursor_hidden: bool,
    scale: f64,
    /// Built from the child's first result; nothing presents before it.
    page: Option<Page>,
    window: Platform,
}

impl Smoke {
    fn new(window: Platform, readiness: &Readiness, readback: bool) -> Result<Self> {
        let instance = wgpu::Instance::new(instance_descriptor(
            wgpu::InstanceDescriptor::new_without_display_handle(),
            wgpu::Backends::METAL,
        ));
        let surface = instance.create_surface(window.window_owner())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                required_limits:
                    wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            }))?;
        let mut surface = SurfaceRenderer::new(surface, &adapter, &device, &queue, [0, 0])?;
        if readback {
            surface.enable_readback(&adapter)?;
        }
        let size = window.device_size();
        surface.resize(size);
        let mut watches = Watches::new(readiness.clone());
        let reload = Reload::spawn(reload::command()?, &mut watches)?;
        Ok(Self {
            surface,
            device,
            queue,
            size,
            scale: window.scale(),
            timers: watches.animation_timer(),
            watches,
            reload,
            config: Config::default(),
            compose: false,
            closing: false,
            blink: None,
            cursor_hidden: false,
            page: None,
            window,
        })
    }

    /// Handle `Event::Ready`: settings results, the watcher's exit, and the blink phase.
    fn ready(&mut self) -> Result<ControlFlow<()>> {
        self.watches.dispatch()?;
        for update in self.reload.service(&mut self.watches)? {
            self.compose |= self.apply(update)?;
        }
        if self.closing {
            return Ok(if self.reload.reaped() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            });
        }
        if self.compose | self.blink()? {
            self.window.request_redraw();
        }
        Ok(ControlFlow::Continue(()))
    }

    /// Cancel the watcher and exit once it is reaped; `ready` drives the reap.
    fn close(&mut self) -> Result<ControlFlow<()>> {
        self.closing = true;
        self.reload.close(&mut self.watches)?;
        self.ready()
    }

    fn apply(&mut self, update: Update) -> Result<bool> {
        let (next, repaired) = match update {
            Update::Settings(next) => (next, self.reload.banner.take().is_some()),
            Update::Diagnostic(message) => {
                let shown = self.reload.show_error(message);
                if self.page.is_some() {
                    return Ok(shown);
                }
                // The first result failed: start from the defaults under the banner.
                (Config::default(), true)
            }
        };
        Ok(self.apply_settings(next, self.window.scale())? | repaired)
    }

    fn apply_settings(&mut self, mut next: Config, scale: f64) -> Result<bool> {
        let first = self.page.is_none();
        if first || self.config.changes(&next).font {
            match Page::new(&next.font, scale) {
                Ok(page) => self.page = Some(page),
                Err(error) => {
                    let shown = self.reload.show_error(format!("sumie.toml:1:1: {error}"));
                    if !first {
                        return Ok(shown);
                    }
                    next = Config::default();
                    self.page = Some(Page::new(&next.font, scale)?);
                }
            }
            self.scale = scale;
        }
        let changes = self.config.changes(&next);
        if first || changes.cursor {
            self.set_blink(next.cursor.blink())?;
        }
        if first || changes.minimum_contrast {
            self.surface.set_minimum_contrast(next.minimum_contrast());
        }
        self.config = next;
        Ok(first || changes.font || changes.terminal_colors || changes.cursor)
    }

    fn set_blink(&mut self, enabled: bool) -> Result<()> {
        if let Some((_, key)) = self.blink.take() {
            self.timers.cancel(key);
        }
        self.cursor_hidden = false;
        if enabled {
            let at = Instant::now() + BLINK;
            self.blink = Some((at, self.timers.register(at)?));
        }
        Ok(())
    }

    /// Advance a due blink phase; returns whether the drawn cursor changed.
    fn blink(&mut self) -> Result<bool> {
        if self.blink.is_none_or(|(at, _)| at > Instant::now()) {
            return Ok(false);
        }
        let at = Instant::now() + BLINK;
        self.blink = Some((at, self.timers.register(at)?));
        let hidden = !self.cursor_hidden && self.window.window_owner().has_focus();
        Ok(std::mem::replace(&mut self.cursor_hidden, hidden) != hidden)
    }

    fn draw(&mut self, options: &capture::Options) -> Result<ControlFlow<()>> {
        self.draw_with(options, None, || {})
    }

    fn draw_with(
        &mut self,
        options: &capture::Options,
        clear: Option<wgpu::Color>,
        presented: impl FnOnce(),
    ) -> Result<ControlFlow<()>> {
        let size = self.window.device_size();
        let scale = self.window.scale();
        let Some(page) = &mut self.page else {
            return Ok(ControlFlow::Continue(()));
        };
        let mut dirty = std::mem::take(&mut self.compose);
        if self.size != size {
            self.surface.resize(size);
            dirty |= self.size[0] != size[0] && self.reload.banner.is_some();
            self.size = size;
        }
        if self.scale.to_bits() != scale.to_bits() {
            *page = Page::new(&self.config.font, scale)?;
            self.scale = scale;
            dirty = true;
        }
        if dirty {
            page.compose(
                &self.config,
                self.reload.banner.as_deref(),
                size[0],
                &mut self.surface,
            )?;
        }
        if !self.window.frame_slot_available() {
            return Ok(ControlFlow::Continue(()));
        }
        let show_cursor = !self.cursor_hidden || !self.window.window_owner().has_focus();
        let window = &mut self.window;
        let last = options.frames == Some(self.surface.present_count() + 1);
        let path = options.screenshot.as_deref().filter(|_| last);
        let mut capture = None;
        let frame = self.surface.draw_with(
            &page.draw_list(show_cursor),
            clear.unwrap_or(page.clear),
            |frame| {
                if path.is_some() {
                    capture = Some(capture::Readback::copy(
                        &self.device,
                        &self.queue,
                        frame,
                        false,
                    ));
                }
                window.arm_frame_callback();
            },
        );
        match frame {
            Ok(SurfaceFrame::Presented { suboptimal }) => {
                presented();
                if let Some(path) = path {
                    capture
                        .ok_or("presented without capturing the frame")??
                        .write(&self.device, path)?;
                }
                if last {
                    return Ok(ControlFlow::Break(()));
                }
                if suboptimal {
                    self.surface.resize(size);
                }
                if options.frames.is_some() || suboptimal {
                    window.request_redraw();
                }
            }
            Ok(SurfaceFrame::Timeout) => window.request_redraw(),
            Err(SurfaceError::Outdated) => {
                self.surface.resize(size);
                window.request_redraw();
            }
            Ok(SurfaceFrame::Suspended | SurfaceFrame::Occluded) => (),
            Err(error) => return Err(error.into()),
        }
        Ok(ControlFlow::Continue(()))
    }
}
