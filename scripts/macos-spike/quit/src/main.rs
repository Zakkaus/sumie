#![forbid(unsafe_code)]

//! Native Quit fixture. `SUMIE_QUIT_TRIGGER` selects how Quit requests enter AppKit:
//! `menu` performs the Quit item's action, `key` posts Cmd+Q to the application's event queue,
//! `cg` posts Cmd+Q through the window server to this PID (only with existing access),
//! and `nested` runs a nested run loop in the reaping handler before it returns. `nested-queued`
//! also drops the window first, so winit has its own queued events for that nested loop.
//! `logout` and `shutdown` send this process a quit Apple event with that `keyAEQuitReason`;
//! `menu-during-later` sends `logout` and performs the Quit item while AppKit waits for the reply.
//! `quit-all` and `unknown-reason` send `kAEQuitAll` and an unassigned reason code, which winit
//! must also treat as system quits. `upgrade` performs the Quit item, then sends `logout` while
//! the ordinary request drains.
//! `external` waits for one Quit from outside the process (a real Cmd+Q or an AX menu click).
//! Phases go to stderr as `quit: <phase>`; winit's own decisions as `winit::quit: <phase>`.

use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use objc2::{MainThreadMarker, rc::Retained, sel};
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSMenu};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGPreflightPostEventAccess};
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventSendOptions, NSDate, NSPoint, NSRunLoop, ns_string,
};
use tracing_subscriber::filter::LevelFilter;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    platform::macos::{ActiveEventLoopExtMacOS, QuitReason},
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
    Logout,
    Shutdown,
    MenuDuringLater,
    QuitAll,
    UnknownReason,
    Upgrade,
    External,
}

impl Trigger {
    /// The `keyAEQuitReason` of the quit Apple event this trigger starts with, if any.
    fn system_reason(self) -> Option<&'static [u8; 4]> {
        match self {
            Self::Logout | Self::MenuDuringLater => Some(b"rlgo"),
            Self::Shutdown => Some(b"shut"),
            Self::QuitAll => Some(b"quia"),
            Self::UnknownReason => Some(b"zzzz"),
            _ => None,
        }
    }
}

struct Probe {
    trigger: Trigger,
    requested: EventLoopProxy<QuitReason>,
    window: Option<Window>,
    child: Child,
    /// When draining started and the latest request's reason.
    draining: Option<(Instant, QuitReason)>,
    reaped: bool,
}

fn app() -> Retained<NSApplication> {
    NSApplication::sharedApplication(MainThreadMarker::new().expect("process main thread"))
}

fn application_menu(app: &NSApplication) -> Retained<NSMenu> {
    app.mainMenu().unwrap().itemAtIndex(0).unwrap().submenu().unwrap()
}

const fn code(bytes: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*bytes)
}

/// Send this process `kAEQuitApplication` with `reason` as `keyAEQuitReason`, as loginwindow does.
fn send_system_quit(reason: &[u8; 4]) {
    let pid = i32::try_from(std::process::id()).unwrap();
    let target = NSAppleEventDescriptor::descriptorWithProcessIdentifier(pid);
    let event =
        NSAppleEventDescriptor::appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID(
            code(b"aevt"),
            code(b"quit"),
            Some(&target),
            -1, // kAutoGenerateReturnID
            0,  // kAnyTransactionID
        );
    event.setAttributeDescriptor_forKeyword(
        &NSAppleEventDescriptor::descriptorWithTypeCode(code(reason)),
        code(b"why?"),
    );
    let options = NSAppleEventSendOptions::NoReply | NSAppleEventSendOptions::NeverInteract;
    event.sendEventWithOptions_timeout_error(options, 10.0).expect("send the quit Apple event");
}

/// Send a system quit once the current handler returns, so AppKit's modal loop runs outside it.
fn request_system_quit(context: &ActiveEventLoop, reason: &'static [u8; 4]) {
    context.run_after_handler(move || {
        eprintln!("quit: request");
        send_system_quit(reason);
        eprintln!("quit: request returned");
    });
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

/// Whether AppKit is waiting for `replyToApplicationShouldTerminate:`.
fn in_terminate_later() -> bool {
    NSRunLoop::currentRunLoop()
        .currentMode()
        .is_some_and(|mode| mode.isEqualToString(ns_string!("NSModalPanelRunLoopMode")))
}

impl ApplicationHandler<QuitReason> for Probe {
    fn resumed(&mut self, context: &ActiveEventLoop) {
        self.window = Some(context.create_window(Window::default_attributes()).unwrap());
        context.intercept_quit(self.requested.clone(), |reason| reason);
        if self.trigger == Trigger::External {
            eprintln!("quit: waiting");
        } else if let Some(reason) = self.trigger.system_reason() {
            // Sent from outside the handler: AppKit runs its modal loop inside `terminate:`.
            request_system_quit(context, reason);
        } else {
            request_quit(self.trigger);
            eprintln!("quit: first request returned");
        }
    }

    fn user_event(&mut self, context: &ActiveEventLoop, reason: QuitReason) {
        // winit asks again only to upgrade an ordinary drain to a system one.
        let (started, upgrade) = match self.draining {
            None => (Instant::now(), false),
            Some((started, QuitReason::Ordinary)) if reason == QuitReason::System => {
                (started, true)
            },
            Some(_) => panic!("duplicate QuitRequested"),
        };
        self.draining = Some((started, reason));
        assert!(self.child.try_wait().unwrap().is_none(), "child exited before draining");
        let system = match self.trigger {
            Trigger::Upgrade => upgrade,
            trigger => trigger.system_reason().is_some(),
        };
        if system || reason == QuitReason::System {
            assert_eq!(reason, QuitReason::System, "system quit arrived as {reason:?}");
            if !in_terminate_later() {
                // AppKit only drains in its modal-panel loop while a reply is owed.
                eprintln!("quit: veto during system quit");
                std::process::exit(1);
            }
            eprintln!("quit: draining in later");
        } else {
            eprintln!("quit: draining");
        }
        match self.trigger {
            Trigger::Logout
            | Trigger::Shutdown
            | Trigger::QuitAll
            | Trigger::UnknownReason
            | Trigger::External => {},
            Trigger::Upgrade => {
                if !upgrade {
                    request_system_quit(context, b"rlgo");
                }
            },
            Trigger::MenuDuringLater => context.run_after_handler(|| {
                request_quit(Trigger::Menu);
                eprintln!("quit: menu returned");
            }),
            trigger => {
                request_quit(trigger);
                eprintln!("quit: repeat request returned");
            },
        }
        context.set_control_flow(ControlFlow::Poll);
    }

    fn about_to_wait(&mut self, context: &ActiveEventLoop) {
        let Some((started, _)) = self.draining else {
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
        Ok("logout") => Trigger::Logout,
        Ok("shutdown") => Trigger::Shutdown,
        Ok("menu-during-later") => Trigger::MenuDuringLater,
        Ok("quit-all") => Trigger::QuitAll,
        Ok("unknown-reason") => Trigger::UnknownReason,
        Ok("upgrade") => Trigger::Upgrade,
        Ok("external") => Trigger::External,
        other => panic!("unknown SUMIE_QUIT_TRIGGER {other:?}"),
    };
    if trigger == Trigger::Cg && !CGPreflightPostEventAccess() {
        eprintln!("quit: no post-event access");
        std::process::exit(3);
    }
    // winit logs its Quit decisions at debug level; the fixture prints them beside its phases.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(LevelFilter::DEBUG)
        .with_level(false)
        .without_time()
        .init();
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
