//! Scan the active screen in place, including scrollback. Search owns only
//! query state, cell coordinates, and scratch space, not a copy of the grid.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::{Error, Terminal};

mod grid;

const BUDGET: Duration = Duration::from_millis(2);
const ROW_BATCH: u32 = 32;
const CELL_BATCH: u16 = 256;

type Position = (u32, u16);

/// An inclusive match in `(row, column)` coordinates from the oldest retained row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchMatch {
    pub start: (u32, u16),
    pub end: (u32, u16),
}

/// An incremental substring search over one terminal's active screen and scrollback.
///
/// Refresh after a write and start a new search after a resize. Scrolling does
/// not invalidate a search.
pub struct Search {
    pattern: Pattern,
    grid: Option<grid::Size>,
    row: u32,
    col: u16,
    progress: Progress,
    checkpoint: Option<u32>,
    saved_progress: Progress,
    cells: Vec<grid::Cell>,
    graphemes: Vec<u32>,
    matches: Vec<SearchMatch>,
}

impl Search {
    /// Create a literal search. A query with uppercase characters is case-sensitive;
    /// otherwise use Unicode simple case folding. An empty query finds nothing.
    #[must_use]
    pub fn new(query: &str) -> Self {
        Self {
            pattern: Pattern::new(query),
            grid: None,
            row: 0,
            col: 0,
            progress: Progress::default(),
            checkpoint: None,
            saved_progress: Progress::default(),
            cells: Vec::new(),
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
        let deadline = Instant::now() + BUDGET;
        let grid = if let Some(grid) = self.grid {
            grid
        } else {
            let grid = grid::Size::read(terminal)?;
            self.grid = Some(grid);
            grid
        };
        if self.row == grid.rows {
            return Ok(true);
        }
        self.cells.resize(usize::from(grid.cols), grid::Cell(0));
        let mut rows = grid::Rows::at(terminal, self.row)?;
        self.save_checkpoint(grid.active);
        while self.row < grid.rows {
            let row = rows.read(&mut self.cells, grid.cols)?;
            if !self.scan_row(&row, deadline)? {
                return Ok(false);
            }
            if !row.data.wrapped {
                self.progress.clear();
            }
            self.row += 1;
            self.col = 0;
            self.save_checkpoint(grid.active);
            if self.row.is_multiple_of(ROW_BATCH) && Instant::now() >= deadline {
                break;
            }
        }
        Ok(self.row == grid.rows)
    }

    /// After a write, preserve search progress before the first possibly changed
    /// physical row (`dirty_from`, in new coordinates). `trimmed` counts rows
    /// evicted from the head since the previous scan or refresh.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the grid or it exceeds the coordinate range.
    pub fn refresh(
        &mut self,
        terminal: &Terminal,
        trimmed: u32,
        dirty_from: u32,
    ) -> Result<(), Error> {
        let Some(old) = self.grid else {
            return Ok(());
        };
        let grid = grid::Size::read(terminal)?;
        if old.cols == grid.cols {
            if trimmed != 0 {
                self.trim(trimmed);
            }
            let dirty = dirty_from.min(grid.rows);
            self.checkpoint = self.checkpoint.filter(|&row| row <= dirty);
            if self.row > dirty || (self.row == dirty && self.col != 0) {
                self.restart_at(terminal, dirty)?;
            }
        } else {
            self.checkpoint = None;
            self.restart_at(terminal, 0)?;
        }
        self.grid = Some(grid);
        Ok(())
    }

    /// Return matches found so far, ordered from the oldest row to the newest.
    #[must_use]
    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }

    fn save_checkpoint(&mut self, active: u32) {
        // A line can wrap from immutable history into the writable screen.
        // Save its matcher at the boundary so appends need not rescan that history.
        if self.row == active && self.col == 0 && self.checkpoint != Some(active) {
            self.checkpoint = Some(active);
            self.saved_progress.copy_from(&self.progress);
        }
    }

    fn restart_at(&mut self, terminal: &Terminal, dirty: u32) -> Result<(), Error> {
        if let Some(checkpoint) = self.checkpoint {
            self.row = checkpoint;
            self.progress.copy_from(&self.saved_progress);
        } else {
            self.row = grid::line_start(terminal, dirty)?;
            self.progress.clear();
        }
        self.col = 0;
        // Pending blanks can produce matches ending before the checkpoint once
        // later text arrives. Revalidate those matches with the suffix.
        let cutoff = self
            .progress
            .blanks
            .front()
            .map_or((self.row, 0), |run| (run.row, run.next));
        self.matches
            .truncate(self.matches.partition_point(|m| m.end < cutoff));
        Ok(())
    }

    fn trim(&mut self, trimmed: u32) {
        self.matches.retain_mut(|m| {
            if m.start.0 < trimmed {
                return false;
            }
            m.start.0 -= trimmed;
            m.end.0 -= trimmed;
            true
        });
        if self.row < trimmed {
            self.row = 0;
            self.col = 0;
            self.progress.clear();
        } else {
            self.row -= trimmed;
            self.progress.trim(&self.pattern, trimmed);
        }
        self.checkpoint = self.checkpoint.and_then(|row| row.checked_sub(trimmed));
        if self.checkpoint.is_some() {
            self.saved_progress.trim(&self.pattern, trimmed);
        }
    }

    fn scan_row(&mut self, row: &grid::Row<'_>, deadline: Instant) -> Result<bool, Error> {
        while self.col < row.data.len {
            let limit = self.col.saturating_add(CELL_BATCH).min(row.data.len);
            while self.col < limit {
                let col = self.col;
                let cell = self.cells[usize::from(col)];
                if cell.is_spacer() {
                    self.col += 1;
                    continue;
                }
                let end = (col + u16::from(cell.is_wide())).min(row.data.cols - 1);
                if cell.codepoint() == 0 {
                    self.progress.blank(&self.pattern, self.row, col);
                } else {
                    if !self.flush_blanks(deadline) {
                        return Ok(false);
                    }
                    self.feed_cell(row, cell, end)?;
                }
                self.col = end + 1;
            }
            if self.col < row.data.len && Instant::now() >= deadline {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn feed_cell(
        &mut self,
        row: &grid::Row<'_>,
        cell: grid::Cell,
        end_col: u16,
    ) -> Result<(), Error> {
        let start = (self.row, self.col);
        let end = (self.row, end_col);
        let cp = cell.codepoint();
        let codepoints = if cell.has_grapheme() {
            row.graphemes(self.col, &mut self.graphemes)?
        } else {
            std::slice::from_ref(&cp)
        };
        for &cp in codepoints {
            self.pattern.feed(
                cp,
                start,
                end,
                &mut self.progress.positions,
                &mut self.matches,
            );
        }
        Ok(())
    }

    fn flush_blanks(&mut self, deadline: Instant) -> bool {
        let mut count = 0;
        while let Some(run) = self.progress.blanks.front_mut() {
            let pos = (run.row, run.next);
            run.next += 1;
            if run.next == run.end {
                self.progress.blanks.pop_front();
            }
            self.pattern.feed(
                u32::from(' '),
                pos,
                pos,
                &mut self.progress.positions,
                &mut self.matches,
            );
            count += 1;
            if count == CELL_BATCH {
                if Instant::now() >= deadline {
                    return false;
                }
                count = 0;
            }
        }
        true
    }
}

#[derive(Default)]
struct Progress {
    positions: VecDeque<Position>,
    blanks: VecDeque<BlankRun>,
}

impl Progress {
    fn copy_from(&mut self, other: &Self) {
        self.positions.clone_from(&other.positions);
        self.blanks.clone_from(&other.blanks);
    }

    fn clear(&mut self) {
        self.positions.clear();
        self.blanks.clear();
    }

    fn trim(&mut self, pattern: &Pattern, trimmed: u32) {
        // Keep the longest matching suffix that doesn't begin in evicted history.
        let len = self.positions.len();
        let mut kept = len;
        while kept > 0 && self.positions[len - kept].0 < trimmed {
            kept = pattern.needle[kept - 1].1;
        }
        self.positions.drain(..len - kept);
        for pos in &mut self.positions {
            pos.0 -= trimmed;
        }
        self.blanks.retain_mut(|run| {
            if run.row < trimmed {
                return false;
            }
            run.row -= trimmed;
            true
        });
    }

    fn blank(&mut self, pattern: &Pattern, row: u32, col: u16) {
        if !pattern.has_space {
            self.positions.clear();
            return;
        }
        // Implicit blanks only contribute spaces once followed by text on the
        // same logical line. Store runs rather than individual cells.
        if let Some(run) = self.blanks.back_mut()
            && run.row == row
            && run.end == col
        {
            run.end += 1;
        } else {
            self.blanks.push_back(BlankRun {
                row,
                next: col,
                end: col + 1,
            });
        }
    }
}

#[derive(Clone)]
struct BlankRun {
    row: u32,
    next: u16,
    end: u16,
}

// KMP keeps matching linear even for repeated prefixes and lets a match span
// arbitrarily many soft-wrapped rows without buffering the line.
struct Pattern {
    needle: Vec<(u32, usize)>,
    exact: bool,
    has_space: bool,
}

impl Pattern {
    fn new(query: &str) -> Self {
        let exact = query.chars().any(char::is_uppercase);
        let mut needle = Vec::with_capacity(query.chars().count());
        for ch in query.chars() {
            let cp = if exact {
                u32::from(ch)
            } else {
                grid::fold(ch.into())
            };
            let next = needle
                .last()
                .map_or(0, |&(_, fallback)| prefix_len(&needle, fallback, cp));
            needle.push((cp, next));
        }
        Self {
            needle,
            exact,
            has_space: query.contains(' '),
        }
    }

    fn feed(
        &self,
        cp: u32,
        start: Position,
        end: Position,
        positions: &mut VecDeque<Position>,
        matches: &mut Vec<SearchMatch>,
    ) {
        let cp = if self.exact { cp } else { grid::fold(cp) };
        let previous = positions.len();
        let next = prefix_len(&self.needle, previous, cp);
        if next == 0 {
            positions.clear();
            return;
        }
        if next <= previous {
            positions.drain(..previous - (next - 1));
        }
        positions.push_back(start);
        if next == self.needle.len() {
            matches.push(SearchMatch {
                start: positions[0],
                end,
            });
            let fallback = self.needle[next - 1].1;
            positions.drain(..next - fallback);
        }
    }
}

fn prefix_len(needle: &[(u32, usize)], mut len: usize, cp: u32) -> usize {
    while len != 0 && needle[len].0 != cp {
        len = needle[len - 1].1;
    }
    len + usize::from(needle[len].0 == cp)
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
        terminal.reveal_search(0, 0).unwrap();
        terminal.reveal_search(1, 1).unwrap();
        assert_eq!(terminal.viewport_top().unwrap(), 0);
        terminal.write("\r\nAAAAA\r\nx\x1b[3Gz\r\nΟΣ".as_bytes());
        for (query, expected) in [
            ("c界e\u{301}z", &[((0, 3), (1, 3)), ((2, 3), (3, 3))][..]),
            ("C界", &[((2, 3), (3, 1))]),
            ("\u{301}", &[((1, 2), (1, 2)), ((3, 2), (3, 2))]),
            ("z0", &[]),
            (
                "aaa",
                &[((5, 0), (5, 2)), ((5, 1), (5, 3)), ((5, 2), (5, 4))],
            ),
            ("x z", &[((6, 0), (6, 2))]),
            ("z ", &[]),
            ("ος", &[((7, 0), (7, 1))]),
            ("Ος", &[]),
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

    #[test]
    fn refresh_keeps_history_and_rescans_edited_wrap() {
        let mut terminal = Terminal::new(5, 2).unwrap();
        terminal.write(b"def\r\nabcdeFghijkdef");
        assert_eq!(terminal.viewport_top().unwrap(), 2);
        let mut search = Search::new("def");
        while !search.advance(&terminal).unwrap() {}
        let prefix = SearchMatch {
            start: (0, 0),
            end: (0, 2),
        };
        let wrapped = SearchMatch {
            start: (1, 3),
            end: (2, 0),
        };
        let suffix = SearchMatch {
            start: (3, 1),
            end: (3, 3),
        };
        assert_eq!(search.matches(), &[prefix, wrapped, suffix]);

        terminal.write(b"\x1b[1A\rZ");
        search.refresh(&terminal, 0, 2).unwrap();
        assert_eq!(search.row, 2);
        assert_eq!(search.matches(), &[prefix]);
        while !search.advance(&terminal).unwrap() {}
        assert_eq!(search.matches(), &[prefix, suffix]);

        terminal.write(b"\rF");
        search.refresh(&terminal, 0, 2).unwrap();
        terminal.write(b"\x1b[2;1H\r\ndef");
        search.refresh(&terminal, 0, 2).unwrap();
        assert_eq!(search.row, 2);
        let appended = SearchMatch {
            start: (4, 0),
            end: (4, 2),
        };
        while !search.advance(&terminal).unwrap() {}
        assert_eq!(search.matches(), &[prefix, wrapped, suffix, appended]);

        terminal.write(b"\x1b[3J");
        assert_eq!(terminal.viewport_top().unwrap(), 0);
        search.refresh(&terminal, 3, 2).unwrap();
        assert!(search.advance(&terminal).unwrap());
        assert_eq!(
            search.matches(),
            &[
                SearchMatch {
                    start: (0, 1),
                    end: (0, 3),
                },
                SearchMatch {
                    start: (1, 0),
                    end: (1, 2),
                },
            ]
        );
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
        for (phase, query, expected) in [
            ("pass", "9999", 20),
            ("keystroke", "9", 50_000),
            ("keystroke", "99", 4_000),
            ("keystroke", "999", 300),
            ("keystroke", "9999", 20),
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
            assert_eq!(search.matches().len(), expected);
        }
    }
}
