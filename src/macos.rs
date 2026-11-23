use std::ops::ControlFlow;

use sumie_macos::{Event, Platform};
use sumie_render::{SurfaceError, SurfaceFrame, SurfaceRenderer, instance_descriptor, wgpu};

use crate::{Result, capture, frame_window::FrameWindow};

mod page;
use page::Page;

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
    let mut state = None;
    sumie_macos::run(|context, event| {
        match (&mut state, event) {
            (None, Event::Resumed) => {
                let window = Platform::open(context)?;
                window.request_redraw();
                state = Some(Smoke::new(window, options.screenshot.is_some())?);
            }
            (Some(app), Event::NeedsRedraw) => app.window.request_redraw(),
            (Some(app), Event::RedrawRequested) if app.window.frame_slot_available() => {
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
        let page = Page::new(scale, &mut surface)?;
        Ok(Self {
            surface,
            device,
            queue,
            size,
            scale,
            page,
            window,
        })
    }

    fn draw(&mut self, options: &capture::Options) -> Result<ControlFlow<()>> {
        let window = &mut self.window;
        let size = window.device_size();
        if self.size != size {
            self.surface.resize(size);
            self.size = size;
        }
        let scale = window.scale();
        if self.scale.to_bits() != scale.to_bits() {
            self.page = Page::new(scale, &mut self.surface)?;
            self.scale = scale;
        }
        let last = options.frames == Some(self.surface.present_count() + 1);
        let path = options.screenshot.as_deref().filter(|_| last);
        let mut capture = None;
        let frame = self
            .surface
            .draw_with(&self.page.draw_list(), wgpu::Color::BLACK, |frame| {
                if path.is_some() {
                    capture = Some(capture::Readback::copy(&self.device, &self.queue, frame));
                }
                window.arm_frame_callback();
            });
        match frame {
            Ok(SurfaceFrame::Presented { suboptimal }) => {
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
