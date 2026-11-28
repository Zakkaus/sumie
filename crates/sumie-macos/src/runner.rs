//! Native operations for the opt-in runner. No permission requests or general pasteboard access.

use std::{
    cell::RefCell,
    io,
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSApplication, NSWindow};
use objc2_foundation::{NSDate, NSRunLoop, NSString, ns_string};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize},
    event::WindowEvent,
    event_loop::{EventLoop, EventLoopProxy},
    platform::macos::ActiveEventLoopExtMacOS as _,
    window::{Window, WindowId},
};

use super::{
    Bridge, Callback, Event, Handler, Platform, Result, WindowContext, ime::Ime, intercept,
    termination::Quit, waker,
};

pub use post::{KeyPhase, MousePhase, PostedKey, PostedMouse, post_key, post_mouse};
pub use quit::request_quit;

mod post;
mod quit;

const TITLE: &str = "sumie / CI runner probe";
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
    /// `capture DIR`: read back every frame presented from now on into `DIR/N.png`, N its
    /// present count; `capture off` stops.
    Capture(Option<PathBuf>),
    /// `cells ROW`: print the width class of each cell in a row of the focused pane.
    Cells(u16),
    /// Print the window's device size and the focused pane's grid origin and cell size.
    Geometry,
    /// One `CGEvent` key, posted to this process while the probe window is key.
    Key(PostedKey),
    /// One mouse event at a cell of the focused pane, queued to the probe window.
    Mouse(PostedMouse),
    /// `ime on` or `ime off`: let input methods compose while the window is key.
    ImeAllowed(bool),
    /// An input-method event as winit would report it, for the protocol check: `ime preedit
    /// [TEXT]` (its caret at the end), `ime commit TEXT`, `ime disabled`.
    InjectIme(Ime),
    /// `ime search TEXT`: Cmd+F and a preedit of TEXT in one batch, as when the input method
    /// composes the key typed right after the chord.
    SearchThenPreedit(String),
    /// `ime overlay`: print the composition overlay's length and selected clause, and the
    /// input generation.
    Overlay,
    /// `ime source`: print the selected keyboard input source.
    InputSource,
    Close,
    /// Send the logical key that confirms (`y`) or cancels (Enter) a close prompt.
    CloseAnswer(bool),
    /// A window step: `minimize`, `blur`, `focus` or `resize W H`.
    Step(Step),
    /// Ask `AppKit` to quit: `terminate:` as the Quit menu item does, or a quit Apple event
    /// with a log-out reason, as loginwindow sends.
    RequestQuit(Quit),
    /// Fail the terminal driver with an injected error.
    Fail,
    /// Block the main thread for a minute: a stuck stage for the watchdog.
    Stall,
    /// Close the terminal window and drain it, staying in the same process without a window.
    Cycle,
    /// Open the next terminal window after a `cycle`.
    Open,
    Invalid,
    /// Not a controller command: the readiness bridge's wake.
    Wake,
    /// Not a controller command: `AppKit` asked to quit.
    Quit(Quit),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Minimize,
    /// Another window of this application becomes key, as after Cmd+Tab.
    Blur,
    /// `resize W H`: set the content size in points, as a script or zoom does.
    Resize(u32, u32),
    /// The probe window becomes key again.
    Focus,
}

impl Command {
    fn parse(line: &str) -> Self {
        match line {
            "close" => Self::Close,
            "close-yes" => Self::CloseAnswer(true),
            "close-cancel" => Self::CloseAnswer(false),
            "minimize" => Self::Step(Step::Minimize),
            "blur" => Self::Step(Step::Blur),
            "focus" => Self::Step(Step::Focus),
            "report" => Self::Report,
            "geometry" => Self::Geometry,
            "quit" => Self::RequestQuit(Quit::Ordinary),
            "system-quit" => Self::RequestQuit(Quit::System),
            "fail" => Self::Fail,
            "stall" => Self::Stall,
            "cycle" => Self::Cycle,
            "open" => Self::Open,
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
                if let Some(ime) = line.strip_prefix("ime") {
                    return Self::ime(ime);
                }
                if let Some(dir) = line.strip_prefix("capture ") {
                    return Self::Capture((dir != "off").then(|| dir.into()));
                }
                if let Some(row) = line.strip_prefix("cells ") {
                    return row.parse().map_or(Self::Invalid, Self::Cells);
                }
                if let Some(size) = line.strip_prefix("resize ") {
                    return size
                        .split_once(' ')
                        .and_then(|(w, h)| Some(Step::Resize(w.parse().ok()?, h.parse().ok()?)))
                        .map_or(Self::Invalid, Self::Step);
                }
                line.strip_prefix("ping ")
                    .and_then(|number| number.parse().ok())
                    .map_or(Self::Invalid, Self::Ping)
            }
        }
    }
}

impl Command {
    fn ime(fields: &str) -> Self {
        match fields {
            " on" => Self::ImeAllowed(true),
            " off" => Self::ImeAllowed(false),
            " overlay" => Self::Overlay,
            " source" => Self::InputSource,
            " disabled" => Self::InjectIme(Ime::Disabled),
            " preedit" => Self::InjectIme(Ime::Preedit {
                text: String::new(),
                cursor: None,
            }),
            fields => {
                if let Some(text) = fields.strip_prefix(" search ") {
                    return Self::SearchThenPreedit(text.to_owned());
                }
                if let Some(text) = fields.strip_prefix(" preedit ") {
                    let cursor = Some((text.len(), text.len()));
                    return Self::InjectIme(Ime::Preedit {
                        text: text.to_owned(),
                        cursor,
                    });
                }
                fields
                    .strip_prefix(" commit ")
                    .map_or(Self::Invalid, |text| {
                        Self::InjectIme(Ime::Commit(text.to_owned()))
                    })
            }
        }
    }
}

/// One line for the controller: the event's kind and byte lengths, never its text.
#[must_use]
pub fn describe(ime: &Ime) -> String {
    match ime {
        Ime::Preedit { text, cursor } => match cursor {
            Some((start, end)) => format!("preedit {} {start} {end}", text.len()),
            None => format!("preedit {} - -", text.len()),
        },
        Ime::Commit(text) => format!("commit {}", text.len()),
        Ime::Disabled => "disabled".into(),
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
    let mut handler = RunnerHandler(Handler::new(
        callback,
        bridge,
        intercept(proxy, Command::Quit),
    ));
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

/// Apply a window step once no winit handler runs.
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
        Step::Minimize => window.set_minimized(true),
        Step::Resize(width, height) => {
            let _ = window.request_inner_size(LogicalSize::new(width, height));
        }
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
    context.run_after_handler(apply);
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

/// Invoke the standard window close action, not the application Quit action.
#[must_use]
pub fn perform_close() -> bool {
    let Some(window) = app().and_then(|app| window(&app)) else {
        return false;
    };
    window.performClose(None);
    true
}
