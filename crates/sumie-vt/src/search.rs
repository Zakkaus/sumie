//! Scan the active screen in place, including scrollback. Search owns only
//! query state, cell coordinates, and scratch space, not a copy of the grid.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::{Error, Terminal, check, ffi};

const BUDGET: Duration = Duration::from_millis(4);
const CLOCK_INTERVAL: usize = 64;

type Position = (u32, u16);

/// An inclusive match in `(row, column)` coordinates from the oldest retained row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchMatch {
    pub start: (u32, u16),
    pub end: (u32, u16),
}

/// An incremental substring search over one unchanged terminal grid.
///
/// Start a new search after writes or resizes; scrolling does not invalidate a search.
pub struct Search {
    pattern: Pattern,
    grid: Option<GridSize>,
    row: u32,
    col: u16,
    wrapped: bool,
    has_graphemes: bool,
    blanks: VecDeque<BlankRun>,
    pending: Option<Glyph>,
    graphemes: Vec<u32>,
    matches: Vec<SearchMatch>,
}

impl Search {
    /// Create a literal search. A query with uppercase characters is case-sensitive;
    /// otherwise each Unicode scalar is lowercased. An empty query finds nothing.
    #[must_use]
    pub fn new(query: &str) -> Self {
        Self {
            pattern: Pattern::new(query),
            grid: None,
            row: 0,
            col: 0,
            wrapped: false,
            has_graphemes: false,
            blanks: VecDeque::new(),
            pending: None,
            graphemes: Vec::new(),
            matches: Vec::new(),
        }
    }

    /// Scan another time-bounded chunk, returning `true` when all rows are scanned.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the grid or it exceeds the coordinate range.
    pub fn advance(&mut self, terminal: &Terminal) -> Result<bool, Error> {
        if self.pattern.needle.is_empty() {
            return Ok(true);
        }
        let grid = if let Some(grid) = self.grid {
            grid
        } else {
            let grid = GridSize::read(terminal)?;
            self.grid = Some(grid);
            grid
        };
        let deadline = Instant::now() + BUDGET;
        while self.row < grid.rows {
            for _ in 0..CLOCK_INTERVAL {
                self.step(terminal, grid.cols)?;
                if self.row == grid.rows {
                    return Ok(true);
                }
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Return matches found so far, ordered from the oldest row to the newest.
    #[must_use]
    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }

    fn step(&mut self, terminal: &Terminal, cols: u16) -> Result<(), Error> {
        if let Some(glyph) = self.pending {
            self.feed_pending(glyph)?;
            return Ok(());
        }
        if self.col == cols {
            if !self.wrapped {
                self.blanks.clear();
                self.pattern.positions.clear();
            }
            self.row += 1;
            self.col = 0;
            return Ok(());
        }

        let col = self.col;
        let reference = grid_ref(terminal, self.row, col)?;
        if col == 0 {
            (self.wrapped, self.has_graphemes) = row_flags(&reference)?;
        }
        let (cell, width) = cell_width(&reference)?;
        if matches!(
            width,
            ffi::CELL_WIDE_SPACER_HEAD | ffi::CELL_WIDE_SPACER_TAIL
        ) {
            self.col += 1;
            return Ok(());
        }
        let end = (col + u16::from(width == ffi::CELL_WIDE_WIDE)).min(cols - 1);
        let text = if self.has_graphemes {
            GlyphText::Cluster(self.read_graphemes(&reference)?)
        } else {
            GlyphText::Scalar(cell_codepoint(cell)?)
        };
        if matches!(text, GlyphText::Scalar(0) | GlyphText::Cluster(0)) {
            self.blank(col);
        } else {
            self.pending = Some(Glyph { col, end, text });
        }
        self.col = end + 1;
        Ok(())
    }

    fn blank(&mut self, col: u16) {
        if !self.pattern.has_space {
            self.pattern.positions.clear();
            return;
        }
        // Empty cells contribute a space only when there is text later on the
        // same logical line. Runs keep long spans of padding out of the matcher.
        if let Some(run) = self.blanks.back_mut()
            && run.row == self.row
            && run.end == col
        {
            run.end += 1;
        } else {
            self.blanks.push_back(BlankRun {
                row: self.row,
                next: col,
                end: col + 1,
            });
        }
    }

    fn feed_pending(&mut self, glyph: Glyph) -> Result<(), Error> {
        if let Some(run) = self.blanks.front_mut() {
            let pos = (run.row, run.next);
            run.next += 1;
            if run.next == run.end {
                self.blanks.pop_front();
            }
            self.pattern.feed(' ', pos, pos, &mut self.matches);
            return Ok(());
        }
        self.pending = None;
        let start = (self.row, glyph.col);
        let end = (self.row, glyph.end);
        let codepoints = match &glyph.text {
            GlyphText::Scalar(cp) => std::slice::from_ref(cp),
            GlyphText::Cluster(len) => &self.graphemes[..*len],
        };
        for &cp in codepoints {
            let ch = char::from_u32(cp).ok_or(Error(ffi::INVALID_VALUE))?;
            self.pattern.feed(ch, start, end, &mut self.matches);
        }
        Ok(())
    }

    fn read_graphemes(&mut self, reference: &ffi::GridRef) -> Result<usize, Error> {
        if self.graphemes.is_empty() {
            self.graphemes.resize(8, 0);
        }
        let mut written = 0;
        // SAFETY: The grid reference is live and the slice describes writable u32 storage.
        let result = unsafe {
            ffi::ghostty_grid_ref_graphemes(
                reference,
                self.graphemes.as_mut_ptr(),
                self.graphemes.len(),
                &raw mut written,
            )
        };
        if result == ffi::OUT_OF_SPACE {
            self.graphemes.resize(written, 0);
            // SAFETY: The grid reference remains live, and the buffer has the reported capacity.
            check(unsafe {
                ffi::ghostty_grid_ref_graphemes(
                    reference,
                    self.graphemes.as_mut_ptr(),
                    self.graphemes.len(),
                    &raw mut written,
                )
            })?;
        } else {
            check(result)?;
        }
        Ok(written)
    }
}

struct BlankRun {
    row: u32,
    next: u16,
    end: u16,
}

#[derive(Clone, Copy)]
struct Glyph {
    col: u16,
    end: u16,
    text: GlyphText,
}

#[derive(Clone, Copy)]
enum GlyphText {
    Scalar(u32),
    Cluster(usize),
}

// KMP keeps matching linear even for repeated prefixes and lets a match span
// arbitrarily many soft-wrapped rows without buffering the line.
struct Pattern {
    needle: Vec<(char, usize)>,
    positions: VecDeque<Position>,
    exact: bool,
    has_space: bool,
}

impl Pattern {
    fn new(query: &str) -> Self {
        let exact = query.chars().any(char::is_uppercase);
        let mut needle = Vec::with_capacity(query.chars().count());
        for ch in query.chars().flat_map(|ch| folded(ch, exact)) {
            let fallback = needle.last().map_or(0, |&(_, fallback)| fallback);
            let next = if needle.is_empty() {
                0
            } else {
                prefix_len(&needle, fallback, ch)
            };
            needle.push((ch, next));
        }
        Self {
            positions: VecDeque::with_capacity(needle.len()),
            needle,
            exact,
            has_space: query.contains(' '),
        }
    }

    fn feed(&mut self, ch: char, start: Position, end: Position, matches: &mut Vec<SearchMatch>) {
        for ch in folded(ch, self.exact) {
            let previous = self.positions.len();
            let next = prefix_len(&self.needle, previous, ch);
            if next == 0 {
                self.positions.clear();
                continue;
            }
            if next <= previous {
                self.positions.drain(..previous - (next - 1));
            }
            self.positions.push_back(start);
            if next == self.needle.len() {
                matches.push(SearchMatch {
                    start: self.positions[0],
                    end,
                });
                let fallback = self.needle[next - 1].1;
                self.positions.drain(..next - fallback);
            }
        }
    }
}

fn folded(ch: char, exact: bool) -> impl Iterator<Item = char> {
    let (scalar, lowercase) = if exact {
        (Some(ch), None)
    } else {
        (None, Some(ch.to_lowercase()))
    };
    scalar.into_iter().chain(lowercase.into_iter().flatten())
}

fn prefix_len(needle: &[(char, usize)], mut len: usize, ch: char) -> usize {
    while len != 0 && needle[len].0 != ch {
        len = needle[len - 1].1;
    }
    len + usize::from(needle[len].0 == ch)
}

#[derive(Clone, Copy)]
struct GridSize {
    cols: u16,
    rows: u32,
}

impl GridSize {
    fn read(terminal: &Terminal) -> Result<Self, Error> {
        let mut cols = 0_u16;
        let mut rows = 0_usize;
        // SAFETY: The live terminal's data keys write a u16 and a size_t, respectively.
        unsafe {
            check(ffi::ghostty_terminal_get(
                terminal.raw,
                ffi::TERMINAL_DATA_COLS,
                (&raw mut cols).cast(),
            ))?;
            check(ffi::ghostty_terminal_get(
                terminal.raw,
                ffi::TERMINAL_DATA_TOTAL_ROWS,
                (&raw mut rows).cast(),
            ))?;
        }
        Ok(Self {
            cols,
            rows: u32::try_from(rows).map_err(|_| Error(ffi::INVALID_VALUE))?,
        })
    }
}

fn grid_ref(terminal: &Terminal, y: u32, x: u16) -> Result<ffi::GridRef, Error> {
    let mut reference = ffi::GridRef::default();
    let point = ffi::Point {
        tag: ffi::POINT_TAG_SCREEN,
        value: ffi::PointValue {
            coordinate: ffi::PointCoordinate { x, y },
        },
    };
    // SAFETY: The screen tag uses the coordinate field and the terminal and output are live.
    check(unsafe { ffi::ghostty_terminal_grid_ref(terminal.raw, point, &raw mut reference) })?;
    Ok(reference)
}

fn row_flags(reference: &ffi::GridRef) -> Result<(bool, bool), Error> {
    let mut row = 0;
    let mut wrapped = false;
    let mut graphemes = false;
    // SAFETY: The reference is live and both row keys write a bool.
    unsafe {
        check(ffi::ghostty_grid_ref_row(reference, &raw mut row))?;
        check(ffi::ghostty_row_get(
            row,
            ffi::ROW_DATA_WRAP,
            (&raw mut wrapped).cast(),
        ))?;
        check(ffi::ghostty_row_get(
            row,
            ffi::ROW_DATA_GRAPHEME,
            (&raw mut graphemes).cast(),
        ))?;
    }
    Ok((wrapped, graphemes))
}

fn cell_width(reference: &ffi::GridRef) -> Result<(ffi::Cell, std::ffi::c_int), Error> {
    let mut cell = 0;
    let mut width = 0;
    // SAFETY: The reference is live and this cell data key writes a C enum.
    unsafe {
        check(ffi::ghostty_grid_ref_cell(reference, &raw mut cell))?;
        check(ffi::ghostty_cell_get(
            cell,
            ffi::CELL_DATA_WIDE,
            (&raw mut width).cast(),
        ))?;
    }
    Ok((cell, width))
}

fn cell_codepoint(cell: ffi::Cell) -> Result<u32, Error> {
    let mut cp = 0;
    // SAFETY: Cell is a copied native value, and the codepoint key writes one u32.
    check(unsafe { ffi::ghostty_cell_get(cell, ffi::CELL_DATA_CODEPOINT, (&raw mut cp).cast()) })?;
    Ok(cp)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use super::*;

    #[test]
    fn matches_logical_lines() {
        let mut terminal = Terminal::new(5, 2).unwrap();
        terminal.write("0abc界e\u{301}Z\r\n0ABC界e\u{301}z\r\nXY".as_bytes());
        assert_eq!(terminal.viewport_top().unwrap(), 3);
        terminal.reveal_search(0).unwrap();
        terminal.reveal_search(1).unwrap();
        assert_eq!(terminal.viewport_top().unwrap(), 0);
        for (query, expected) in [
            ("c界e\u{301}z", &[((0, 3), (1, 3)), ((2, 3), (3, 3))][..]),
            ("C界", &[((2, 3), (3, 1))]),
            ("\u{301}", &[((1, 2), (1, 2)), ((3, 2), (3, 2))]),
            ("z0", &[]),
        ] {
            let mut search = Search::new(query);
            while !search.advance(&terminal).unwrap() {}
            let matches: Vec<_> = search.matches().iter().map(|m| (m.start, m.end)).collect();
            assert_eq!(matches, expected, "{query:?}");
        }
        if std::env::var_os("SUMIE_SEARCH_BENCH").is_some() {
            time_scrollback_search();
        }
    }

    fn time_scrollback_search() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        terminal.set_scrollback_bytes(usize::MAX).unwrap();
        let mut text = String::new();
        for line in 1..=100_000 {
            write!(text, "{line}\r\n").unwrap();
        }
        terminal.write(text.as_bytes());
        assert!(terminal.viewport_top().unwrap() > 99_000);
        for (phase, query) in [
            ("pass", "9999"),
            ("keystroke", "9"),
            ("keystroke", "99"),
            ("keystroke", "999"),
            ("keystroke", "9999"),
        ] {
            let start = Instant::now();
            let mut search = Search::new(query);
            let mut longest = Duration::ZERO;
            let mut chunks = 0;
            loop {
                let start = Instant::now();
                let done = search.advance(&terminal).unwrap();
                longest = longest.max(start.elapsed());
                chunks += 1;
                if done {
                    break;
                }
            }
            eprintln!(
                "100k {phase} {query:?}: {:?} total, {longest:?} max chunk ({chunks} chunks, {} matches)",
                start.elapsed(),
                search.matches().len()
            );
        }
    }
}
