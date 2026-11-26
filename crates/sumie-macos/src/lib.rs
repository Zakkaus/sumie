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
    platform::macos::{ActiveEventLoopExtMacOS as _, QuitReason},
    window::WindowId,
};

pub use winit::event_loop::ActiveEventLoop as WindowContext;

pub use keyboard::{Key, Symbol};
use readiness::{Bridge, Readiness};
use termination::Quit;
pub use window::{Platform, Pointer, double_click_interval};

pub mod exit;
mod keyboard;
pub mod pasteboard;
pub mod readiness;
#[cfg(feature = "macos-ci")]
pub mod runner;
pub mod termination;
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
    /// `AppKit` asked to quit: drain, then return `Break`. A repeat arrives only to upgrade an
    /// ordinary Quit to a system one.
    Quit(Quit),
    Key(Key),
    /// The keyboard modifiers changed; winit reports empty modifiers when the window loses
    /// focus with any held.
    ModifiersChanged(sumie_core::KeyModifiers),
    PointerEntered(Pointer),
    Pointer(Pointer),
    #[cfg(feature = "macos-ci")]
    RunnerCommand(runner::Command),
    #[cfg(feature = "macos-ci")]
    RunnerKey(runner::Key),
}

type Result<T, E = Box<dyn Error>> = std::result::Result<T, E>;

/// Dispatch on the main thread. Return `Break` from the callback to exit: through `AppKit` once
/// an [`Event::Quit`] arrived, so the intercepted Quit proceeds, else by ending the loop.
///
/// An error ends the loop without `terminate:`, and `run` returns it, unless a system quit
/// waits for its reply: then the error is logged and the reply lets the process exit, since
/// log out, restart and shut down are never refused.
///
/// # Errors
/// Propagates window creation, event loop, bridge and application errors.
pub fn run(callback: impl Callback) -> Result<()> {
    let event_loop = EventLoop::<Wake>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let bridge = Bridge::spawn(waker(proxy.clone(), Wake::Ready))?;
    let mut handler = Handler::new(callback, bridge, intercept(proxy, Wake::Quit));
    event_loop.run_app(&mut handler)?;
    handler.result
}

#[derive(Clone)]
enum Wake {
    Ready,
    Quit(Quit),
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

/// Installs the Quit hook once the loop runs; requests arrive as the loop's own user event.
type Intercept = Box<dyn FnOnce(&WindowContext)>;

fn intercept<T: 'static>(proxy: EventLoopProxy<T>, event: fn(Quit) -> T) -> Intercept {
    Box::new(move |context| {
        context.intercept_quit(proxy, move |reason: QuitReason| event(reason.into()));
    })
}

struct Handler<F> {
    callback: F,
    bridge: Bridge,
    result: Result<()>,
    input: window::Input,
    intercept: Option<Intercept>,
    /// The strongest Quit winit holds until `finish_quit`.
    quit: Option<Quit>,
    /// The callback ended the loop; later events are not delivered.
    done: bool,
    #[cfg(feature = "macos-ci")]
    forward_runner_keys: bool,
}

impl<F: Callback> Handler<F> {
    fn new(callback: F, bridge: Bridge, intercept: Intercept) -> Self {
        Self {
            callback,
            bridge,
            result: Ok(()),
            input: window::Input::default(),
            intercept: Some(intercept),
            quit: None,
            done: false,
            #[cfg(feature = "macos-ci")]
            forward_runner_keys: false,
        }
    }

    fn emit(&mut self, context: &WindowContext, event: Event) {
        if context.exiting() || self.done {
            return;
        }
        match (self.callback)(context, self.bridge.readiness(), event) {
            Ok(ControlFlow::Continue(())) => (),
            Ok(ControlFlow::Break(())) => self.finish(context),
            Err(error) if self.quit == Some(Quit::System) => {
                tracing::error!(%error, "replying to the system quit after a fatal error");
                self.finish(context);
            }
            Err(error) => {
                self.result = Err(error);
                self.done = true;
                context.exit();
            }
        }
    }

    fn finish(&mut self, context: &WindowContext) {
        self.done = true;
        match self.quit {
            Some(quit) => {
                tracing::info!(reason = ?quit, "drained; letting AppKit quit");
                context.finish_quit();
            }
            None => context.exit(),
        }
    }

    fn resume(&mut self, context: &WindowContext) {
        if let Some(intercept) = self.intercept.take() {
            intercept(context);
        }
        self.emit(context, Event::Resumed);
    }

    fn quit(&mut self, context: &WindowContext, quit: Quit) {
        self.quit = self.quit.max(Some(quit));
        self.emit(context, Event::Quit(quit));
    }
}

impl<F: Callback> ApplicationHandler<Wake> for Handler<F> {
    fn resumed(&mut self, context: &WindowContext) {
        self.resume(context);
    }

    fn user_event(&mut self, context: &WindowContext, wake: Wake) {
        match wake {
            Wake::Ready if self.bridge.readiness().accept_wake() => {
                self.emit(context, Event::Ready);
            }
            Wake::Ready => (),
            Wake::Quit(quit) => self.quit(context, quit),
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
