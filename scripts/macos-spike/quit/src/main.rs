#![forbid(unsafe_code)]

//! Native Quit fixture. `SUMIE_QUIT_TRIGGER` selects how both Quit requests enter AppKit:
//! `menu` performs the Quit item's action, `key` posts Cmd+Q to the application's event queue,
//! `cg` posts Cmd+Q through the window server to this PID (only with existing access),
//! and `nested` runs a nested run loop in the reaping handler before it returns. `nested-queued`
//! also drops the window first, so winit has its own queued events for that nested loop.

use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use objc2::{MainThreadMarker, rc::Retained, sel};
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSMenu};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGPreflightPostEventAccess};
use objc2_foundation::{NSDate, NSPoint, NSRunLoop, ns_string};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    platform::macos::ActiveEventLoopExtMacOS,
    window::{Window, WindowId},
};

const KEY_Q: u16 = 12;
/// Lets a posted repeat request reach AppKit before the child can exit.
const HOLD_CHILD: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Menu,
    Key,
    Cg,
    Nested,
    NestedQueued,
}

struct Probe {
    trigger: Trigger,
    requested: EventLoopProxy<()>,
    window: Option<Window>,
    child: Child,
    draining: Option<Instant>,
    reaped: bool,
}

fn app() -> Retained<NSApplication> {
    NSApplication::sharedApplication(MainThreadMarker::new().expect("process main thread"))
}

fn application_menu(app: &NSApplication) -> Retained<NSMenu> {
    app.mainMenu().unwrap().itemAtIndex(0).unwrap().submenu().unwrap()
}

fn request_quit(trigger: Trigger) {
    let app = app();
    if trigger == Trigger::Cg {
        let pid = i32::try_from(std::process::id()).unwrap();
        for pressed in [true, false] {
            let event = CGEvent::new_keyboard_event(None, KEY_Q, pressed).expect("Cmd+Q event");
            CGEvent::set_flags(Some(&event), CGEventFlags::MaskCommand);
            CGEvent::post_to_pid(pid, Some(&event));
        }
        return;
    }
    if trigger == Trigger::Key {
        let q = ns_string!("q");
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags::Command,
            0.0,
            app.windows().firstObject().map_or(0, |window| window.windowNumber()),
            None,
            q,
            q,
            false,
            KEY_Q,
        )
        .expect("Cmd+Q event");
        app.postEvent_atStart(&event, false);
        return;
    }
    let menu = application_menu(&app);
    let index = (0..menu.numberOfItems())
        .find(|&i| menu.itemAtIndex(i).unwrap().action() == Some(sel!(terminate:)))
        .expect("winit's application menu has a Quit item");
    menu.performActionForItemAtIndex(index);
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, context: &ActiveEventLoop) {
        self.window = Some(context.create_window(Window::default_attributes()).unwrap());
        context.intercept_quit(self.requested.clone(), ());
        request_quit(self.trigger);
        eprintln!("quit: first request returned");
    }

    fn user_event(&mut self, context: &ActiveEventLoop, (): ()) {
        assert!(self.draining.is_none(), "duplicate QuitRequested");
        self.draining = Some(Instant::now());
        assert!(self.child.try_wait().unwrap().is_none(), "child exited before draining");
        eprintln!("quit: draining");
        request_quit(self.trigger);
        eprintln!("quit: repeat request returned");
        context.set_control_flow(ControlFlow::Poll);
    }

    fn about_to_wait(&mut self, context: &ActiveEventLoop) {
        let Some(started) = self.draining else {
            return;
        };
        if self.reaped {
            return;
        }
        if self.child.stdin.is_some() && started.elapsed() >= HOLD_CHILD {
            drop(self.child.stdin.take());
        }
        let Some(status) = self.child.try_wait().unwrap() else {
            return;
        };
        assert!(status.success());
        self.reaped = true;
        // Dropping the window queues winit events; a nested run loop would run them inside this
        // handler and hit winit's re-entrancy panic before the Quit hook is exercised.
        if self.trigger != Trigger::Nested {
            drop(self.window.take());
        }
        eprintln!("quit: reaped");
        context.set_control_flow(ControlFlow::Wait);
        context.finish_quit();
        if matches!(self.trigger, Trigger::Nested | Trigger::NestedQueued) {
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.2));
            eprintln!("quit: nested run loop returned");
        }
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}

    fn exiting(&mut self, _: &ActiveEventLoop) {
        assert!(self.draining.is_some() && self.reaped);
        eprintln!("quit: exiting");
    }
}

fn main() {
    let trigger = match std::env::var("SUMIE_QUIT_TRIGGER").as_deref() {
        Ok("menu") => Trigger::Menu,
        Ok("key") => Trigger::Key,
        Ok("cg") => Trigger::Cg,
        Ok("nested") => Trigger::Nested,
        Ok("nested-queued") => Trigger::NestedQueued,
        other => panic!("unknown SUMIE_QUIT_TRIGGER {other:?}"),
    };
    if trigger == Trigger::Cg && !CGPreflightPostEventAccess() {
        eprintln!("quit: no post-event access");
        std::process::exit(3);
    }
    let event_loop = EventLoop::with_user_event().build().unwrap();
    let requested = event_loop.create_proxy();
    let child = Command::new("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    event_loop
        .run_app(&mut Probe {
            trigger,
            requested,
            window: None,
            child,
            draining: None,
            reaped: false,
        })
        .unwrap();
    panic!("native Quit must exit through AppKit, not return from run_app");
}
