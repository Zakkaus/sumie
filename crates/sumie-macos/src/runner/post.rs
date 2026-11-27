//! Keys and mouse events the runner posts to its own probe window, as the controller names
//! them.

use std::process;

use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventType, CGPreflightPostEventAccess,
};
use objc2_foundation::NSPoint;

use super::{app, input_state, window};

/// `kVK_Shift`, `kVK_Control`, `kVK_Option`, `kVK_Command` and their right-hand twins: their
/// events are flag changes, not key downs.
const MODIFIER_KEYS: [u16; 8] = [0x38, 0x3b, 0x3a, 0x37, 0x3c, 0x3e, 0x3d, 0x36];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyPhase {
    Down,
    Up,
    Repeat,
}

/// `key <virtual key code> <down|up|repeat> <modifiers>`, the modifiers as `shift+control`,
/// `option` (no side), `loption` or `roption` (with the left or right device bit) or
/// `command`, or `-` for none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostedKey {
    code: u16,
    phase: KeyPhase,
    flags: u64,
}

impl PostedKey {
    pub(super) fn parse(fields: &str) -> Option<Self> {
        let mut fields = fields.split(' ');
        let code = fields.next()?.parse().ok()?;
        let phase = match fields.next()? {
            "down" => KeyPhase::Down,
            "up" => KeyPhase::Up,
            "repeat" => KeyPhase::Repeat,
            _ => return None,
        };
        let flags = modifier_flags(fields.next()?)?;
        fields
            .next()
            .is_none()
            .then_some(Self { code, phase, flags })
    }
}

/// `NX_DEVICELALTKEYMASK` and `NX_DEVICERALTKEYMASK`: the side bits winit reads for
/// `OptionAsAlt`.
const LEFT_OPTION: u64 = 0x20;
const RIGHT_OPTION: u64 = 0x40;

fn modifier_flags(names: &str) -> Option<u64> {
    let mut flags = 0;
    for name in names.split('+').filter(|name| *name != "-") {
        flags |= match name {
            "shift" => CGEventFlags::MaskShift.bits(),
            "control" => CGEventFlags::MaskControl.bits(),
            "option" => CGEventFlags::MaskAlternate.bits(),
            "loption" => CGEventFlags::MaskAlternate.bits() | LEFT_OPTION,
            "roption" => CGEventFlags::MaskAlternate.bits() | RIGHT_OPTION,
            "command" => CGEventFlags::MaskCommand.bits(),
            _ => return None,
        };
    }
    Some(flags)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MousePhase {
    Down,
    Up,
    Drag,
    Move,
}

/// `mouse <down|up|drag|move> <column> <row> <modifiers>`: the left button at the centre of a
/// cell of the focused pane, the modifiers as for `key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostedMouse {
    pub phase: MousePhase,
    pub cell: [u16; 2],
    flags: u64,
}

impl PostedMouse {
    pub(super) fn parse(fields: &str) -> Option<Self> {
        let mut fields = fields.split(' ');
        let phase = match fields.next()? {
            "down" => MousePhase::Down,
            "up" => MousePhase::Up,
            "drag" => MousePhase::Drag,
            "move" => MousePhase::Move,
            _ => return None,
        };
        let cell = [fields.next()?.parse().ok()?, fields.next()?.parse().ok()?];
        let flags = modifier_flags(fields.next()?)?;
        fields
            .next()
            .is_none()
            .then_some(Self { phase, cell, flags })
    }
}

/// With existing permission, post one key event to this PID while the probe window is key; a
/// repeat goes through `AppKit`'s queue instead.
#[must_use]
pub fn post_key(key: PostedKey) -> bool {
    if !CGPreflightPostEventAccess() {
        return false;
    }
    let Some(app) = app() else {
        return false;
    };
    if window(&app).is_none_or(|window| input_state(&app, &window) != (true, true)) {
        return false;
    }
    let (Ok(pid), Some(event)) = (
        i32::try_from(process::id()),
        CGEvent::new_keyboard_event(None, key.code, key.phase != KeyPhase::Up),
    ) else {
        return false;
    };
    if MODIFIER_KEYS.contains(&key.code) {
        CGEvent::set_type(Some(&event), CGEventType::FlagsChanged);
    }
    CGEvent::set_flags(Some(&event), CGEventFlags::from_bits_retain(key.flags));
    if key.phase == KeyPhase::Repeat {
        CGEvent::set_integer_value_field(Some(&event), CGEventField::KeyboardEventAutorepeat, 1);
        // The window server delivers a repeat posted to a PID as nothing; queued in AppKit it
        // reaches the window as `isARepeat`, like a held key's.
        let Some(repeat) = NSEvent::eventWithCGEvent(&event) else {
            return false;
        };
        app.postEvent_atStart(&repeat, false);
        return true;
    }
    CGEvent::post_to_pid(pid, Some(&event));
    true
}

/// Queue one left-button event at `point`, in points from the content view's top left, to
/// the probe window while it is key. `AppKit` delivers it like a real one, through
/// `sendEvent:`, so winit sees its position and modifiers.
#[must_use]
pub fn post_mouse(mouse: PostedMouse, point: [f64; 2]) -> bool {
    let Some(app) = app() else {
        return false;
    };
    let Some(window) = window(&app) else {
        return false;
    };
    let Some(view) = window.contentView() else {
        return false;
    };
    if input_state(&app, &window) != (true, true) {
        return false;
    }
    let location = NSPoint::new(point[0], view.frame().size.height - point[1]);
    let kind = match mouse.phase {
        MousePhase::Down => NSEventType::LeftMouseDown,
        MousePhase::Up => NSEventType::LeftMouseUp,
        MousePhase::Drag => NSEventType::LeftMouseDragged,
        MousePhase::Move => NSEventType::MouseMoved,
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "NSEventModifierFlags keeps the low 32 bits, where the device bits are"
    )]
    let flags = NSEventModifierFlags::from_bits_retain(mouse.flags as usize);
    let Some(event) = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind,
        location,
        flags,
        // sumie times clicks itself; AppKit's timestamp is unused.
        0.0,
        window.windowNumber(),
        None,
        0,
        1,
        if mouse.phase == MousePhase::Up { 0.0 } else { 1.0 },
    ) else {
        return false;
    };
    app.postEvent_atStart(&event, false);
    true
}
