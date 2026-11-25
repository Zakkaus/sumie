//! Native operations for the opt-in runner probe. No permission requests or general pasteboard access.

use std::{io, ops::ControlFlow, process, thread};

use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSPasteboard, NSWindow,
};
use objc2_core_graphics::{
    CGEvent, CGPreflightPostEventAccess, CGPreflightScreenCaptureAccess,
    CGWindowListCopyWindowInfo, CGWindowListOption,
};
use objc2_foundation::{NSPoint, ns_string};
use winit::{
    application::ApplicationHandler,
    event::{KeyEvent, WindowEvent},
    event_loop::{EventLoop, EventLoopProxy},
    keyboard::{KeyCode, PhysicalKey},
    window::WindowId,
};

use super::{Event, Handler, Platform, Result, WindowContext};

const TITLE: &str = "sumie / CI runner probe";
const F13: u16 = 0x69;
const F14: u16 = 0x6b;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Ping(u64),
    Finish,
    Inspect,
    AppKitKeys,
    CgKeys,
    Close,
    Invalid,
}

impl Command {
    fn parse(line: &str) -> Self {
        match line {
            "finish" => Self::Finish,
            "inspect" => Self::Inspect,
            "appkit" => Self::AppKitKeys,
            "cg" => Self::CgKeys,
            "close" => Self::Close,
            line => line
                .strip_prefix("ping ")
                .and_then(|number| number.parse().ok())
                .map_or(Self::Invalid, Self::Ping),
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
pub fn run(callback: impl FnMut(&WindowContext, Event) -> Result<ControlFlow<()>>) -> Result<()> {
    let event_loop = EventLoop::<Command>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    thread::Builder::new()
        .name("sumie-runner-control".into())
        .spawn(move || bridge(&proxy))?;
    let mut handler = RunnerHandler(Handler {
        callback,
        result: Ok(()),
        forward_runner_keys: true,
    });
    event_loop.run_app(&mut handler)?;
    handler.0.result
}

fn bridge(proxy: &EventLoopProxy<Command>) {
    for line in io::stdin().lines() {
        let command = line.map_or(Command::Invalid, |line| Command::parse(&line));
        if proxy.send_event(command).is_err() || command == Command::Invalid {
            return;
        }
    }
}

struct RunnerHandler<F>(Handler<F>);

impl<F: FnMut(&WindowContext, Event) -> Result<ControlFlow<()>>> ApplicationHandler<Command>
    for RunnerHandler<F>
{
    fn resumed(&mut self, context: &WindowContext) {
        self.0.resumed(context);
    }

    fn user_event(&mut self, context: &WindowContext, event: Command) {
        self.0.emit(context, Event::RunnerCommand(event));
    }

    fn window_event(&mut self, context: &WindowContext, id: WindowId, event: WindowEvent) {
        self.0.window_event(context, id, event);
    }
}

pub fn name_window(window: &Platform) {
    window.0.set_title(TITLE);
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
