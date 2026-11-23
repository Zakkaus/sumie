//! Owns the safe `libghostty-vt` wrapper. Does not draw, call platform code, or
//! decide which shortcuts to handle.

mod effects;
mod ffi;
mod identity;
mod key;
mod mouse;
mod selection;

pub use effects::Event;
pub use key::{KeyAction, KeyEncoder, KeyEvent};
pub use mouse::{MouseAction, MouseButton, MouseEncoder, MouseReport};

use effects::Effects;
use std::ffi::{c_int, c_void};
use std::fmt;
use std::ptr::{from_mut, null, null_mut};

/// A cell in the visible grid.
#[derive(Debug, PartialEq, Eq)]
pub struct Cell {
    /// The full grapheme cluster, or empty text for a blank or continuation.
    pub text: String,
    /// Display columns: 1 for a regular cell, 2 for a wide cell, and 0 for its trailing cell.
    pub width: u8,
    pub foreground: Rgb,
    pub background: Rgb,
}

pub type Rgb = [u8; 3];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub column: u16,
    pub row: u16,
    pub visible: bool,
    pub color: Rgb,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub rows: Vec<Vec<Cell>>,
    pub cursor: Option<Cursor>,
    pub foreground: Rgb,
    pub background: Rgb,
}

#[derive(Clone, Copy)]
struct Colors {
    foreground: Rgb,
    background: Rgb,
    cursor: Rgb,
}

/// Width of a Unicode codepoint, before any grapheme clustering.
#[must_use]
pub fn codepoint_width(ch: char) -> u8 {
    // SAFETY: Any Rust char is a valid Unicode scalar value.
    unsafe { ffi::ghostty_unicode_codepoint_width(u32::from(ch)) }
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
    effects: *mut Effects,
}

impl Terminal {
    /// Create a terminal with the given size in cells.
    ///
    /// # Errors
    /// Returns an error for zero dimensions or a native allocation failure.
    pub fn new(cols: u16, rows: u16) -> Result<Self, Error> {
        let mut terminal = Self {
            raw: null_mut(),
            render: null_mut(),
            row_iter: null_mut(),
            cell_iter: null_mut(),
            effects: Box::into_raw(Box::<Effects>::default()),
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
            check(ffi::ghostty_terminal_set(
                terminal.raw,
                ffi::TERMINAL_OPT_USERDATA,
                terminal.effects.cast(),
            ))?;
            for (option, callback) in [
                (
                    ffi::TERMINAL_OPT_WRITE_PTY,
                    effects::queue_reply as *const c_void,
                ),
                (
                    ffi::TERMINAL_OPT_CLIPBOARD_WRITE,
                    effects::queue_clipboard as *const c_void,
                ),
                (
                    ffi::TERMINAL_OPT_CLIPBOARD_READ,
                    effects::deny_clipboard_read as *const c_void,
                ),
                (
                    ffi::TERMINAL_OPT_TITLE_CHANGED,
                    effects::queue_title as *const c_void,
                ),
            ] {
                check(ffi::ghostty_terminal_set(terminal.raw, option, callback))?;
            }
            check(ffi::ghostty_terminal_set(
                terminal.raw,
                ffi::TERMINAL_OPT_CLIPBOARD_WRITE_MAX_BYTES,
                std::ptr::from_ref(&effects::OUTPUT_LIMIT).cast(),
            ))?;
        }
        Ok(terminal)
    }

    /// Feed terminal output bytes, retaining parser state between calls.
    pub fn write(&mut self, bytes: &[u8]) {
        // SAFETY: The terminal is live and exclusive. Ghostty reads the slice during the call.
        unsafe { ffi::ghostty_terminal_vt_write(self.raw, bytes.as_ptr(), bytes.len()) };
    }

    /// Consume protocol replies after writes or resizes, on the terminal owner thread.
    pub fn drain_replies(&mut self, consume: impl FnOnce(&[u8])) {
        let replies = &mut self.effects().replies;
        if !replies.is_empty() {
            consume(replies);
            replies.clear();
        }
    }

    /// Consume queued clipboard writes and the latest title on the terminal owner thread.
    pub fn drain_events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.effects().drain_events()
    }

    fn effects(&mut self) -> &mut Effects {
        // SAFETY: This terminal uniquely owns the box, and no callback can run here.
        unsafe { &mut *self.effects }
    }

    /// Encode a single paste for atomic admission to the PTY queue.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot query mode 2004 or encode the text.
    pub fn encode_paste(&self, mut text: String) -> Result<Vec<u8>, Error> {
        const END: &str = "\x1b[201~";
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let mut mode = ffi::TerminalModeConfig {
            mode: ffi::MODE_BRACKETED_PASTE,
            value: false,
        };
        // SAFETY: DATA_MODE reads and updates this initialized TerminalModeConfig.
        check(unsafe {
            ffi::ghostty_terminal_get(
                self.raw,
                ffi::TERMINAL_DATA_MODE,
                from_mut(&mut mode).cast(),
            )
        })?;
        if mode.value && text.contains(END) {
            text = text.replace(END, "");
        }
        text.retain(|ch| matches!(ch, '\t' | '\n' | '\r') || !ch.is_control());
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let mut data = text.into_bytes();
        let mut out = Vec::with_capacity(data.len() + if mode.value { 2 * END.len() } else { 0 });
        let mut written = 0;
        // SAFETY: The input is writable, and out has room for the payload and both
        // markers. On success the encoder initialized exactly `written` bytes.
        unsafe {
            check(ffi::ghostty_paste_encode(
                data.as_mut_ptr(),
                data.len(),
                mode.value,
                out.as_mut_ptr(),
                out.capacity(),
                &raw mut written,
            ))?;
            out.set_len(written);
        }
        Ok(out)
    }

    /// Set default colors and ANSI palette entries; `None` clears the cursor default.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read or update the palette.
    pub fn set_colors(
        &mut self,
        foreground: Rgb,
        background: Rgb,
        cursor: Option<Rgb>,
        palette: &[Rgb; 16],
    ) -> Result<(), Error> {
        let mut colors = [[0; 3]; 256];
        // SAFETY: Ghostty copies RGB and palette inputs and accepts null for an unset RGB default.
        unsafe {
            check(ffi::ghostty_terminal_get(
                self.raw,
                ffi::TERMINAL_DATA_COLOR_PALETTE,
                from_mut(&mut colors).cast(),
            ))?;
            colors[..16].copy_from_slice(palette);
            for (option, color) in [
                (ffi::TERMINAL_OPT_COLOR_FOREGROUND, Some(foreground)),
                (ffi::TERMINAL_OPT_COLOR_BACKGROUND, Some(background)),
                (ffi::TERMINAL_OPT_COLOR_CURSOR, cursor),
            ] {
                check(ffi::ghostty_terminal_set(
                    self.raw,
                    option,
                    color.as_ref().map_or(null(), std::ptr::from_ref).cast(),
                ))?;
            }
            check(ffi::ghostty_terminal_set(
                self.raw,
                ffi::TERMINAL_OPT_COLOR_PALETTE,
                colors.as_ptr().cast(),
            ))
        }
    }

    /// Resize the grid. Ghostty reflows the primary screen when wraparound is enabled.
    ///
    /// # Errors
    /// Returns an error for zero dimensions or a native allocation failure.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        self.resize_pixels(cols, rows, [0, 0])
    }

    /// Resize the grid and report the device-pixel dimensions of one cell.
    ///
    /// # Errors
    /// Returns an error for zero grid dimensions or a native allocation failure.
    pub fn resize_pixels(
        &mut self,
        cols: u16,
        rows: u16,
        [width, height]: [u32; 2],
    ) -> Result<(), Error> {
        // SAFETY: The terminal is live and exclusive.
        check(unsafe { ffi::ghostty_terminal_resize(self.raw, cols, rows, width, height) })
    }

    /// Copy the visible grid, with one entry per column, top to bottom.
    ///
    /// The result owns its text and can outlive the terminal.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot update or read the render state.
    pub fn snapshot(&mut self) -> Result<Vec<Vec<Cell>>, Error> {
        Ok(self.render_snapshot()?.rows)
    }

    /// Copy visible cells, effective colors and the cursor into owned Rust data.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot update or read the render state.
    pub fn render_snapshot(&mut self) -> Result<Snapshot, Error> {
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
        let colors = self.colors()?;
        let cursor = self.cursor(colors.cursor)?;
        let mut screen = Vec::with_capacity(usize::from(rows));
        // SAFETY: The row iterator was populated above; the render state is not updated in this loop.
        while unsafe { ffi::ghostty_render_state_row_iterator_next(self.row_iter) } {
            screen.push(self.snapshot_row(cols, colors)?);
        }
        Ok(Snapshot {
            rows: screen,
            cursor,
            foreground: colors.foreground,
            background: colors.background,
        })
    }

    fn colors(&self) -> Result<Colors, Error> {
        // SAFETY: All three data keys have the RGB output layout.
        let foreground = unsafe { self.render_value(ffi::RENDER_STATE_DATA_COLOR_FOREGROUND) }?;
        // SAFETY: The key has the RGB output layout.
        let background = unsafe { self.render_value(ffi::RENDER_STATE_DATA_COLOR_BACKGROUND) }?;
        // SAFETY: The key has the RGB output layout, or reports an unset cursor color.
        let cursor = match unsafe { self.render_value(ffi::RENDER_STATE_DATA_COLOR_CURSOR) } {
            Err(Error(ffi::NO_VALUE)) => foreground,
            result => result?,
        };
        Ok(Colors {
            foreground,
            background,
            cursor,
        })
    }

    fn cursor(&self, color: Rgb) -> Result<Option<Cursor>, Error> {
        // SAFETY: Each key is paired with its documented bool or u16 output layout.
        unsafe {
            if !self.render_value::<bool>(ffi::RENDER_STATE_DATA_CURSOR_VIEWPORT_HAS_VALUE)? {
                return Ok(None);
            }
            let column: u16 = self.render_value(ffi::RENDER_STATE_DATA_CURSOR_VIEWPORT_X)?;
            let wide_tail: bool =
                self.render_value(ffi::RENDER_STATE_DATA_CURSOR_VIEWPORT_WIDE_TAIL)?;
            Ok(Some(Cursor {
                column: column - u16::from(wide_tail),
                row: self.render_value(ffi::RENDER_STATE_DATA_CURSOR_VIEWPORT_Y)?,
                visible: self.render_value(ffi::RENDER_STATE_DATA_CURSOR_VISIBLE)?,
                color,
            }))
        }
    }

    unsafe fn render_value<T: Default>(&self, data: c_int) -> Result<T, Error> {
        let mut value = T::default();
        // SAFETY: The caller pairs the data key with its output layout; the state is live.
        check(unsafe {
            ffi::ghostty_render_state_get(self.render, data, from_mut(&mut value).cast())
        })?;
        Ok(value)
    }

    fn snapshot_row(&mut self, cols: u16, colors: Colors) -> Result<Vec<Cell>, Error> {
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
            row.push(self.snapshot_cell(colors)?);
        }
        Ok(row)
    }

    fn snapshot_cell(&self, colors: Colors) -> Result<Cell, Error> {
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
        let mut foreground =
            self.cell_color(ffi::RENDER_STATE_ROW_CELLS_DATA_FG_COLOR, colors.foreground)?;
        let mut background =
            self.cell_color(ffi::RENDER_STATE_ROW_CELLS_DATA_BG_COLOR, colors.background)?;
        let mut style = ffi::Style::default();
        // SAFETY: STYLE writes a 16-byte GhosttyStyle for the current cell.
        check(unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cell_iter,
                ffi::RENDER_STATE_ROW_CELLS_DATA_STYLE,
                from_mut(&mut style).cast(),
            )
        })?;
        if style.flags & ffi::STYLE_INVERSE != 0 {
            std::mem::swap(&mut foreground, &mut background);
        }
        let text = if style.flags & ffi::STYLE_INVISIBLE == 0 {
            self.cell_text()?
        } else {
            String::new()
        };
        Ok(Cell {
            text,
            width,
            foreground,
            background,
        })
    }

    fn cell_color(&self, data: c_int, default: Rgb) -> Result<Rgb, Error> {
        let mut rgb = default;
        // SAFETY: Both color keys used here write exactly one RGB value for the current cell.
        let result = unsafe {
            ffi::ghostty_render_state_row_cells_get(self.cell_iter, data, rgb.as_mut_ptr().cast())
        };
        if result != ffi::NO_VALUE {
            check(result)?;
        }
        Ok(rgb)
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
            drop(Box::from_raw(self.effects));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Terminal;

    #[test]
    fn paste_filters_controls_in_both_modes() {
        let mut terminal = Terminal::new(5, 2).unwrap();
        for (mode, input, expected) in [
            (
                "\x1b[?2004h",
                "a\x00\x01\x1b[201~\u{7f}\u{80}\u{9f}\t\nb\x1b中",
                "\x1b[200~a\t\nb中\x1b[201~",
            ),
            ("\x1b[?2004h", "\x01\x1b[201~\u{9b}", ""),
            (
                "\x1b[?2004l",
                "a\x00\x01\x1b\u{7f}\u{80}\u{9f}\t\rb\n中",
                "a\t\rb\r中",
            ),
        ] {
            terminal.write(mode.as_bytes());
            assert_eq!(
                terminal.encode_paste(input.into()).unwrap(),
                expected.as_bytes()
            );
        }
    }

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
