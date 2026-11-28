//! Owns the safe `libghostty-vt` wrapper. Does not draw, call platform code, or
//! decide which shortcuts to handle.

mod anchor;
mod cursor;
mod effects;
mod ffi;
mod graphics;
mod identity;
mod key;
mod mouse;
mod reports;
mod search;
mod selection;
mod shell;
mod style;
pub use anchor::GridAnchor;
pub use cursor::{Cursor, CursorStyle};
pub use effects::Event;
pub use key::{KeyAction, KeyEncoder, KeyEvent};
pub use mouse::{MouseAction, MouseButton, MouseEncoder, MouseReport};
pub use search::{Search, SearchMatch};
pub use style::{CellStyle, Underline};

use effects::Effects;
use graphics::Graphics;
use std::ffi::c_int;
use std::fmt;
use std::ptr::{from_mut, null, null_mut};
use sumie_core::graphics::Placement;

/// A cell in the visible grid.
#[derive(Debug, PartialEq, Eq)]
pub struct Cell {
    /// The full grapheme cluster, or empty text for a blank or continuation.
    pub text: String,
    /// Display columns: 1 for a regular cell or wrap spacer, 2 for a wide cell, and 0 for its tail.
    pub width: u8,
    /// Empty cell at the end of a row where a wide glyph wrapped to the next row.
    pub wide_wrap_spacer: bool,
    pub foreground: Rgb,
    pub background: Rgb,
    /// Whether the cell uses the effective terminal background, including screen-wide reverse video.
    /// An explicit background or cell-level reverse video does not qualify, even at the same RGB.
    pub default_background: bool,
    pub style: CellStyle,
}

pub type Rgb = [u8; 3];

#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub rows: Vec<Vec<Cell>>,
    /// The row below the viewport, shown while a smooth scroll sits between rows; none at the bottom.
    pub below: Option<Vec<Cell>>,
    /// Drawable placements, excluding images hidden by an opaque cover of the same footprint.
    pub images: std::sync::Arc<Vec<Placement>>,
    pub cursor: Option<Cursor>,
    pub foreground: Rgb,
    pub background: Rgb,
    /// Effective ANSI yellow (palette entry 3), including any OSC 4 override.
    pub ansi_yellow: Rgb,
    /// Effective ANSI red (palette entry 1), including any OSC 4 override.
    pub ansi_red: Rgb,
}

/// Viewport contents for change detection, excluding cursor and terminal metadata.
#[derive(PartialEq, Eq)]
pub struct ScreenContents {
    grid: u64,
    images: Option<std::sync::Arc<Vec<Placement>>>,
}

struct Colors {
    foreground: Rgb,
    background: Rgb,
    cursor: Rgb,
    palette: Option<[Rgb; 256]>,
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
    graphics: Graphics,
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
            graphics: Graphics::new()?,
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
            (*terminal.effects).render = terminal.render;
            check(ffi::ghostty_terminal_set(
                terminal.raw,
                ffi::TERMINAL_OPT_USERDATA,
                terminal.effects.cast(),
            ))?;
            check(effects::register(terminal.raw))?;
            check(reports::register(terminal.raw))?;
            check(ffi::ghostty_terminal_set(
                terminal.raw,
                ffi::TERMINAL_OPT_CLIPBOARD_WRITE_MAX_BYTES,
                std::ptr::from_ref(&effects::OUTPUT_LIMIT).cast(),
            ))?;
        }
        terminal.configure_graphics()?;
        terminal.request_row_below()?;
        Ok(terminal)
    }

    /// Feed terminal output bytes, retaining parser state between calls.
    pub fn write(&mut self, bytes: &[u8]) {
        // SAFETY: The terminal is live and exclusive. Ghostty reads the slice during the call.
        unsafe { ffi::ghostty_terminal_vt_write(self.raw, bytes.as_ptr(), bytes.len()) };
    }

    /// Fingerprint the cells in place and share any visible image placements.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot update or read visible image placements.
    pub fn screen_contents(&mut self) -> Result<ScreenContents, Error> {
        let mut has_placements = false;
        // SAFETY: The terminal is live and exclusive; the bridge only reads its grid.
        let grid = unsafe { ffi::sumie_screen_stamp(self.raw, &raw mut has_placements) };
        let images = if has_placements {
            self.update_render()?;
            let images = self.snapshot_images()?;
            (!images.is_empty()).then_some(images)
        } else {
            None
        };
        Ok(ScreenContents { grid, images })
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
        let bracketed = self.mode(ffi::MODE_BRACKETED_PASTE)?;
        if bracketed && text.contains(END) {
            text = text.replace(END, "");
        }
        text.retain(|ch| matches!(ch, '\t' | '\n' | '\r') || !ch.is_control());
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let mut data = text.into_bytes();
        let mut out = Vec::with_capacity(data.len() + if bracketed { 2 * END.len() } else { 0 });
        let mut written = 0;
        // SAFETY: The input is writable, and out has room for the payload and both
        // markers. On success the encoder initialized exactly `written` bytes.
        unsafe {
            check(ffi::ghostty_paste_encode(
                data.as_mut_ptr(),
                data.len(),
                bracketed,
                out.as_mut_ptr(),
                out.capacity(),
                &raw mut written,
            ))?;
            out.set_len(written);
        }
        Ok(out)
    }

    /// The focus report for a window focus change, empty unless mode 1004 is set.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot query mode 1004 or encode the report.
    pub fn encode_focus(&self, focused: bool) -> Result<Vec<u8>, Error> {
        if !self.mode(ffi::MODE_FOCUS_EVENT)? {
            return Ok(Vec::new());
        }
        let mut out = [0; 8];
        let mut written = 0;
        let event = if focused {
            ffi::FOCUS_GAINED
        } else {
            ffi::FOCUS_LOST
        };
        // SAFETY: out is writable for its full length; on success Ghostty initialized
        // `written` bytes of it.
        check(unsafe {
            ffi::ghostty_focus_encode(event, out.as_mut_ptr(), out.len(), &raw mut written)
        })?;
        Ok(out[..written].to_vec())
    }

    fn mode(&self, mode: u16) -> Result<bool, Error> {
        let mut mode = ffi::TerminalModeConfig { mode, value: false };
        // SAFETY: DATA_MODE reads and updates this initialized TerminalModeConfig.
        check(unsafe {
            ffi::ghostty_terminal_get(
                self.raw,
                ffi::TERMINAL_DATA_MODE,
                from_mut(&mut mode).cast(),
            )
        })?;
        Ok(mode.value)
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
    /// Also returns an error if Ghostty cannot read or update the viewport.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        self.resize_pixels(cols, rows, [0, 0])
    }

    /// Resize the grid and report the device-pixel dimensions of one cell.
    ///
    /// # Errors
    /// Returns an error for zero grid dimensions or a native allocation failure.
    /// Also returns an error if Ghostty cannot read or update the viewport.
    pub fn resize_pixels(
        &mut self,
        cols: u16,
        rows: u16,
        [width, height]: [u32; 2],
    ) -> Result<(), Error> {
        // SAFETY: The terminal is live and exclusive.
        check(unsafe { ffi::ghostty_terminal_resize(self.raw, cols, rows, width, height) })?;
        // Ghostty can retain a history anchor when the larger grid fits all rows.
        if self.at_bottom()? {
            self.scroll_bottom()?;
        }
        Ok(())
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

    /// Copy visible cells, image placements, effective colors and the cursor into owned Rust data.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot update or read the render state.
    pub fn render_snapshot(&mut self) -> Result<Snapshot, Error> {
        const ANSI_RED: u8 = 1;
        const ANSI_YELLOW: u8 = 3;

        let mut cols = 0_u16;
        let mut rows = 0_u16;
        self.update_render()?;
        // SAFETY: Both handles are live and exclusive. Each key matches the output slot type.
        unsafe {
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
        let images = self.snapshot_images()?;
        let mut colors = self.colors()?;
        let cursor = self.cursor(colors.cursor)?;
        let mut screen = Vec::with_capacity(usize::from(rows));
        // SAFETY: The row iterator was populated above; the render state is not updated in this loop.
        while unsafe { ffi::ghostty_render_state_row_iterator_next(self.row_iter) } {
            screen.push(self.snapshot_row(cols, &mut colors)?);
        }
        let below = screen.split_off(usize::from(rows).min(screen.len())).pop();
        Ok(Snapshot {
            rows: screen,
            below,
            images,
            cursor,
            foreground: colors.foreground,
            background: colors.background,
            ansi_yellow: self.palette_entry(ANSI_YELLOW, colors.palette.as_ref())?,
            ansi_red: self.palette_entry(ANSI_RED, colors.palette.as_ref())?,
        })
    }

    fn snapshot_row(&mut self, cols: u16, colors: &mut Colors) -> Result<Vec<Cell>, Error> {
        let row_may_have_hyperlinks = self.row_may_have_hyperlinks()?;
        // SAFETY: The row iterator is positioned on a live row; CELLS fills the existing iterator.
        unsafe {
            check(ffi::ghostty_render_state_row_get(
                self.row_iter,
                ffi::RENDER_STATE_ROW_DATA_CELLS,
                from_mut(&mut self.cell_iter).cast(),
            ))?;
        }
        let mut row = Vec::with_capacity(usize::from(cols));
        // SAFETY: The cell iterator was populated above and does not outlive the captured row.
        while unsafe { ffi::ghostty_render_state_row_cells_next(self.cell_iter) } {
            row.push(self.snapshot_cell(colors, row_may_have_hyperlinks)?);
        }
        Ok(row)
    }

    fn snapshot_cell(
        &self,
        colors: &mut Colors,
        row_may_have_hyperlinks: bool,
    ) -> Result<Cell, Error> {
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
        let mut foreground = self
            .cell_color(ffi::RENDER_STATE_ROW_CELLS_DATA_FG_COLOR)?
            .unwrap_or(colors.foreground);
        let cell_background = self.cell_color(ffi::RENDER_STATE_ROW_CELLS_DATA_BG_COLOR)?;
        let mut background = cell_background.unwrap_or(colors.background);
        let mut style = ffi::Style::default();
        // SAFETY: STYLE writes a 16-byte GhosttyStyle for the current cell.
        check(unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cell_iter,
                ffi::RENDER_STATE_ROW_CELLS_DATA_STYLE,
                from_mut(&mut style).cast(),
            )
        })?;
        let inverse = style.flags & ffi::STYLE_INVERSE != 0;
        if inverse {
            std::mem::swap(&mut foreground, &mut background);
        }
        let is_spacer = matches!(
            wide,
            ffi::CELL_WIDE_SPACER_HEAD | ffi::CELL_WIDE_SPACER_TAIL
        );
        let text = if !is_spacer && style.flags & ffi::STYLE_INVISIBLE == 0 {
            self.cell_text(cell)?
        } else {
            String::new()
        };
        Ok(Cell {
            text,
            width,
            wide_wrap_spacer: wide == ffi::CELL_WIDE_SPACER_HEAD,
            foreground,
            background,
            default_background: cell_background.is_none() && !inverse,
            // Ghostty copies the current SGR onto spacers, but only the wide cell draws ink.
            style: if is_spacer {
                CellStyle::default()
            } else {
                self.cell_style(cell, row_may_have_hyperlinks, &style, &mut colors.palette)?
            },
        })
    }

    fn cell_color(&self, data: c_int) -> Result<Option<Rgb>, Error> {
        let mut rgb = [0; 3];
        // SAFETY: Both color keys used here write exactly one RGB value for the current cell.
        let result = unsafe {
            ffi::ghostty_render_state_row_cells_get(self.cell_iter, data, rgb.as_mut_ptr().cast())
        };
        if result == ffi::NO_VALUE {
            return Ok(None);
        }
        check(result)?;
        Ok(Some(rgb))
    }

    fn cell_text(&self, cell: ffi::Cell) -> Result<String, Error> {
        let mut stack = [0; 32];
        let mut buffer = ffi::Buffer {
            ptr: stack.as_mut_ptr(),
            cap: stack.len(),
            len: 0,
        };
        let result = self.read_cell_text(&mut buffer);
        if result == ffi::OUT_OF_SPACE {
            let mut codepoint = 0_u32;
            // SAFETY: CODEPOINT writes a u32 for the raw cell captured by the row iterator.
            check(unsafe {
                ffi::ghostty_cell_get(
                    cell,
                    ffi::CELL_DATA_CODEPOINT,
                    from_mut(&mut codepoint).cast(),
                )
            })?;
            if codepoint == u32::from(graphics::PLACEHOLDER) {
                return Ok(String::new());
            }
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
        let text = unsafe { std::str::from_utf8_unchecked(&stack[..buffer.len]) };
        if text.starts_with(graphics::PLACEHOLDER) {
            return Ok(String::new());
        }
        Ok(text.to_owned())
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
mod tests;
