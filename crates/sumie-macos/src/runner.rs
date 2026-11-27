//! Native operations for the opt-in runner probe. No permission requests or general pasteboard access.

use std::{
    cell::RefCell,
    io, process,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSPasteboard, NSWindow,
};
use objc2_core_graphics::{
    CGEvent, CGPreflightPostEventAccess, CGPreflightScreenCaptureAccess,
    CGWindowListCopyWindowInfo, CGWindowListOption,
};
use objc2_foundation::{NSDate, NSPoint, NSRunLoop, NSString, ns_string};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize},
    event::{KeyEvent, WindowEvent},
    event_loop::{EventLoop, EventLoopProxy},
    keyboard::{KeyCode, PhysicalKey},
    platform::macos::ActiveEventLoopExtMacOS as _,
    window::{Fullscreen, Window, WindowId},
};

use super::{
    Bridge, Callback, Event, Handler, Platform, Result, WindowContext, intercept,
    termination::Quit, waker,
};

pub use post::{KeyPhase, MousePhase, PostedKey, PostedMouse, post_key, post_mouse};
pub use quit::request_quit;

mod post;
mod quit;

const TITLE: &str = "sumie / CI runner probe";
const F13: u16 = 0x69;
const F14: u16 = 0x6b;
thread_local! {
    /// A second window that takes key status from the probe window for `blur`; its events
    /// never reach the callback.
    static DECOY: RefCell<Option<Arc<Window>>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Ping(u64),
    /// Text for the focused pane, as if typed: the rest of the line plus a carriage return.
    Type(String),
    /// Print the terminal driver's counters.
    Report,
    Finish,
    Inspect,
    AppKitKeys,
    CgKeys,
    /// One `CGEvent` key, posted to this process while the probe window is key.
    Key(PostedKey),
    /// One mouse event at a cell of the focused pane, queued to the probe window.
    Mouse(PostedMouse),
    Close,
    /// Window steps for the idle mode; see `scripts/macos-spike/animations.py`.
    Step(Step),
    /// Ask `AppKit` to quit: `terminate:` as the Quit menu item does, or a quit Apple event
    /// with a log-out reason, as loginwindow sends.
    RequestQuit(Quit),
    /// Fail the terminal driver with an injected error.
    Fail,
    /// Block the main thread for a minute: a stuck stage for the watchdog.
    Stall,
    /// Close the terminal window, then open a new one in the same process.
    Cycle,
    Invalid,
    /// Not a controller command: the readiness bridge's wake.
    Wake,
    /// Not a controller command: `AppKit` asked to quit.
    Quit(Quit),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    MinimizeAfterHandler,
    MinimizeInHandler,
    Restore,
    Fullscreen,
    Windowed,
    /// Another window of this application becomes key, as after Cmd+Tab.
    Blur,
    /// The probe window becomes key again.
    Focus,
}

impl Command {
    fn parse(line: &str) -> Self {
        match line {
            "finish" => Self::Finish,
            "inspect" => Self::Inspect,
            "appkit" => Self::AppKitKeys,
            "cg" => Self::CgKeys,
            "close" => Self::Close,
            "minimize-after" => Self::Step(Step::MinimizeAfterHandler),
            "minimize-in" => Self::Step(Step::MinimizeInHandler),
            "restore" => Self::Step(Step::Restore),
            "fullscreen" => Self::Step(Step::Fullscreen),
            "windowed" => Self::Step(Step::Windowed),
            "blur" => Self::Step(Step::Blur),
            "focus" => Self::Step(Step::Focus),
            "report" => Self::Report,
            "quit" => Self::RequestQuit(Quit::Ordinary),
            "system-quit" => Self::RequestQuit(Quit::System),
            "fail" => Self::Fail,
            "stall" => Self::Stall,
            "cycle" => Self::Cycle,
            line => {
                if let Some(text) = line.strip_prefix("type ") {
                    return Self::Type(format!("{text}\r"));
                }
                if let Some(key) = line.strip_prefix("key ") {
                    return PostedKey::parse(key).map_or(Self::Invalid, Self::Key);
                }
                if let Some(mouse) = line.strip_prefix("mouse ") {
                    return PostedMouse::parse(mouse).map_or(Self::Invalid, Self::Mouse);
                }
                line.strip_prefix("ping ")
                    .and_then(|number| number.parse().ok())
                    .map_or(Self::Invalid, Self::Ping)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRoute {
    AppKit,
    Cg,
}

impl KeyRoute {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::AppKit => "appkit",
            Self::Cg => "cg",
        }
    }
}

/// Only the fixture's two physical keys reach the probe protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub route: KeyRoute,
    pub pressed: bool,
}

impl Key {
    pub(super) fn from_event(event: &KeyEvent) -> Option<Self> {
        if event.repeat {
            return None;
        }
        let route = match event.physical_key {
            PhysicalKey::Code(KeyCode::F13) => KeyRoute::AppKit,
            PhysicalKey::Code(KeyCode::F14) => KeyRoute::Cg,
            _ => return None,
        };
        Some(Self {
            route,
            pressed: event.state.is_pressed(),
        })
    }
}

/// Run the same winit window handler with numbered controller commands on its main thread.
///
/// # Errors
/// Propagates event loop, thread creation, and callback errors.
pub fn run(callback: impl Callback) -> Result<()> {
    let event_loop = EventLoop::<Command>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let bridge = Bridge::spawn(waker(proxy.clone(), Command::Wake))?;
    let commands = proxy.clone();
    thread::Builder::new()
        .name("sumie-runner-control".into())
        .spawn(move || control(&commands))?;
    let mut handler = RunnerHandler(Handler {
        forward_runner_keys: true,
        ..Handler::new(callback, bridge, intercept(proxy, Command::Quit))
    });
    event_loop.run_app(&mut handler)?;
    handler.0.result
}

fn control(proxy: &EventLoopProxy<Command>) {
    for line in io::stdin().lines() {
        let command = line.map_or(Command::Invalid, |line| Command::parse(&line));
        let last = command == Command::Invalid;
        if proxy.send_event(command).is_err() || last {
            return;
        }
    }
}

struct RunnerHandler<F>(Handler<F>);

impl<F: Callback> ApplicationHandler<Command> for RunnerHandler<F> {
    fn resumed(&mut self, context: &WindowContext) {
        self.0.resume(context);
    }

    fn user_event(&mut self, context: &WindowContext, event: Command) {
        match event {
            Command::Wake if self.0.bridge.readiness().accept_wake() => {
                self.0.emit(context, Event::Ready);
            }
            Command::Wake => (),
            Command::Quit(quit) => self.0.quit(context, quit),
            command => self.0.emit(context, Event::RunnerCommand(command)),
        }
    }

    fn window_event(&mut self, context: &WindowContext, id: WindowId, event: WindowEvent) {
        if DECOY.with_borrow(|decoy| decoy.as_ref().is_some_and(|decoy| decoy.id() == id)) {
            return;
        }
        self.0.window_event(context, id, event);
    }

    fn about_to_wait(&mut self, context: &WindowContext) {
        self.0.about_to_wait(context);
    }
}

pub fn name_window(window: &Platform) {
    window.0.set_title(TITLE);
}

/// Apply a measurement step to the window, either now or once no winit handler runs.
///
/// # Errors
/// Returns the window server's error if the decoy window for `blur` cannot be created.
pub fn step(context: &WindowContext, window: &Platform, step: Step) -> Result<()> {
    let decoy = match step {
        Step::Blur => Some(decoy(context)?),
        Step::Focus => DECOY.with_borrow(Clone::clone),
        _ => None,
    };
    let window = window.window_owner();
    let apply = move || match step {
        Step::MinimizeAfterHandler | Step::MinimizeInHandler => window.set_minimized(true),
        Step::Restore => {
            window.set_minimized(false);
            window.focus_window();
        }
        Step::Fullscreen => window.set_fullscreen(Some(Fullscreen::Borderless(None))),
        Step::Windowed => window.set_fullscreen(None),
        Step::Blur => {
            if let Some(decoy) = decoy {
                decoy.set_visible(true);
                decoy.focus_window();
            }
        }
        Step::Focus => {
            if let Some(decoy) = decoy {
                decoy.set_visible(false);
            }
            window.focus_window();
        }
    };
    if step == Step::MinimizeInHandler {
        apply();
    } else {
        context.run_after_handler(apply);
    }
    Ok(())
}

/// The hidden decoy window, created on first use in the screen's corner, clear of the probe.
fn decoy(context: &WindowContext) -> Result<Arc<Window>> {
    if let Some(decoy) = DECOY.with_borrow(Clone::clone) {
        return Ok(decoy);
    }
    let attributes = Window::default_attributes()
        .with_title("sumie / CI runner decoy")
        .with_visible(false)
        .with_position(LogicalPosition::new(0, 0))
        .with_inner_size(LogicalSize::new(160, 60));
    let decoy = Arc::new(context.create_window(attributes)?);
    DECOY.set(Some(Arc::clone(&decoy)));
    Ok(decoy)
}

/// The `AppKit` run-loop modes that the wake scenario enters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunLoopMode {
    Default,
    EventTracking,
    ModalPanel,
}

impl RunLoopMode {
    fn name(self) -> &'static NSString {
        // The values of `NSDefaultRunLoopMode`, `NSEventTrackingRunLoopMode` and
        // `NSModalPanelRunLoopMode`; run loops compare modes by name.
        match self {
            Self::Default => ns_string!("kCFRunLoopDefaultMode"),
            Self::EventTracking => ns_string!("NSEventTrackingRunLoopMode"),
            Self::ModalPanel => ns_string!("NSModalPanelRunLoopMode"),
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::EventTracking => "event-tracking",
            Self::ModalPanel => "modal-panel",
        }
    }

    /// The mode the main run loop is running in, if it is one of these.
    #[must_use]
    pub fn current() -> Option<Self> {
        let mode = NSRunLoop::currentRunLoop().currentMode()?;
        [Self::Default, Self::EventTracking, Self::ModalPanel]
            .into_iter()
            .find(|candidate| mode.isEqualToString(candidate.name()))
    }

    /// Run the current run loop in this mode for `length`, as `AppKit`'s own loops do.
    pub fn run_for(self, length: Duration) {
        let end = Instant::now() + length;
        let run_loop = NSRunLoop::currentRunLoop();
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            let limit = NSDate::dateWithTimeIntervalSinceNow(left.as_secs_f64());
            run_loop.runMode_beforeDate(self.name(), &limit);
        }
    }
}

/// Run `closure` once no winit handler is running, through the fork's common-modes queue.
pub fn after_handler(context: &WindowContext, closure: impl FnOnce() + 'static) {
    context.run_after_handler(closure);
}

fn app() -> Option<Retained<NSApplication>> {
    MainThreadMarker::new().map(NSApplication::sharedApplication)
}

fn window(app: &NSApplication) -> Option<Retained<NSWindow>> {
    let windows = app.windows();
    (0..windows.count())
        .map(|index| windows.objectAtIndex(index))
        .find(|window| window.title().isEqualToString(ns_string!(TITLE)))
}

fn input_state(app: &NSApplication, window: &NSWindow) -> (bool, bool) {
    let key = window.isKeyWindow()
        && app
            .keyWindow()
            .is_some_and(|key| std::ptr::eq(Retained::as_ptr(&key), window));
    let responder = window
        .firstResponder()
        .zip(window.contentView())
        .is_some_and(|(responder, view)| {
            std::ptr::eq(
                Retained::as_ptr(&responder).cast::<()>(),
                Retained::as_ptr(&view).cast::<()>(),
            )
        });
    (key, responder)
}

#[derive(Default)]
pub struct WindowState {
    pub id: u32,
    pub window_server: bool,
    pub key: bool,
    pub responder: bool,
}

pub struct Inspection {
    pub window: WindowState,
    pub pasteboard: bool,
    pub screen_capture_access: bool,
    pub post_event_access: bool,
}

/// Inspect only this process's named window and a throwaway, named pasteboard.
#[must_use]
pub fn inspect() -> Inspection {
    let window = app().and_then(|app| {
        window(&app).map(|window| {
            let id = u32::try_from(window.windowNumber()).unwrap_or(0);
            let window_server = id != 0
                && window.isVisible()
                && CGWindowListCopyWindowInfo(CGWindowListOption::OptionIncludingWindow, id)
                    .is_some_and(|windows| !windows.is_empty());
            let (key, responder) = input_state(&app, &window);
            WindowState {
                id,
                window_server,
                key,
                responder,
            }
        })
    });
    Inspection {
        window: window.unwrap_or_default(),
        pasteboard: named_pasteboard(),
        screen_capture_access: CGPreflightScreenCaptureAccess(),
        post_event_access: CGPreflightPostEventAccess(),
    }
}

fn named_pasteboard() -> bool {
    let board = NSPasteboard::pasteboardWithUniqueName();
    let sentinel = ns_string!("sumie runner fixture");
    let text = ns_string!("public.utf8-plain-text");
    board.clearContents();
    let verified = board.setString_forType(sentinel, text)
        && NSPasteboard::pasteboardWithName(&board.name())
            .stringForType(text)
            .is_some_and(|value| value.isEqualToString(sentinel));
    board.clearContents();
    verified
}

/// Queue a real F13 down/up pair in `AppKit`, addressed to the probe window.
#[must_use]
pub fn post_appkit_keys() -> bool {
    let Some(app) = app() else {
        return false;
    };
    let Some(window) = window(&app) else {
        return false;
    };
    if input_state(&app, &window) != (true, true) {
        return false;
    }
    let f13 = ns_string!("\u{f710}");
    let [Some(down), Some(up)] = [NSEventType::KeyDown, NSEventType::KeyUp].map(|kind| {
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags::empty(),
            0.0,
            window.windowNumber(),
            None,
            f13,
            f13,
            false,
            F13,
        )
    }) else {
        return false;
    };
    app.postEvent_atStart(&down, false);
    app.postEvent_atStart(&up, false);
    true
}

/// With existing permission, post a distinct F14 pair to this PID only.
#[must_use]
pub fn post_cg_keys() -> bool {
    if !CGPreflightPostEventAccess() {
        return false;
    }
    let Some(app) = app() else {
        return false;
    };
    if window(&app).is_none_or(|window| input_state(&app, &window) != (true, true)) {
        return false;
    }
    let Ok(pid) = i32::try_from(process::id()) else {
        return false;
    };
    let [Some(down), Some(up)] =
        [true, false].map(|pressed| CGEvent::new_keyboard_event(None, F14, pressed))
    else {
        return false;
    };
    CGEvent::post_to_pid(pid, Some(&down));
    CGEvent::post_to_pid(pid, Some(&up));
    true
}

/// Invoke the standard window close action, not the application Quit action.
#[must_use]
pub fn perform_close() -> bool {
    let Some(window) = app().and_then(|app| window(&app)) else {
        return false;
    };
    window.performClose(None);
    true
}
