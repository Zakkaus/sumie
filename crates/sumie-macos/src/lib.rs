#![forbid(unsafe_code)]
#![cfg(target_os = "macos")]

//! Owns macOS windows, input, input methods, and clipboard access. Does not
//! match shortcuts, make business decisions, or block the main loop on reads
//! from the clipboard.

use std::{error::Error, ops::ControlFlow};

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{EventLoop, EventLoopProxy},
    window::WindowId,
};

pub use winit::event_loop::ActiveEventLoop as WindowContext;

use readiness::{Bridge, Readiness};
pub use window::{Platform, Pointer, double_click_interval};

pub mod exit;
pub mod readiness;
#[cfg(feature = "macos-ci")]
pub mod runner;
mod window;

pub enum Event {
    Resumed,
    /// The backing size or scale factor changed; read both from the window.
    Resized,
    Focused(bool),
    Occluded(bool),
    /// The window moved, possibly onto another screen.
    Moved,
    RedrawRequested,
    CloseRequested,
    /// The run loop is about to sleep, in any mode: the end of one batch of events.
    AboutToWait,
    /// The readiness bridge has reports; take them with [`Readiness::take`].
    Ready,
    /// The text a key press produced. Physical keys are not reported yet.
    KeyText(String),
    PointerEntered(Pointer),
    Pointer(Pointer),
    #[cfg(feature = "macos-ci")]
    RunnerCommand(runner::Command),
    #[cfg(feature = "macos-ci")]
    RunnerKey(runner::Key),
}

type Result<T, E = Box<dyn Error>> = std::result::Result<T, E>;

/// Dispatch on the main thread. Return `Break` from the callback to exit.
///
/// # Errors
/// Propagates window creation, event loop, bridge and application errors.
pub fn run(callback: impl Callback) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let bridge = Bridge::spawn(waker(event_loop.create_proxy(), ()))?;
    let mut handler = Handler::new(callback, bridge);
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
    input: window::Input,
    #[cfg(feature = "macos-ci")]
    forward_runner_keys: bool,
}

impl<F: Callback> Handler<F> {
    fn new(callback: F, bridge: Bridge) -> Self {
        Self {
            callback,
            bridge,
            result: Ok(()),
            input: window::Input::default(),
            #[cfg(feature = "macos-ci")]
            forward_runner_keys: false,
        }
    }

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
        #[cfg(feature = "macos-ci")]
        if let WindowEvent::KeyboardInput {
            event,
            is_synthetic: false,
            ..
        } = &event
            && self.forward_runner_keys
            && let Some(key) = runner::Key::from_event(event)
        {
            self.emit(context, Event::RunnerKey(key));
            return;
        }
        if let Some(event) = self.input.convert(event) {
            self.emit(context, event);
        }
    }

    fn about_to_wait(&mut self, context: &WindowContext) {
        self.emit(context, Event::AboutToWait);
    }
}
