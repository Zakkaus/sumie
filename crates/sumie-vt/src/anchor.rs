//! Owns tracked native grid positions, not terminal text or viewport state.

use std::ffi::{c_int, c_void};
use std::ptr::null_mut;

use crate::{Error, Terminal, check, ffi};

type TrackedGridRef = *mut c_void;

// SAFETY: These declarations match the pinned Ghostty grid and terminal headers.
unsafe extern "C" {
    fn ghostty_terminal_grid_ref_track(
        terminal: ffi::Terminal,
        point: ffi::Point,
        out: *mut TrackedGridRef,
    ) -> c_int;
    fn ghostty_tracked_grid_ref_point(
        reference: TrackedGridRef,
        tag: c_int,
        out: *mut ffi::PointCoordinate,
    ) -> c_int;
    fn ghostty_tracked_grid_ref_free(reference: TrackedGridRef);
}

/// A tracked position on its original terminal screen, owned by the terminal thread.
/// It may outlive the terminal, but no longer has a position when the terminal is dropped.
pub struct GridAnchor(TrackedGridRef);

impl GridAnchor {
    /// Return `(row, column)` on the original screen, including its history.
    /// A discarded position returns `None`. This does not check which screen is active.
    ///
    /// # Errors
    /// Returns an error if Ghostty rejects the position query.
    pub fn position(&self) -> Result<Option<(u32, u16)>, Error> {
        let mut coordinate = ffi::PointCoordinate { x: 0, y: 0 };
        // SAFETY: The anchor owns the reference even after its terminal is freed, and
        // the output slot matches the native coordinate layout.
        let result = unsafe {
            ghostty_tracked_grid_ref_point(self.0, ffi::POINT_TAG_SCREEN, &raw mut coordinate)
        };
        if result == ffi::NO_VALUE {
            return Ok(None);
        }
        check(result)?;
        Ok(Some((coordinate.y, coordinate.x)))
    }
}

impl Drop for GridAnchor {
    fn drop(&mut self) {
        // SAFETY: This handle is uniquely owned and can be freed after its terminal.
        unsafe { ghostty_tracked_grid_ref_free(self.0) };
    }
}

impl Terminal {
    /// Track a zero-based cell in the active screen, counting rows from the oldest history.
    /// Ghostty updates its position through scrolling and reflow.
    ///
    /// # Errors
    /// Returns an error for an invalid cell or a native allocation failure.
    pub fn track_cell(&self, row: u32, col: u16) -> Result<GridAnchor, Error> {
        let point = ffi::Point {
            tag: ffi::POINT_TAG_SCREEN,
            value: ffi::PointValue {
                coordinate: ffi::PointCoordinate { x: col, y: row },
            },
        };
        let mut reference = null_mut();
        // SAFETY: The terminal is live, the point contains the coordinate variant, and
        // Ghostty writes a uniquely owned reference to the output slot on success.
        check(unsafe { ghostty_terminal_grid_ref_track(self.raw, point, &raw mut reference) })?;
        Ok(GridAnchor(reference))
    }
}

#[cfg(test)]
mod tests {
    use crate::Terminal;

    #[test]
    fn tracked_cell_follows_reflow_and_is_lost_when_history_is_erased() {
        let mut terminal = Terminal::new(10, 2).unwrap();
        terminal.write(b"0123456789\r\nL07\r\nL08\r\nL09");
        let anchor = terminal.track_cell(1, 0).unwrap();
        terminal.resize(5, 2).unwrap();
        assert_eq!(anchor.position().unwrap(), Some((2, 0)));
        terminal.restore_viewport(2).unwrap();
        assert_eq!(terminal.snapshot().unwrap()[0][0].text, "L");
        terminal.set_scrollback_bytes(0).unwrap();
        assert_eq!(anchor.position().unwrap(), None);
        let active = terminal.track_cell(0, 0).unwrap();
        drop(terminal);
        assert_eq!(active.position().unwrap(), None);
    }
}
