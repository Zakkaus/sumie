#![forbid(unsafe_code)]
#![cfg(target_os = "macos")]

//! Owns macOS windows, input, input methods, and clipboard access. Does not
//! match shortcuts, make business decisions, or block the main loop on reads
//! from the clipboard.

use std::{error::Error, ops::ControlFlow, sync::Arc};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

pub use winit::event_loop::ActiveEventLoop as WindowContext;

use readiness::{Bridge, Readiness};

pub mod exit;
pub mod readiness;
#[cfg(feature = "macos-ci")]
pub mod runner;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Resumed,
    NeedsRedraw,
    RedrawRequested,
    CloseRequested,
    /// The readiness bridge has reports; take them with [`Readiness::take`].
    Ready,
    #[cfg(feature = "macos-ci")]
    RunnerCommand(runner::Command),
    #[cfg(feature = "macos-ci")]
    RunnerKey(runner::Key),
}

/// Window access from the winit main-thread callback. wgpu retains its own owner.
pub struct Platform(Arc<Window>);

impl Platform {
    /// Create the window during the event loop's resume callback.
    ///
    /// # Errors
    /// Returns the window server's creation error.
    pub fn create(context: &WindowContext) -> Result<Self> {
        let attributes = Window::default_attributes()
            .with_title("sumie")
            .with_inner_size(LogicalSize::new(480, 180));
        Ok(Self(Arc::new(context.create_window(attributes)?)))
    }

    #[must_use]
    pub fn window_owner(&self) -> Arc<Window> {
        self.0.clone()
    }

    #[must_use]
    pub fn device_size(&self) -> [u32; 2] {
        self.0.inner_size().into()
    }

    #[must_use]
    pub fn scale(&self) -> f64 {
        self.0.scale_factor()
    }

    pub fn request_redraw(&self) {
        self.0.request_redraw();
    }

    pub fn before_present(&self) {
        self.0.pre_present_notify();
    }
}

type Result<T, E = Box<dyn Error>> = std::result::Result<T, E>;

/// Dispatch on the main thread. Return `Break` from the callback to exit.
///
/// # Errors
/// Propagates window creation, event loop, bridge and application errors.
pub fn run(callback: impl Callback) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let bridge = Bridge::spawn(waker(event_loop.create_proxy(), ()))?;
    let mut handler = Handler {
        callback,
        bridge,
        result: Ok(()),
        #[cfg(feature = "macos-ci")]
        forward_runner_keys: false,
    };
    event_loop.run_app(&mut handler)?;
    handler.result
}

/// The application callback: registrations go through the [`Readiness`] handle.
pub trait Callback: FnMut(&WindowContext, &Readiness, Event) -> Result<ControlFlow<()>> {}

impl<F: FnMut(&WindowContext, &Readiness, Event) -> Result<ControlFlow<()>>> Callback for F {}

/// The bridge's wake: one proxy event, whose run-loop source winit adds in common modes.
fn waker<T: Clone + Send + 'static>(proxy: EventLoopProxy<T>, event: T) -> impl Fn() + Send {
    move || {
        let _ = proxy.send_event(event.clone());
    }
}

struct Handler<F> {
    callback: F,
    bridge: Bridge,
    result: Result<()>,
    #[cfg(feature = "macos-ci")]
    forward_runner_keys: bool,
}

impl<F: Callback> Handler<F> {
    fn emit(&mut self, context: &WindowContext, event: Event) {
        if context.exiting() {
            return;
        }
        match (self.callback)(context, self.bridge.readiness(), event) {
            Ok(ControlFlow::Continue(())) => (),
            Ok(ControlFlow::Break(())) => context.exit(),
            Err(error) => {
                self.result = Err(error);
                context.exit();
            }
        }
    }
}

impl<F: Callback> ApplicationHandler for Handler<F> {
    fn resumed(&mut self, context: &WindowContext) {
        self.emit(context, Event::Resumed);
    }

    fn user_event(&mut self, context: &WindowContext, (): ()) {
        if self.bridge.readiness().accept_wake() {
            self.emit(context, Event::Ready);
        }
    }

    fn window_event(&mut self, context: &WindowContext, _: WindowId, event: WindowEvent) {
        let event = match event {
            WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. }
            | WindowEvent::Occluded(false) => Event::NeedsRedraw,
            WindowEvent::RedrawRequested => Event::RedrawRequested,
            WindowEvent::CloseRequested => Event::CloseRequested,
            #[cfg(feature = "macos-ci")]
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if self.forward_runner_keys => {
                let Some(key) = runner::Key::from_event(&event) else {
                    return;
                };
                Event::RunnerKey(key)
            }
            _ => return,
        };
        self.emit(context, event);
    }
}
