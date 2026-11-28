//! The native window and the conversion of its winit events. Pointer positions stay in
//! backing pixels; the binary maps them to the grid without passing through points.

use std::{sync::Arc, time::Instant};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSScreen, NSTextInputContext, NSWorkspace};
use objc2_foundation::{NSNumber, ns_string};
use sumie_core::{
    KeyModifiers,
    mouse::{Button, MousePhase},
};
use winit::{
    dpi::{LogicalPosition, LogicalSize},
    event::{
        ElementState, Ime as WinitIme, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
    },
    keyboard::{ModifiersKeyState, ModifiersState},
    platform::macos::{
        ActiveEventLoopExtMacOS as _, MonitorHandleExtMacOS as _, OptionAsAlt,
        WindowAttributesExtMacOS as _,
    },
    window::{CursorIcon, Window},
};

use super::{Event, Result, WindowContext, ime::Ime};

/// Used when the window has no screen or the screen reports no rate.
const FALLBACK_FPS: u32 = 60;

/// A pointer event in the backing pixels of the window's content view. Scrolling arrives as
/// [`PointerPhase::Scroll`], in winit's units, so the binary converts it once.
#[derive(Clone, Copy)]
pub struct Pointer {
    pub phase: PointerPhase,
    pub position: [f64; 2],
    pub modifiers: KeyModifiers,
}

#[derive(Clone, Copy)]
pub enum PointerPhase {
    /// A button or motion; never a wheel or touch phase.
    Mouse(MousePhase),
    Scroll(Scroll),
}

/// Vertical scrolling as winit reports it: positive reveals the content above.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scroll {
    /// A wheel without precise deltas, in lines; possibly fractional.
    Lines(f64),
    /// A trackpad or precise wheel, in backing pixels.
    Pixels(f64),
    /// The fingers lifted, or the system's momentum ended.
    Ended,
}

/// Window access from the winit main-thread callback. wgpu retains its own owner.
pub struct Platform(pub(super) Arc<Window>);

impl Platform {
    /// Create the window during the event loop's resume callback; `size` is in points.
    ///
    /// # Errors
    /// Returns the window server's creation error.
    pub fn create(context: &WindowContext, title: &str, size: [u32; 2]) -> Result<Self> {
        let attributes = Window::default_attributes()
            .with_title(title)
            .with_inner_size(LogicalSize::new(size[0], size[1]))
            .with_option_as_alt(OptionAsAlt::OnlyLeft);
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

    #[must_use]
    pub fn reduce_motion() -> bool {
        NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }

    #[must_use]
    pub fn has_focus(&self) -> bool {
        self.0.has_focus()
    }

    /// The display the window is on, if `AppKit` placed it on one.
    #[must_use]
    pub fn screen(&self) -> Option<u32> {
        self.0.current_monitor().map(|monitor| monitor.native_id())
    }

    /// `NSScreen.maximumFramesPerSecond` of the window's screen; read it again after a
    /// screen or scale change.
    #[must_use]
    pub fn maximum_frames_per_second(&self) -> u32 {
        let (Some(display), Some(mtm)) = (self.screen(), MainThreadMarker::new()) else {
            return FALLBACK_FPS;
        };
        let screens = NSScreen::screens(mtm);
        (0..screens.count())
            .map(|index| screens.objectAtIndex(index))
            .find(|screen| screen_number(screen) == Some(display))
            .and_then(|screen| u32::try_from(screen.maximumFramesPerSecond()).ok())
            .filter(|&fps| fps > 0)
            .unwrap_or(FALLBACK_FPS)
    }

    pub fn set_title(&self, title: &str) {
        self.0.set_title(title);
    }

    pub fn set_cursor_text(&self, text: bool) {
        self.0.set_cursor(if text {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        });
    }

    /// Let the input method compose; off, keys reach the session below it.
    pub fn set_ime_allowed(&self, allowed: bool) {
        self.0.set_ime_allowed(allowed);
    }

    /// End the native composition: the pinned winit discards the input method's marked text
    /// when input methods are disallowed, then they are allowed again.
    pub fn cancel_composition(&self) {
        self.0.set_ime_allowed(false);
        self.0.set_ime_allowed(true);
    }

    /// Place the candidate window at the caret, in points from the content view's top left.
    pub fn set_ime_cursor_area(&self, origin: [i32; 2], size: [i32; 2]) {
        self.0.set_ime_cursor_area(
            LogicalPosition::new(origin[0], origin[1]),
            LogicalSize::new(size[0], size[1]),
        );
    }

    /// The keyboard input source the active text input context selected, such as
    /// `com.apple.keylayout.ABC`.
    #[must_use]
    pub fn input_source() -> Option<String> {
        let mtm = MainThreadMarker::new()?;
        NSTextInputContext::currentInputContext(mtm)?
            .selectedKeyboardInputSource()
            .map(|source| source.to_string())
    }

    pub fn request_redraw(&self) {
        self.0.request_redraw();
    }

    pub fn before_present(&self) {
        self.0.pre_present_notify();
    }

    /// Take the window off screen (`orderOut:`) once no winit handler runs, then call `done`.
    pub fn order_out(&self, context: &WindowContext, done: impl FnOnce() + 'static) {
        let window = self.window_owner();
        context.run_after_handler(move || {
            window.set_visible(false);
            done();
        });
    }
}

fn screen_number(screen: &NSScreen) -> Option<u32> {
    screen
        .deviceDescription()
        .objectForKey(ns_string!("NSScreenNumber"))?
        .downcast::<NSNumber>()
        .ok()
        .map(|number| number.as_u32())
}

/// The desktop's double-click interval in milliseconds.
#[must_use]
pub fn double_click_interval() -> Option<u32> {
    let millis = (NSEvent::doubleClickInterval() * 1000.0).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked to be a positive interval below u32::MAX milliseconds"
    )]
    (millis > 0.0 && millis < f64::from(u32::MAX)).then_some(millis as u32)
}

/// Input state that winit reports separately from the events that need it.
pub(super) struct Input {
    modifiers: KeyModifiers,
    position: [f64; 2],
    entering: bool,
    origin: Instant,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            modifiers: KeyModifiers::default(),
            position: [0.0; 2],
            entering: false,
            origin: Instant::now(),
        }
    }
}

impl Input {
    pub(super) fn convert(&mut self, event: WindowEvent) -> Option<Event> {
        let pointer = |input: &Self, phase: PointerPhase| {
            Some(Event::Pointer(Pointer {
                phase,
                position: input.position,
                modifiers: input.modifiers,
            }))
        };
        Some(match event {
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => Event::Resized,
            WindowEvent::Moved(_) => Event::Moved,
            WindowEvent::Focused(focused) => Event::Focused(focused),
            WindowEvent::Occluded(occluded) => Event::Occluded(occluded),
            WindowEvent::RedrawRequested => Event::RedrawRequested,
            WindowEvent::CloseRequested => Event::CloseRequested,
            WindowEvent::ModifiersChanged(modifiers) => {
                // Only a known left Option is Meta, as winit's `OptionAsAlt::OnlyLeft` decides:
                // an Option flag without a side bit keeps the layout's characters, and with
                // both held winit already typed the key without Option.
                let left_alt = modifiers.lalt_state() == ModifiersKeyState::Pressed;
                self.modifiers = key_modifiers(modifiers.state(), left_alt);
                Event::ModifiersChanged(self.modifiers)
            }
            WindowEvent::KeyboardInput { event, .. } => {
                Event::Key(super::Key::new(&event, self.modifiers))
            }
            WindowEvent::Ime(ime) => Event::Ime(match ime {
                WinitIme::Enabled => return None,
                WinitIme::Preedit(text, cursor) => Ime::Preedit { text, cursor },
                WinitIme::Commit(text) => Ime::Commit(text),
                WinitIme::Disabled => Ime::Disabled,
            }),
            WindowEvent::CursorEntered { .. } => {
                self.entering = true;
                return None;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.position = [position.x, position.y];
                if std::mem::take(&mut self.entering) {
                    return Some(Event::PointerEntered(Pointer {
                        phase: PointerPhase::Mouse(MousePhase::Motion),
                        position: self.position,
                        modifiers: self.modifiers,
                    }));
                }
                return pointer(self, PointerPhase::Mouse(MousePhase::Motion));
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    MouseButton::Left => Button::Left,
                    MouseButton::Middle => Button::Middle,
                    MouseButton::Right => Button::Right,
                    _ => return None,
                };
                let phase = match state {
                    ElementState::Pressed => MousePhase::Press(button, self.millis()),
                    ElementState::Released => MousePhase::Release(button),
                };
                return pointer(self, PointerPhase::Mouse(phase));
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => Scroll::Lines(f64::from(lines)),
                    MouseScrollDelta::PixelDelta(_) if phase == TouchPhase::Ended => Scroll::Ended,
                    MouseScrollDelta::PixelDelta(pixels) => Scroll::Pixels(pixels.y),
                };
                return pointer(self, PointerPhase::Scroll(scroll));
            }
            _ => return None,
        })
    }

    /// Wrapping milliseconds since the window opened, for double-click timing.
    fn millis(&self) -> u32 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "press timestamps wrap like the Wayland ones"
        )]
        {
            self.origin.elapsed().as_millis() as u32
        }
    }
}

fn key_modifiers(state: ModifiersState, left_alt: bool) -> KeyModifiers {
    KeyModifiers {
        shift: state.shift_key(),
        control: state.control_key(),
        alt: state.alt_key(),
        right_alt: state.alt_key() && !left_alt,
        alt_consumed: false,
        super_key: state.super_key(),
        caps_lock: false,
        num_lock: false,
    }
}
