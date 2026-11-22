//! Owns the safe `libghostty-vt` wrapper. Does not draw, call platform code, or
//! decide which shortcuts to handle.

mod ffi;

use std::ffi::c_int;
use std::fmt;
use std::ptr::{from_mut, null, null_mut};

/// A cell in the visible grid.
#[derive(Debug, PartialEq, Eq)]
pub struct Cell {
    /// The full grapheme cluster, or empty text for a blank or continuation.
    pub text: String,
    /// Display columns: 1 for a regular cell, 2 for a wide cell, and 0 for its trailing cell.
    pub width: u8,
}

/// An error reported by libghostty-vt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error(c_int);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "libghostty-vt returned error {}", self.0)
    }
}

impl std::error::Error for Error {}

fn check(result: c_int) -> Result<(), Error> {
    if result == ffi::SUCCESS {
        Ok(())
    } else {
        Err(Error(result))
    }
}

/// A terminal and its render state, owned by one thread (`!Send`, `!Sync`).
pub struct Terminal {
    raw: ffi::Terminal,
    render: ffi::RenderState,
    row_iter: ffi::RenderStateRowIterator,
    cell_iter: ffi::RenderStateRowCells,
}

impl Terminal {
    /// Create a terminal with the given size in cells and no callbacks.
    ///
    /// # Errors
    /// Returns an error for zero dimensions or a native allocation failure.
    pub fn new(cols: u16, rows: u16) -> Result<Self, Error> {
        let mut terminal = Self {
            raw: null_mut(),
            render: null_mut(),
            row_iter: null_mut(),
            cell_iter: null_mut(),
        };
        // SAFETY: The constructors accept a null allocator and write to the corresponding
        // handle slots. Drop accepts null handles if any constructor fails.
        unsafe {
            check(ffi::ghostty_terminal_new(
                null(),
                &raw mut terminal.raw,
                cols,
                rows,
            ))?;
            check(ffi::ghostty_render_state_new(
                null(),
                &raw mut terminal.render,
            ))?;
            check(ffi::ghostty_render_state_row_iterator_new(
                null(),
                &raw mut terminal.row_iter,
            ))?;
            check(ffi::ghostty_render_state_row_cells_new(
                null(),
                &raw mut terminal.cell_iter,
            ))?;
        }
        Ok(terminal)
    }

    /// Feed terminal output bytes, retaining parser state between calls.
    pub fn write(&mut self, bytes: &[u8]) {
        // SAFETY: The terminal is live and exclusive. Ghostty reads the slice during the call.
        unsafe { ffi::ghostty_terminal_vt_write(self.raw, bytes.as_ptr(), bytes.len()) };
    }

    /// Resize the grid. Ghostty reflows the primary screen when wraparound is enabled.
    ///
    /// # Errors
    /// Returns an error for zero dimensions or a native allocation failure.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        // SAFETY: The terminal is live and exclusive. Pixel dimensions are unused here.
        check(unsafe { ffi::ghostty_terminal_resize(self.raw, cols, rows, 0, 0) })
    }

    /// Copy the visible grid, with one entry per column, top to bottom.
    ///
    /// The result owns its text and can outlive the terminal.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot update or read the render state.
    pub fn snapshot(&mut self) -> Result<Vec<Vec<Cell>>, Error> {
        let mut cols = 0_u16;
        let mut rows = 0_u16;
        // SAFETY: Both handles are live and exclusive. Each key matches the output slot type.
        unsafe {
            check(ffi::ghostty_render_state_update(self.render, self.raw))?;
            check(ffi::ghostty_render_state_get(
                self.render,
                ffi::RENDER_STATE_DATA_COLS,
                from_mut(&mut cols).cast(),
            ))?;
            check(ffi::ghostty_render_state_get(
                self.render,
                ffi::RENDER_STATE_DATA_ROWS,
                from_mut(&mut rows).cast(),
            ))?;
            check(ffi::ghostty_render_state_get(
                self.render,
                ffi::RENDER_STATE_DATA_ROW_ITERATOR,
                from_mut(&mut self.row_iter).cast(),
            ))?;
        }
        let mut screen = Vec::with_capacity(usize::from(rows));
        // SAFETY: The row iterator was populated above; the render state is not updated in this loop.
        while unsafe { ffi::ghostty_render_state_row_iterator_next(self.row_iter) } {
            screen.push(self.snapshot_row(cols)?);
        }
        Ok(screen)
    }

    fn snapshot_row(&mut self, cols: u16) -> Result<Vec<Cell>, Error> {
        // SAFETY: The row iterator is positioned on a live row, and CELLS takes a pointer to
        // a preallocated cell iterator handle.
        check(unsafe {
            ffi::ghostty_render_state_row_get(
                self.row_iter,
                ffi::RENDER_STATE_ROW_DATA_CELLS,
                from_mut(&mut self.cell_iter).cast(),
            )
        })?;
        let mut row = Vec::with_capacity(usize::from(cols));
        // SAFETY: The cell iterator was populated above and does not outlive the captured row.
        while unsafe { ffi::ghostty_render_state_row_cells_next(self.cell_iter) } {
            row.push(self.snapshot_cell()?);
        }
        Ok(row)
    }

    fn snapshot_cell(&self) -> Result<Cell, Error> {
        let mut cell: ffi::Cell = 0;
        let mut wide: c_int = 0;
        // SAFETY: The cell iterator is positioned on a live cell. RAW writes a GhosttyCell;
        // WIDE reads that value and writes a C enum.
        unsafe {
            check(ffi::ghostty_render_state_row_cells_get(
                self.cell_iter,
                ffi::RENDER_STATE_ROW_CELLS_DATA_RAW,
                from_mut(&mut cell).cast(),
            ))?;
            check(ffi::ghostty_cell_get(
                cell,
                ffi::CELL_DATA_WIDE,
                from_mut(&mut wide).cast(),
            ))?;
        }
        let width = match wide {
            ffi::CELL_WIDE_NARROW | ffi::CELL_WIDE_SPACER_HEAD => 1,
            ffi::CELL_WIDE_WIDE => 2,
            ffi::CELL_WIDE_SPACER_TAIL => 0,
            _ => return Err(Error(ffi::INVALID_VALUE)),
        };
        let text = self.cell_text()?;
        Ok(Cell { text, width })
    }

    fn cell_text(&self) -> Result<String, Error> {
        let mut stack = [0; 32];
        let mut buffer = ffi::Buffer {
            ptr: stack.as_mut_ptr(),
            cap: stack.len(),
            len: 0,
        };
        let result = self.read_cell_text(&mut buffer);
        if result == ffi::OUT_OF_SPACE {
            let mut bytes = Vec::with_capacity(buffer.len);
            buffer.ptr = bytes.as_mut_ptr();
            buffer.cap = bytes.capacity();
            check(self.read_cell_text(&mut buffer))?;
            // SAFETY: On success Ghostty initialized exactly len bytes of valid UTF-8 within cap.
            return Ok(unsafe {
                bytes.set_len(buffer.len);
                String::from_utf8_unchecked(bytes)
            });
        }
        check(result)?;
        // SAFETY: On success Ghostty wrote valid UTF-8 into the initialized prefix of stack.
        Ok(unsafe { std::str::from_utf8_unchecked(&stack[..buffer.len]) }.to_owned())
    }

    fn read_cell_text(&self, buffer: &mut ffi::Buffer) -> c_int {
        // SAFETY: The iterator is positioned on a live cell, and Buffer describes writable
        // storage of cap bytes that remains valid for the call.
        unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cell_iter,
                ffi::RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8,
                from_mut(buffer).cast(),
            )
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: Each handle is uniquely owned or null after a partial initialization.
        unsafe {
            ffi::ghostty_render_state_row_cells_free(self.cell_iter);
            ffi::ghostty_render_state_row_iterator_free(self.row_iter);
            ffi::ghostty_render_state_free(self.render);
            ffi::ghostty_terminal_free(self.raw);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Terminal;

    #[test]
    fn snapshot_keeps_wide_cells_whole() {
        let mut terminal = Terminal::new(5, 2).unwrap();
        terminal.write("hello\r\n中文".as_bytes());
        let screen = terminal.snapshot().unwrap();
        let expected = [
            [("h", 1), ("e", 1), ("l", 1), ("l", 1), ("o", 1)],
            [("中", 2), ("", 0), ("文", 2), ("", 0), ("", 1)],
        ];
        assert_eq!(screen.len(), expected.len());
        for (row, expected) in screen.iter().zip(expected) {
            assert_eq!(row.len(), expected.len());
            assert!(
                row.iter()
                    .map(|cell| (cell.text.as_str(), cell.width))
                    .eq(expected),
                "unexpected row: {row:?}"
            );
        }
    }

    #[test]
    fn resize_preserves_content() {
        let mut terminal = Terminal::new(5, 2).unwrap();
        terminal.write(b"hello");
        terminal.resize(8, 3).unwrap();
        let screen = terminal.snapshot().unwrap();
        assert_eq!(screen.len(), 3);
        assert!(screen.iter().all(|row| row.len() == 8));
        assert_eq!(
            screen[0][..5]
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>(),
            "hello"
        );
    }

    #[test]
    fn repeatedly_create_and_drop() {
        for _ in 0..200 {
            let mut terminal = Terminal::new(5, 2).unwrap();
            terminal.write(b"hello");
            terminal.snapshot().unwrap();
        }
    }
}
