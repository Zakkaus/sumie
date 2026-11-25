//! Runs the Metal test page and applies watched settings on the window thread.

use std::ops::ControlFlow;

use sumie_config::Config;
use sumie_macos::{Event, Platform};
use sumie_render::{SurfaceError, SurfaceFrame, SurfaceRenderer, instance_descriptor, wgpu};

use crate::{Result, capture, frame_window::FrameWindow};

mod page;
mod reload;
#[cfg(feature = "macos-ci")]
mod runner;
use page::Page;
use reload::{Reload, Update};

impl FrameWindow for Platform {
    type Context = sumie_macos::WindowContext;

    fn open(context: &Self::Context) -> Result<Self> {
        Platform::create(context)
    }

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

    let mut state = None;
    sumie_macos::run(|context, event| {
        match (&mut state, event) {
            (None, Event::Resumed) => {
                let window = Platform::open(context)?;
                window.request_redraw();
                state = Some(Smoke::new(window, options.screenshot.is_some())?);
            }
            (Some(app), Event::NeedsRedraw) => app.window.request_redraw(),
            (Some(app), Event::RedrawRequested) => {
                return app.draw(options);
            }
            (_, Event::CloseRequested) => return Ok(ControlFlow::Break(())),
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
    reload: Reload,
    config: Config,
    cursor_hidden: bool,
    scale: f64,
    page: Page,
    window: Platform,
}

impl Smoke {
    fn new(window: Platform, readback: bool) -> Result<Self> {
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
        let scale = window.scale();
        let (mut reload, mut config) = Reload::new(&window)?;
        let mut page = match Page::new(&config.font, scale) {
            Ok(page) => page,
            Err(error) => {
                reload.show_error(format!("sumie.toml:1:1: {error}"));
                config = Config::default();
                reload.set_blink(config.cursor.blink())?;
                Page::new(&config.font, scale)?
            }
        };
        page.compose(&config, reload.banner.as_deref(), size[0], &mut surface)?;
        surface.set_minimum_contrast(config.minimum_contrast());
        Ok(Self {
            surface,
            device,
            queue,
            size,
            scale,
            reload,
            config,
            cursor_hidden: false,
            page,
            window,
        })
    }

    fn apply_reload(&mut self, scale: f64) -> Result<bool> {
        let mut dirty = false;
        while let Some(update) = self.reload.next() {
            match update {
                Update::Settings(Ok(next)) => dirty |= self.apply_settings(next, scale)?,
                Update::Settings(Err(error)) => dirty |= self.reload.load_error(&error),
                Update::Blink => {
                    self.cursor_hidden = !self.cursor_hidden
                        && self.config.cursor.blink()
                        && self.window.window_owner().has_focus();
                }
                Update::Failed(error) => return Err(error.into()),
            }
        }
        Ok(dirty)
    }

    fn apply_settings(&mut self, next: Config, scale: f64) -> Result<bool> {
        let changes = self.config.changes(&next);
        if changes.font {
            match Page::new(&next.font, scale) {
                Ok(page) => {
                    self.page = page;
                    self.scale = scale;
                }
                Err(error) => {
                    return Ok(self.reload.show_error(format!("sumie.toml:1:1: {error}")));
                }
            }
        }
        if changes.cursor {
            self.cursor_hidden = false;
            self.reload.set_blink(next.cursor.blink())?;
        }
        if changes.minimum_contrast {
            self.surface.set_minimum_contrast(next.minimum_contrast());
        }
        self.config = next;
        let repaired = self.reload.banner.take().is_some();
        Ok(changes.font || changes.terminal_colors || changes.cursor || repaired)
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
        let mut dirty = self.apply_reload(scale)?;
        if self.size != size {
            self.surface.resize(size);
            dirty |= self.size[0] != size[0] && self.reload.banner.is_some();
            self.size = size;
        }
        if self.scale.to_bits() != scale.to_bits() {
            self.page = Page::new(&self.config.font, scale)?;
            self.scale = scale;
            dirty = true;
        }
        if dirty {
            self.page.compose(
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
            &self.page.draw_list(show_cursor),
            clear.unwrap_or(self.page.clear),
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
