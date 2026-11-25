//! The native window and the conversion of its winit events. Pointer positions stay in
//! backing pixels; the binary maps them to the grid without passing through points.

use std::{sync::Arc, time::Instant};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSScreen};
use objc2_foundation::{NSNumber, ns_string};
use sumie_core::{
    KeyModifiers,
    mouse::{Button, MousePhase},
};
use winit::{
    dpi::LogicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent},
    keyboard::ModifiersState,
    platform::macos::{ActiveEventLoopExtMacOS as _, MonitorHandleExtMacOS as _},
    window::{CursorIcon, Window},
};

use super::{Event, Result, WindowContext};

/// Used when the window has no screen or the screen reports no rate.
const FALLBACK_FPS: u32 = 60;
const VALUE120_PER_NOTCH: f64 = 120.0;

/// A pointer event in the backing pixels of the window's content view. `MousePhase::Touch`
/// carries the trackpad's vertical movement in backing pixels, not value120 units; positive
/// scrolls down.
#[derive(Clone, Copy)]
pub struct Pointer {
    pub phase: MousePhase,
    pub position: [f64; 2],
    pub modifiers: KeyModifiers,
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
            .with_inner_size(LogicalSize::new(size[0], size[1]));
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
        let pointer = |input: &Self, phase| {
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
                self.modifiers = key_modifiers(modifiers.state());
                Event::ModifiersChanged(self.modifiers)
            }
            WindowEvent::KeyboardInput { event, .. } => {
                Event::Key(super::Key::new(&event, self.modifiers))
            }
            WindowEvent::CursorEntered { .. } => {
                self.entering = true;
                return None;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.position = [position.x, position.y];
                if std::mem::take(&mut self.entering) {
                    return Some(Event::PointerEntered(Pointer {
                        phase: MousePhase::Motion,
                        position: self.position,
                        modifiers: self.modifiers,
                    }));
                }
                return pointer(self, MousePhase::Motion);
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
                return pointer(self, phase);
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                // winit's positive deltas reveal content above; value120 scrolls down.
                let phase = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => {
                        MousePhase::Wheel(-f64::from(lines) * VALUE120_PER_NOTCH)
                    }
                    MouseScrollDelta::PixelDelta(_) if phase == TouchPhase::Ended => {
                        MousePhase::TouchEnd
                    }
                    MouseScrollDelta::PixelDelta(pixels) => MousePhase::Touch(-pixels.y),
                };
                return pointer(self, phase);
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

fn key_modifiers(state: ModifiersState) -> KeyModifiers {
    KeyModifiers {
        shift: state.shift_key(),
        control: state.control_key(),
        alt: state.alt_key(),
        super_key: state.super_key(),
        caps_lock: false,
        num_lock: false,
    }
}
