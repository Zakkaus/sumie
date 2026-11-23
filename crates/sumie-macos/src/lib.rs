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
    event_loop::EventLoop,
    window::{Window, WindowId},
};

pub use winit::event_loop::ActiveEventLoop as WindowContext;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Resumed,
    NeedsRedraw,
    RedrawRequested,
    CloseRequested,
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
/// Propagates window creation, event loop and application errors.
pub fn run(callback: impl FnMut(&WindowContext, Event) -> Result<ControlFlow<()>>) -> Result<()> {
    let mut handler = Handler {
        callback,
        result: Ok(()),
    };
    EventLoop::new()?.run_app(&mut handler)?;
    handler.result
}

struct Handler<F> {
    callback: F,
    result: Result<()>,
}

impl<F: FnMut(&WindowContext, Event) -> Result<ControlFlow<()>>> Handler<F> {
    fn emit(&mut self, context: &WindowContext, event: Event) {
        if context.exiting() {
            return;
        }
        match (self.callback)(context, event) {
            Ok(ControlFlow::Continue(())) => (),
            Ok(ControlFlow::Break(())) => context.exit(),
            Err(error) => {
                self.result = Err(error);
                context.exit();
            }
        }
    }
}

impl<F: FnMut(&WindowContext, Event) -> Result<ControlFlow<()>>> ApplicationHandler for Handler<F> {
    fn resumed(&mut self, context: &WindowContext) {
        self.emit(context, Event::Resumed);
    }

    fn window_event(&mut self, context: &WindowContext, _: WindowId, event: WindowEvent) {
        let event = match event {
            WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. }
            | WindowEvent::Occluded(false) => Event::NeedsRedraw,
            WindowEvent::RedrawRequested => Event::RedrawRequested,
            WindowEvent::CloseRequested => Event::CloseRequested,
            _ => return,
        };
        self.emit(context, event);
    }
}
