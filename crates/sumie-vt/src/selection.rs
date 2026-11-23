use std::ffi::c_void;
use std::ptr::{from_mut, from_ref, null, null_mut};

use crate::{Error, Terminal, check, ffi};

struct Formatter(ffi::Formatter);

impl Drop for Formatter {
    fn drop(&mut self) {
        // SAFETY: This guard uniquely owns the formatter and drops before its terminal.
        unsafe { ffi::ghostty_formatter_free(self.0) };
    }
}

unsafe extern "C" fn append(data: *mut c_void, bytes: *const u8, len: usize) -> bool {
    // SAFETY: The synchronous formatter supplies a non-null initialized chunk. Its
    // userdata is the exclusively borrowed Vec for this call, not the Vec's buffer.
    unsafe { (*data.cast::<Vec<u8>>()).extend_from_slice(std::slice::from_raw_parts(bytes, len)) };
    true
}

impl Terminal {
    fn grid_ref(&self, [x, y]: [u16; 2]) -> Result<ffi::GridRef, Error> {
        let point = ffi::Point {
            tag: ffi::POINT_TAG_VIEWPORT,
            value: ffi::PointValue {
                coordinate: ffi::PointCoordinate { x, y: y.into() },
            },
        };
        let mut cell = ffi::GridRef::default();
        // SAFETY: The point contains the coordinate variant; the terminal and output are live.
        check(unsafe { ffi::ghostty_terminal_grid_ref(self.raw, point, &raw mut cell) })?;
        Ok(cell)
    }

    /// Format the inclusive viewport selection as plain UTF-8, joining soft-wrapped rows.
    /// Coordinates are `[column, row]`, ordered and expanded to whole cell groups.
    ///
    /// # Errors
    /// Returns an error for invalid endpoints or a native formatter failure.
    pub fn selection_text(&self, start: [u16; 2], end: [u16; 2]) -> Result<String, Error> {
        let selection = ffi::Selection {
            start: self.grid_ref(start)?,
            end: self.grid_ref(end)?,
            rectangle: false,
        };
        let mut raw = null_mut();
        // SAFETY: The formatter borrows this terminal until the guard drops.
        check(unsafe { ffi::ghostty_formatter_terminal_new(null(), &raw mut raw, self.raw) })?;
        let formatter = Formatter(raw);
        let mut bytes = Vec::<u8>::new();
        let writer = ffi::Writer {
            write: append,
            userdata: from_mut(&mut bytes).cast(),
        };
        // SAFETY: Option values have their documented types. Neither the grid refs nor
        // the writer escape this immutable borrow. Plain output and trimming are defaults.
        unsafe {
            check(ffi::ghostty_formatter_set(
                formatter.0,
                ffi::FORMATTER_OPT_SELECTION,
                from_ref(&selection).cast(),
            ))?;
            check(ffi::ghostty_formatter_set(
                formatter.0,
                ffi::FORMATTER_OPT_UNWRAP,
                from_ref(&true).cast(),
            ))?;
            check(ffi::ghostty_formatter_format(formatter.0, writer))?;
        }
        String::from_utf8(bytes).map_err(|_| Error(ffi::INVALID_VALUE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_selected_viewport_text() {
        let mut terminal = Terminal::new(5, 3).unwrap();
        terminal.write("0界abcd\r\nxy".as_bytes());
        assert_eq!(
            terminal.selection_text([1, 0], [4, 2]).unwrap(),
            "界abcd\nxy"
        );
    }
}
