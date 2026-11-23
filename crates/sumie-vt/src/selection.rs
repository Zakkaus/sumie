use std::ffi::{c_int, c_void};
use std::ptr::{from_mut, from_ref, null, null_mut};
use std::sync::LazyLock;

use sumie_core::{CellPosition, CellRange, SelectionPoint, mouse::SelectionUnit};

use crate::{Error, Terminal, check, ffi};

static LINKS_READY: LazyLock<bool> = LazyLock::new(|| {
    // SAFETY: LazyLock initializes the process-wide native matcher once.
    unsafe { ffi::sumie_selection_init_links() }
});

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

unsafe extern "C" fn compare(data: *mut c_void, bytes: *const u8, len: usize) -> bool {
    // SAFETY: The synchronous formatter owns the live chunk and exclusively borrows
    // the remaining slice reference for this call.
    unsafe {
        let remaining = &mut *data.cast::<Option<&[u8]>>();
        *remaining = remaining.and_then(|s| s.strip_prefix(std::slice::from_raw_parts(bytes, len)));
    }
    true
}

fn cell_wide(reference: &ffi::GridRef) -> Result<c_int, Error> {
    let mut cell = 0;
    let mut wide = 0;
    // SAFETY: The reference was returned by Ghostty and the terminal has not been mutated.
    // CELL_DATA_WIDE writes a C enum.
    unsafe {
        check(ffi::ghostty_grid_ref_cell(reference, &raw mut cell))?;
        check(ffi::ghostty_cell_get(
            cell,
            ffi::CELL_DATA_WIDE,
            from_mut(&mut wide).cast(),
        ))?;
    }
    Ok(wide)
}

impl Terminal {
    fn grid_ref(&self, tag: c_int, x: u16, y: u32) -> Result<ffi::GridRef, Error> {
        let point = ffi::Point {
            tag,
            value: ffi::PointValue {
                coordinate: ffi::PointCoordinate { x, y },
            },
        };
        let mut cell = ffi::GridRef::default();
        // SAFETY: The point contains the coordinate variant; the terminal and output are live.
        check(unsafe { ffi::ghostty_terminal_grid_ref(self.raw, point, &raw mut cell) })?;
        Ok(cell)
    }

    fn point(&self, reference: &ffi::GridRef, tag: c_int) -> Result<ffi::PointCoordinate, Error> {
        let mut coord = ffi::PointCoordinate { x: 0, y: 0 };
        // SAFETY: The reference belongs to the live, immutably borrowed terminal.
        check(unsafe {
            ffi::ghostty_terminal_point_from_grid_ref(self.raw, reference, tag, &raw mut coord)
        })?;
        Ok(coord)
    }

    fn selection_point(&self, reference: &ffi::GridRef) -> Result<SelectionPoint, Error> {
        match self.point(reference, ffi::POINT_TAG_VIEWPORT) {
            Ok(point) => Ok(SelectionPoint {
                row: point.y.into(),
                column: point.x,
            }),
            Err(Error(ffi::NO_VALUE)) => {
                let point = self.point(reference, ffi::POINT_TAG_SCREEN)?;
                Ok(SelectionPoint {
                    row: i64::from(point.y) - i64::from(self.viewport_top()?),
                    column: point.x,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Resolve a word or logical line with Ghostty's default boundaries.
    /// A word press prefers a full Ghostty URL or path match under `hit`.
    /// Word resolution starts at `hit` and searches towards `toward` through unwritten cells.
    /// `Cell` returns no native range.
    ///
    /// # Errors
    /// Returns an error for an invalid hit or a native selection failure.
    pub fn selection_group(
        &self,
        hit: CellPosition,
        toward: CellPosition,
        unit: SelectionUnit,
    ) -> Result<Option<CellRange>, Error> {
        if unit == SelectionUnit::Cell {
            return Ok(None);
        }
        let start = self.grid_ref(ffi::POINT_TAG_VIEWPORT, hit.column, hit.row.into())?;
        let mut selection = ffi::Selection {
            start: ffi::GridRef::default(),
            end: ffi::GridRef::default(),
            rectangle: false,
        };
        let result = if unit == SelectionUnit::Line {
            self.select_line(start, &mut selection)
        } else if toward == hit && self.select_link(&start, &mut selection) {
            ffi::SUCCESS
        } else {
            let end = if toward == hit {
                start
            } else {
                self.grid_ref(ffi::POINT_TAG_VIEWPORT, toward.column, toward.row.into())?
            };
            // SAFETY: The sized options match the pinned C header and both references
            // belong to this terminal. The output is written synchronously.
            unsafe {
                ffi::ghostty_terminal_select_word_between(
                    self.raw,
                    &ffi::SelectWordBetweenOptions {
                        size: size_of::<ffi::SelectWordBetweenOptions>(),
                        start,
                        end,
                        boundary_codepoints: null(),
                        boundary_codepoints_len: 0,
                    },
                    &raw mut selection,
                )
            }
        };
        if result == ffi::NO_VALUE {
            return Ok(None);
        }
        check(result)?;
        let mut start = self.selection_point(&selection.start)?;
        let mut end = self.selection_point(&selection.end)?;
        if cell_wide(&selection.start)? == ffi::CELL_WIDE_SPACER_TAIL {
            start.column -= 1;
        }
        if cell_wide(&selection.end)? == ffi::CELL_WIDE_WIDE {
            end.column += 1;
        }
        Ok(Some(CellRange { start, end }))
    }

    fn select_link(&self, reference: &ffi::GridRef, selection: &mut ffi::Selection) -> bool {
        // SAFETY: The matcher is initialized, and the grid reference and output stay live.
        *LINKS_READY
            && unsafe { ffi::sumie_selection_link_at(self.raw, reference, selection) }
                == ffi::SUCCESS
    }

    fn select_line(&self, reference: ffi::GridRef, selection: &mut ffi::Selection) -> c_int {
        let mut options = ffi::SelectLineOptions {
            size: size_of::<ffi::SelectLineOptions>(),
            reference,
            whitespace: null(),
            whitespace_len: 0,
            semantic_prompt_boundary: false,
        };
        // SAFETY: The sized options match the pinned C header. The reference and
        // output stay live for each call. A non-null pointer with zero length
        // tells Ghostty to use an empty whitespace set, without reading it.
        unsafe {
            let result = ffi::ghostty_terminal_select_line(self.raw, from_ref(&options), selection);
            if result != ffi::NO_VALUE {
                return result;
            }
            // A line of written spaces can itself wrap.
            options.whitespace = from_ref(&0);
            ffi::ghostty_terminal_select_line(self.raw, from_ref(&options), selection)
        }
    }

    fn selection_ref(&self, point: SelectionPoint) -> Result<ffi::GridRef, Error> {
        let (tag, row) = if point.row < 0 {
            (
                ffi::POINT_TAG_SCREEN,
                i64::from(self.viewport_top()?) + point.row,
            )
        } else {
            (ffi::POINT_TAG_VIEWPORT, point.row)
        };
        let row = u32::try_from(row).map_err(|_| Error(ffi::INVALID_VALUE))?;
        self.grid_ref(tag, point.column, row)
    }

    /// Format an inclusive selection as plain UTF-8, joining soft-wrapped rows.
    ///
    /// # Errors
    /// Returns an error for invalid endpoints or a native formatter failure.
    pub fn selection_text(&self, range: CellRange) -> Result<String, Error> {
        let mut text = String::new();
        self.write_selection_text(range, &mut text)?;
        Ok(text)
    }

    /// Replace `text` with the selected UTF-8, reusing its buffer.
    ///
    /// # Errors
    /// Returns an error for invalid endpoints or a native formatter failure.
    pub fn write_selection_text(&self, range: CellRange, text: &mut String) -> Result<(), Error> {
        let selection = self.selection_refs(range)?;
        let mut bytes = std::mem::take(text).into_bytes();
        bytes.clear();
        self.format_selection(
            &selection,
            ffi::Writer {
                write: append,
                userdata: from_mut(&mut bytes).cast(),
            },
        )?;
        *text = String::from_utf8(bytes).map_err(|_| Error(ffi::INVALID_VALUE))?;
        Ok(())
    }

    /// Compare selected UTF-8 without allocating another copy. Missing endpoints never match.
    ///
    /// # Errors
    /// Returns a native formatter error.
    pub fn selection_text_matches(&self, range: CellRange, text: &str) -> Result<bool, Error> {
        let selection = match self.selection_refs(range) {
            Ok(selection) => selection,
            Err(Error(ffi::INVALID_VALUE)) => return Ok(false),
            Err(error) => return Err(error),
        };
        let mut remaining = Some(text.as_bytes());
        self.format_selection(
            &selection,
            ffi::Writer {
                write: compare,
                userdata: from_mut(&mut remaining).cast(),
            },
        )?;
        Ok(remaining == Some(&[]))
    }

    fn selection_refs(&self, range: CellRange) -> Result<ffi::Selection, Error> {
        Ok(ffi::Selection {
            start: self.selection_ref(range.start)?,
            end: self.selection_ref(range.end)?,
            rectangle: false,
        })
    }

    fn format_selection(
        &self,
        selection: &ffi::Selection,
        writer: ffi::Writer,
    ) -> Result<(), Error> {
        let mut raw = null_mut();
        // SAFETY: The formatter borrows this terminal until the guard drops.
        check(unsafe { ffi::ghostty_formatter_terminal_new(null(), &raw mut raw, self.raw) })?;
        let formatter = Formatter(raw);
        // SAFETY: Option values have their documented types. Neither the grid refs nor
        // the writer escape this immutable borrow. Plain output and trimming are defaults.
        unsafe {
            check(ffi::ghostty_formatter_set(
                formatter.0,
                ffi::FORMATTER_OPT_SELECTION,
                from_ref(selection).cast(),
            ))?;
            check(ffi::ghostty_formatter_set(
                formatter.0,
                ffi::FORMATTER_OPT_UNWRAP,
                from_ref(&true).cast(),
            ))?;
            check(ffi::ghostty_formatter_format(formatter.0, writer))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_selected_viewport_text() {
        let mut terminal = Terminal::new(5, 3).unwrap();
        terminal.write("0界abcd\r\nxy\r\nnext".as_bytes());
        terminal.restore_viewport(1).unwrap();
        let range = CellRange {
            start: SelectionPoint { row: -1, column: 1 },
            end: SelectionPoint { row: 1, column: 4 },
        };
        let text = terminal.selection_text(range).unwrap();
        assert_eq!(text, "界abcd\nxy");
        for (output, expected) in [(b"\x1b[?25l".as_slice(), true), (b"\x1b[2;1HZ", false)] {
            terminal.write(output);
            assert_eq!(
                terminal.selection_text_matches(range, &text).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn selects_words_and_wrapped_lines() {
        use SelectionUnit::{Line, Word};

        let mut terminal = Terminal::new(24, 2).unwrap();
        let line = "tag: 中文 https://ex.io/a?q=1 終";
        terminal.write(line.as_bytes());
        terminal.write(b"\r\nnext");
        let cell = |(row, column)| CellPosition { row, column };
        for (top, hit, toward, unit, end_column, expected) in [
            (0, (0, 6), None, Word, 8, "中文"),
            (0, (0, 12), None, Word, 4, "https://ex.io/a?q=1"),
            (0, (1, 2), None, Word, 4, "https://ex.io/a?q=1"),
            (0, (1, 22), Some((1, 2)), Word, 7, "終"),
            (1, (0, 1), None, Line, 7, line),
        ] {
            terminal.restore_viewport(top).unwrap();
            let range = terminal
                .selection_group(cell(hit), cell(toward.unwrap_or(hit)), unit)
                .unwrap()
                .unwrap();
            assert_eq!(range.end.column, end_column);
            assert_eq!(terminal.selection_text(range).unwrap(), expected);
        }
    }
}
