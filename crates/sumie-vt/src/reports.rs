//! Answers to XTWINOPS size queries (CSI 14/16/18/19 t) and the color-scheme query
//! (CSI ? 996 n), read from the terminal while Ghostty handles the query.

use std::ffi::{c_int, c_void};
use std::ptr::from_mut;

use crate::{Rgb, ffi};

const TERMINAL_OPT_SIZE: c_int = 7;
const TERMINAL_OPT_COLOR_SCHEME: c_int = 8;
const TERMINAL_DATA_ROWS: c_int = 2;
const TERMINAL_DATA_WIDTH_PX: c_int = 16;
const TERMINAL_DATA_HEIGHT_PX: c_int = 17;
const TERMINAL_DATA_COLOR_BACKGROUND: c_int = 19;
const COLOR_SCHEME_LIGHT: c_int = 0;
const COLOR_SCHEME_DARK: c_int = 1;

/// `GhosttySizeReportSize`.
#[repr(C)]
struct SizeReport {
    rows: u16,
    columns: u16,
    cell_width: u32,
    cell_height: u32,
}

/// Install both callbacks. Returns the first failing result code.
///
/// # Safety
/// `terminal` is live.
pub(super) unsafe fn register(terminal: ffi::Terminal) -> c_int {
    for (option, callback) in [
        (TERMINAL_OPT_SIZE, report_size as *const c_void),
        (
            TERMINAL_OPT_COLOR_SCHEME,
            report_color_scheme as *const c_void,
        ),
    ] {
        // SAFETY: The caller guarantees a live terminal; each callback matches its option's type.
        let result = unsafe { ffi::ghostty_terminal_set(terminal, option, callback) };
        if result != ffi::SUCCESS {
            return result;
        }
    }
    ffi::SUCCESS
}

/// Read one terminal value, or `None` if Ghostty has none.
///
/// # Safety
/// `terminal` is live and `T` is the output type of `data`.
unsafe fn get<T: Default>(terminal: ffi::Terminal, data: c_int) -> Option<T> {
    let mut value = T::default();
    // SAFETY: Guaranteed by the caller.
    let result = unsafe { ffi::ghostty_terminal_get(terminal, data, from_mut(&mut value).cast()) };
    (result == ffi::SUCCESS).then_some(value)
}

/// Report the grid and the cell size last passed to `resize_pixels`.
unsafe extern "C" fn report_size(
    terminal: ffi::Terminal,
    _: *mut c_void,
    out: *mut SizeReport,
) -> bool {
    // SAFETY: Ghostty passes its live terminal, and each key has the output type read.
    let size = unsafe {
        (
            get::<u16>(terminal, ffi::TERMINAL_DATA_COLS),
            get::<u16>(terminal, TERMINAL_DATA_ROWS),
            get::<u32>(terminal, TERMINAL_DATA_WIDTH_PX),
            get::<u32>(terminal, TERMINAL_DATA_HEIGHT_PX),
        )
    };
    let (Some(columns @ 1..), Some(rows @ 1..), Some(width), Some(height)) = size else {
        return false;
    };
    let (cell_width, cell_height) = (width / u32::from(columns), height / u32::from(rows));
    // Without a cell size yet, no reply beats one that claims 0;0.
    if cell_width == 0 || cell_height == 0 {
        return false;
    }
    // SAFETY: Ghostty passes a writable size for the duration of the call.
    unsafe {
        *out = SizeReport {
            rows,
            columns,
            cell_width,
            cell_height,
        };
    }
    true
}

/// Report dark or light from the current background, including an OSC 11 override.
unsafe extern "C" fn report_color_scheme(
    terminal: ffi::Terminal,
    _: *mut c_void,
    out: *mut c_int,
) -> bool {
    // SAFETY: Ghostty passes its live terminal, and the key's output type is an RGB triple.
    let Some(background) = (unsafe { get::<Rgb>(terminal, TERMINAL_DATA_COLOR_BACKGROUND) }) else {
        return false;
    };
    // SAFETY: Ghostty passes a writable scheme for the duration of the call.
    unsafe {
        *out = if is_light(background) {
            COLOR_SCHEME_LIGHT
        } else {
            COLOR_SCHEME_DARK
        };
    }
    true
}

/// The rule the drawn chrome uses: light where black text has at least the contrast of white.
fn is_light(rgb: Rgb) -> bool {
    let [r, g, b] = rgb.map(|channel| {
        let srgb = f64::from(channel) / 255.0;
        if srgb <= 0.04045 {
            srgb / 12.92
        } else {
            ((srgb + 0.055) / 1.055).powf(2.4)
        }
    });
    let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    (luminance + 0.05).powi(2) >= 1.05 * 0.05
}
