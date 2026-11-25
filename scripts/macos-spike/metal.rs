#![forbid(unsafe_code)]

//! Visible-window Metal probe. Every scenario runs on winit's main thread, driven by numbered
//! watchdog commands on stdin; `metal.m` observes the real CAMetalLayer and Metal calls.

use std::{
    io::{self, BufRead},
    sync::Arc,
    thread,
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize, PhysicalSize},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId, WindowLevel},
};

const WINDOW_TITLE: &str = "sumie Metal spike";
const GATE_LABEL: &str = "sumie-metal-spike-gated";
const DRAWABLES: usize = 3;
/// Unthrottled presents in the FIFO scenarios; the occluded run covers the window after `COVER_AT`.
const FIFO_FRAMES: u32 = 240;
const COVER_AT: u32 = 60;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    /// GPU work pending on every pool drawable, then one more acquire.
    Held,
    /// Present as fast as acquire allows on the visible window.
    Fifo,
    /// FIFO presents, then an opaque window covers the probe window.
    Occluded,
    /// FIFO presents, then the probe window is minimized (visibility race only, not exhaustion).
    Minimized,
    /// Resize while GPU work is pending, configuring at once on the main thread.
    BusyResize,
    /// The same resize, configuring only once the queue reports no pending work.
    DeferredResize,
}

enum Command {
    Ping(u64),
    Begin,
    Quit,
}

struct Graphics {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    scratch: wgpu::Buffer,
}

impl Graphics {
    fn new(event_loop: &ActiveEventLoop) -> Self {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title(WINDOW_TITLE)
                        .with_position(LogicalPosition::new(240, 160))
                        .with_inner_size(LogicalSize::new(320, 240)),
                )
                .expect("create visible AppKit window"),
        );
        let mut options = wgpu::InstanceDescriptor::new_without_display_handle();
        options.backends = wgpu::Backends::METAL;
        options.flags.remove(wgpu::InstanceFlags::DISCARD_HAL_LABELS);
        let instance = wgpu::Instance::new(options);
        let surface = instance
            .create_surface(Arc::clone(&window))
            .expect("create wgpu Metal surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("request Metal adapter");
        assert_eq!(adapter.get_info().backend, wgpu::Backend::Metal);
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))
            .expect("request Metal device");
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width, size.height)
            .expect("configure Metal surface");
        config.desired_maximum_frame_latency = 2;
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);
        let scratch = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 4,
            usage: wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        println!("READY {} {}", size.width, size.height);
        Self {
            window,
            surface,
            device,
            queue,
            config,
            scratch,
        }
    }

    fn acquire(&self) -> Option<wgpu::SurfaceTexture> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => Some(texture),
            wgpu::CurrentSurfaceTexture::Occluded => None,
            other => panic!("drawable acquisition did not succeed: {other:?}"),
        }
    }

    /// Clear and present one drawable. A gated submission waits in `metal.m` for its own event
    /// value; wgpu records passes into internal buffers, so a labeled clear goes first.
    fn present(&self, frame: wgpu::SurfaceTexture, gated: bool) {
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: gated.then_some(GATE_LABEL),
        });
        encoder.clear_buffer(&self.scratch, 0, None);
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::GREEN),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        }));
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }

    fn queue_empty(&self) -> bool {
        self.device.poll(wgpu::PollType::Poll).unwrap().is_queue_empty()
    }

    fn begin_held(&self) {
        for _ in 0..DRAWABLES {
            self.present(self.acquire().expect("visible window"), true);
        }
        // Nothing from wgpu is outstanding here: only the pool can make this acquire wait.
        drop(self.acquire().expect("visible window"));
    }

    fn begin_resize(&self) -> PhysicalSize<u32> {
        drop(self.acquire().expect("visible window"));
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some(GATE_LABEL),
        });
        encoder.clear_buffer(&self.scratch, 0, None);
        self.queue.submit([encoder.finish()]);
        assert!(!self.queue_empty(), "gated submission completed early");
        let target = PhysicalSize::new(self.config.width + 64, self.config.height + 48);
        println!("RESIZE_REQUEST {} {}", target.width, target.height);
        let _ = self.window.request_inner_size(target);
        target
    }

    fn configure(&mut self, size: PhysicalSize<u32>) {
        assert_eq!(self.window.inner_size(), size);
        self.config.width = size.width;
        self.config.height = size.height;
        println!("CONFIGURE_ENTER {} {}", size.width, size.height);
        self.surface.configure(&self.device, &self.config);
        println!("CONFIGURE_RETURN");
    }

    fn cover(&self, event_loop: &ActiveEventLoop) -> Window {
        let position = self.window.outer_position().expect("window position");
        let size = self.window.outer_size();
        event_loop
            .create_window(
                Window::default_attributes()
                    .with_title("sumie Metal spike cover")
                    .with_decorations(false)
                    .with_window_level(WindowLevel::AlwaysOnTop)
                    .with_position(position)
                    .with_inner_size(PhysicalSize::new(size.width + 64, size.height + 64)),
            )
            .expect("create cover window")
    }
}

struct Probe {
    scenario: Scenario,
    main: thread::ThreadId,
    graphics: Option<Graphics>,
    cover: Option<Window>,
    target: Option<PhysicalSize<u32>>,
    pending: Option<PhysicalSize<u32>>,
    frames: Option<u32>,
    finished: bool,
}

impl Probe {
    fn finish(&mut self) {
        self.finished = true;
        self.frames = None;
        drop(self.cover.take());
        println!("DONE");
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(frame), Some(graphics)) = (self.frames, self.graphics.as_ref()) else {
            return;
        };
        if frame == COVER_AT && self.scenario == Scenario::Occluded && self.cover.is_none() {
            println!("COVER");
            self.cover = Some(graphics.cover(event_loop));
        }
        if frame == COVER_AT && self.scenario == Scenario::Minimized {
            println!("COVER");
            graphics.window.set_minimized(true);
        }
        match graphics.acquire() {
            Some(texture) => graphics.present(texture, false),
            None => {
                println!("WGPU_OCCLUDED {frame}");
                return self.finish();
            }
        }
        if frame + 1 == FIFO_FRAMES {
            return self.finish();
        }
        self.frames = Some(frame + 1);
        graphics.window.request_redraw();
    }

    /// The deferred-resize readiness condition: configure only with no submitted work pending.
    fn configure_when_idle(&mut self) {
        let Some(graphics) = self.graphics.as_mut() else {
            return;
        };
        if let Some(size) = self.pending
            && graphics.queue_empty()
        {
            self.pending = None;
            graphics.configure(size);
            self.finish();
        }
    }
}

impl ApplicationHandler<Command> for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.graphics.is_none() {
            self.graphics = Some(Graphics::new(event_loop));
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, command: Command) {
        assert_eq!(thread::current().id(), self.main);
        match command {
            Command::Ping(sequence) => {
                println!("PONG {sequence}");
                self.configure_when_idle();
            }
            Command::Begin => {
                let graphics = self.graphics.as_ref().unwrap();
                assert!(graphics.queue_empty());
                println!("QUEUE_IDLE");
                match self.scenario {
                    Scenario::Held => {
                        graphics.begin_held();
                        self.finish();
                    }
                    Scenario::Fifo | Scenario::Occluded | Scenario::Minimized => {
                        self.frames = Some(0);
                        graphics.window.request_redraw();
                    }
                    Scenario::BusyResize | Scenario::DeferredResize => {
                        self.target = Some(graphics.begin_resize());
                    }
                }
            }
            Command::Quit => {
                assert!(self.finished, "quit before the probe finished");
                self.graphics = None;
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.graphics.as_ref().is_none_or(|graphics| graphics.window.id() != id) {
            return;
        }
        match event {
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            WindowEvent::Resized(size) if self.target == Some(size) => {
                assert_eq!(thread::current().id(), self.main);
                self.target = None;
                if self.scenario == Scenario::DeferredResize {
                    println!("CONFIGURE_DEFERRED {} {}", size.width, size.height);
                    self.pending = Some(size);
                    self.configure_when_idle();
                } else {
                    self.graphics.as_mut().unwrap().configure(size);
                    self.finish();
                }
            }
            _ => (),
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        self.configure_when_idle();
    }
}

fn main() {
    let argument = std::env::args().nth(1).expect("scenario argument");
    let scenario = match argument.as_str() {
        "held" => Scenario::Held,
        "fifo" => Scenario::Fifo,
        "occluded" => Scenario::Occluded,
        "minimized" => Scenario::Minimized,
        "busy-resize" => Scenario::BusyResize,
        "deferred-resize" => Scenario::DeferredResize,
        _ => panic!("unknown scenario: {argument}"),
    };
    let event_loop = EventLoop::<Command>::with_user_event().build().unwrap();
    let proxy = event_loop.create_proxy();
    thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut line = String::new();
        while stdin.read_line(&mut line).expect("read watchdog input") != 0 {
            let command = match line.trim_end() {
                "BEGIN" => Command::Begin,
                "QUIT" => Command::Quit,
                ping => Command::Ping(ping.strip_prefix("PING ").unwrap().parse().unwrap()),
            };
            let quit = matches!(command, Command::Quit);
            if proxy.send_event(command).is_err() || quit {
                break;
            }
            line.clear();
        }
    });
    let mut probe = Probe {
        scenario,
        main: thread::current().id(),
        graphics: None,
        cover: None,
        target: None,
        pending: None,
        frames: None,
        finished: false,
    };
    event_loop.run_app(&mut probe).unwrap();
    assert!(probe.finished);
}
