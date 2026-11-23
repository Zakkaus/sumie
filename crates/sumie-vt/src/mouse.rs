use std::ffi::{c_int, c_void};
use std::mem::size_of;
use std::ptr::{from_mut, from_ref, null, null_mut};

use crate::{Error, Terminal, check, ffi};

const TERMINAL_DATA_ACTIVE_SCREEN: c_int = 6;
const TERMINAL_DATA_SCROLLBAR: c_int = 9;
const TERMINAL_DATA_MOUSE_TRACKING: c_int = 11;
const TERMINAL_OPT_SCROLLBACK_MAX_BYTES: c_int = 28;
const SCREEN_ALTERNATE: c_int = 1;
const MODE_ALTERNATE_SCROLL: u16 = 1007;
const SCROLL_BOTTOM: c_int = 1;
const SCROLL_DELTA: c_int = 2;
const SCROLL_ROW: c_int = 3;
const MOUSE_OPT_SIZE: c_int = 3;
const MOUSE_OPT_ANY_BUTTON_PRESSED: c_int = 4;
const MOUSE_OPT_TRACK_LAST_CELL: c_int = 5;
const MOUSE_BUFFER_SIZE: usize = 128;

type MouseHandle = *mut c_void;

#[repr(C)]
struct Scrollbar {
    total: u64,
    offset: u64,
    len: u64,
}

#[repr(C)]
union ScrollValue {
    delta: isize,
    row: usize,
    padding: [u64; 2],
}

#[repr(C)]
struct ScrollViewport {
    tag: c_int,
    value: ScrollValue,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct MouseSize {
    size: usize,
    screen_width: u32,
    screen_height: u32,
    cell_width: u32,
    cell_height: u32,
    padding_top: u32,
    padding_bottom: u32,
    padding_right: u32,
    padding_left: u32,
}

#[repr(C)]
struct MousePosition {
    x: f32,
    y: f32,
}

// SAFETY: These declarations match the pinned Ghostty terminal and mouse C headers.
unsafe extern "C" {
    fn ghostty_terminal_scroll_viewport(terminal: ffi::Terminal, viewport: ScrollViewport)
    -> c_int;
    fn ghostty_mouse_encoder_new(allocator: *const ffi::Allocator, out: *mut MouseHandle) -> c_int;
    fn ghostty_mouse_encoder_free(encoder: MouseHandle);
    fn ghostty_mouse_encoder_set(
        encoder: MouseHandle,
        option: c_int,
        value: *const c_void,
    ) -> c_int;
    fn ghostty_mouse_encoder_set_from_terminal(
        encoder: MouseHandle,
        terminal: ffi::Terminal,
    ) -> c_int;
    fn ghostty_mouse_encoder_encode(
        encoder: MouseHandle,
        event: MouseHandle,
        out: *mut u8,
        len: usize,
        written: *mut usize,
    ) -> c_int;
    fn ghostty_mouse_event_new(allocator: *const ffi::Allocator, out: *mut MouseHandle) -> c_int;
    fn ghostty_mouse_event_free(event: MouseHandle);
    fn ghostty_mouse_event_set_action(event: MouseHandle, action: c_int) -> c_int;
    fn ghostty_mouse_event_set_button(event: MouseHandle, button: c_int) -> c_int;
    fn ghostty_mouse_event_clear_button(event: MouseHandle) -> c_int;
    fn ghostty_mouse_event_set_mods(event: MouseHandle, modifiers: u16) -> c_int;
    fn ghostty_mouse_event_set_position(event: MouseHandle, position: MousePosition) -> c_int;
}

impl Terminal {
    /// Return (mouse tracking, alternate screen active, DEC mode 1007).
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read a mode or the active screen.
    pub fn mouse_modes(&self) -> Result<(bool, bool, bool), Error> {
        let mut tracking = false;
        let mut screen: c_int = 0;
        let mut alternate_scroll = ffi::TerminalModeConfig {
            mode: MODE_ALTERNATE_SCROLL,
            value: false,
        };
        // SAFETY: The terminal is live, and each data key matches its output slot type.
        unsafe {
            check(ffi::ghostty_terminal_get(
                self.raw,
                TERMINAL_DATA_MOUSE_TRACKING,
                from_mut(&mut tracking).cast(),
            ))?;
            check(ffi::ghostty_terminal_get(
                self.raw,
                TERMINAL_DATA_ACTIVE_SCREEN,
                from_mut(&mut screen).cast(),
            ))?;
            check(ffi::ghostty_terminal_get(
                self.raw,
                ffi::TERMINAL_DATA_MODE,
                from_mut(&mut alternate_scroll).cast(),
            ))?;
        }
        Ok((tracking, screen == SCREEN_ALTERNATE, alternate_scroll.value))
    }

    /// Move the viewport by rows (positive is down). New output does not unpin scrollback.
    ///
    /// # Errors
    /// Returns an error if Ghostty rejects the scroll request.
    pub fn scroll_rows(&mut self, delta: i32) -> Result<(), Error> {
        // SAFETY: The terminal is live and the delta tag matches the initialized union field.
        check(unsafe {
            ghostty_terminal_scroll_viewport(
                self.raw,
                ScrollViewport {
                    tag: SCROLL_DELTA,
                    value: ScrollValue {
                        delta: delta as isize,
                    },
                },
            )
        })
    }

    /// Return the viewport to the active area so it follows new output.
    ///
    /// # Errors
    /// Returns an error if Ghostty rejects the scroll request.
    pub fn scroll_bottom(&mut self) -> Result<(), Error> {
        // SAFETY: The terminal is live, and the bottom tag ignores the union value.
        check(unsafe {
            ghostty_terminal_scroll_viewport(
                self.raw,
                ScrollViewport {
                    tag: SCROLL_BOTTOM,
                    value: ScrollValue { padding: [0; 2] },
                },
            )
        })
    }

    /// Set the primary screen's approximate scrollback byte limit. Zero erases and disables
    /// history; `usize::MAX` removes the byte limit. Ghostty prunes whole pages.
    ///
    /// # Errors
    /// Returns an error if Ghostty rejects the option.
    pub fn set_scrollback_bytes(&mut self, bytes: usize) -> Result<(), Error> {
        // SAFETY: The terminal is live and this option reads a size_t during the call.
        check(unsafe {
            ffi::ghostty_terminal_set(
                self.raw,
                TERMINAL_OPT_SCROLLBACK_MAX_BYTES,
                from_ref(&bytes).cast(),
            )
        })
    }

    /// Return whether the viewport is at the active area, including screens without scrollback.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the scrollbar.
    pub fn at_bottom(&self) -> Result<bool, Error> {
        let scrollbar = self.scrollbar()?;
        Ok(scrollbar.offset == scrollbar.total - scrollbar.len)
    }

    /// Return the first visible row in the active screen's scrollback coordinates.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the scrollbar or the offset exceeds `u32`.
    pub fn viewport_top(&self) -> Result<u32, Error> {
        u32::try_from(self.scrollbar()?.offset).map_err(|_| Error(ffi::INVALID_VALUE))
    }

    /// Scroll a search-result row into view if it is not already visible.
    ///
    /// # Errors
    /// Returns an error if the row is outside the screen or Ghostty rejects the scroll.
    pub fn reveal_search(&mut self, row: u32) -> Result<(), Error> {
        let scrollbar = self.scrollbar()?;
        let target = u64::from(row);
        if target >= scrollbar.total {
            return Err(Error(ffi::INVALID_VALUE));
        }
        if (scrollbar.offset..scrollbar.offset + scrollbar.len).contains(&target) {
            return Ok(());
        }
        let row = usize::try_from(row).map_err(|_| Error(ffi::INVALID_VALUE))?;
        // SAFETY: The terminal is live and the absolute-row tag matches the union field.
        check(unsafe {
            ghostty_terminal_scroll_viewport(
                self.raw,
                ScrollViewport {
                    tag: SCROLL_ROW,
                    value: ScrollValue { row },
                },
            )
        })
    }

    fn scrollbar(&self) -> Result<Scrollbar, Error> {
        let mut scrollbar = Scrollbar {
            total: 0,
            offset: 0,
            len: 0,
        };
        // SAFETY: The terminal is live and SCROLLBAR writes the initialized scrollbar slot.
        check(unsafe {
            ffi::ghostty_terminal_get(
                self.raw,
                TERMINAL_DATA_SCROLLBAR,
                from_mut(&mut scrollbar).cast(),
            )
        })?;
        Ok(scrollbar)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum MouseAction {
    Press = 0,
    Release = 1,
    Motion = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum MouseButton {
    Left = 1,
    Right = 2,
    Middle = 3,
    WheelUp = 4,
    WheelDown = 5,
}

#[derive(Clone, Copy, Debug)]
pub struct MouseReport {
    pub action: MouseAction,
    pub button: Option<MouseButton>,
    /// Device pixels relative to the grid origin. Negative coordinates are outside the grid.
    pub position: [f32; 2],
    /// Cell width and height in device pixels.
    pub cell: [u32; 2],
    /// Grid columns and rows in cells.
    pub grid: [u32; 2],
    /// A `GhosttyMods` bitmask, as in [`crate::KeyEvent`].
    pub modifiers: u16,
    /// Whether any mouse button is down after applying this event.
    pub any_button: bool,
}

/// Reusable native mouse encoder. Keep one per terminal on its owner thread.
pub struct MouseEncoder {
    raw: MouseHandle,
    event: MouseHandle,
    size: Option<MouseSize>,
    bytes: [u8; MOUSE_BUFFER_SIZE],
}

impl MouseEncoder {
    /// # Errors
    /// Returns an error if Ghostty cannot allocate or configure the encoder or event.
    pub fn new() -> Result<Self, Error> {
        let mut encoder = Self {
            raw: null_mut(),
            event: null_mut(),
            size: None,
            bytes: [0; MOUSE_BUFFER_SIZE],
        };
        // SAFETY: Constructors accept the default allocator and write their handle slots.
        // The tracking option reads a bool from a live slot during the call.
        unsafe {
            check(ghostty_mouse_encoder_new(null(), &raw mut encoder.raw))?;
            check(ghostty_mouse_event_new(null(), &raw mut encoder.event))?;
            check(ghostty_mouse_encoder_set(
                encoder.raw,
                MOUSE_OPT_TRACK_LAST_CELL,
                from_ref(&true).cast(),
            ))?;
        }
        Ok(encoder)
    }

    /// Copy the terminal's tracking mode and format, resetting motion deduplication.
    /// Call before encoding input and after processing terminal output.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot update the encoder.
    pub fn sync(&mut self, terminal: &Terminal) -> Result<(), Error> {
        // SAFETY: Both handles are live, and Ghostty copies the mode values.
        check(unsafe { ghostty_mouse_encoder_set_from_terminal(self.raw, terminal.raw) })
    }

    /// Encode using the modes from the last [`Self::sync`]. An empty slice means Ghostty
    /// did not emit a report.
    ///
    /// # Errors
    /// Returns an error if the geometry or position is not representable in Ghostty, or if
    /// native encoding fails.
    pub fn encode(&mut self, report: MouseReport) -> Result<&[u8], Error> {
        let [x, y] = report.position;
        let [cell_width, cell_height] = report.cell;
        let [cols, rows] = report.grid;
        let size = MouseSize {
            size: size_of::<MouseSize>(),
            screen_width: screen_dimension(cell_width, cols, x)?,
            screen_height: screen_dimension(cell_height, rows, y)?,
            cell_width,
            cell_height,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        };
        let mut written = 0;
        // SAFETY: The handles are live and both options copy values from matching C layouts.
        // The geometry and position are in range, and the entire output buffer is writable.
        unsafe {
            if self.size != Some(size) {
                check(ghostty_mouse_encoder_set(
                    self.raw,
                    MOUSE_OPT_SIZE,
                    from_ref(&size).cast(),
                ))?;
                self.size = Some(size);
            }
            check(ghostty_mouse_encoder_set(
                self.raw,
                MOUSE_OPT_ANY_BUTTON_PRESSED,
                from_ref(&report.any_button).cast(),
            ))?;
            check(ghostty_mouse_event_set_action(
                self.event,
                report.action as c_int,
            ))?;
            check(match report.button {
                Some(button) => ghostty_mouse_event_set_button(self.event, button as c_int),
                None => ghostty_mouse_event_clear_button(self.event),
            })?;
            check(ghostty_mouse_event_set_mods(self.event, report.modifiers))?;
            check(ghostty_mouse_event_set_position(
                self.event,
                MousePosition { x, y },
            ))?;
            check(ghostty_mouse_encoder_encode(
                self.raw,
                self.event,
                self.bytes.as_mut_ptr(),
                self.bytes.len(),
                &raw mut written,
            ))?;
        }
        Ok(&self.bytes[..written])
    }
}

impl Drop for MouseEncoder {
    fn drop(&mut self) {
        // SAFETY: Both handles are uniquely owned or null after partial initialization.
        unsafe {
            ghostty_mouse_event_free(self.event);
            ghostty_mouse_encoder_free(self.raw);
        }
    }
}

fn screen_dimension(cell: u32, grid: u32, position: f32) -> Result<u32, Error> {
    let invalid = Error(ffi::INVALID_VALUE);
    // Ghostty derives u16 grid dimensions through f32 division. Keep the inputs exactly
    // representable so they round-trip to the requested grid.
    let screen = cell
        .checked_mul(grid)
        .filter(|size| (1..=(1 << 24)).contains(size))
        .ok_or(invalid)?;
    u16::try_from(grid).map_err(|_| invalid)?;

    let position = f64::from(position);
    // Ghostty converts pixels to i32, and converts to u16 cells before clamping to the grid.
    if !(f64::from(i32::MIN)..f64::from(i32::MAX) + 1.0).contains(&position) {
        return Err(invalid);
    }
    let index = (position.max(0.0) / f64::from(cell)).trunc();
    if index > f64::from(u16::MAX) {
        return Err(invalid);
    }
    // The UTF-8 wire format adds 33 to each cell index and assumes a valid Unicode scalar.
    let codepoint = index.min(f64::from(grid - 1)) + 33.0;
    if (55_296.0..=57_343.0).contains(&codepoint) {
        return Err(invalid);
    }
    Ok(screen)
}
