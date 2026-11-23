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
    active_rows: Vec<grid::Stamp>,
    recheck: grid::Recheck,
    next_repair: usize,
    row_started: bool,
    repair_from: Option<usize>,
    replacement: Vec<SearchMatch>,
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
            active_rows: Vec::new(),
            recheck: grid::Recheck::default(),
            next_repair: 0,
            row_started: false,
            repair_from: None,
            replacement: Vec::new(),
        }
    }

    /// Scan another time-bounded chunk, returning `true` when all rows are scanned.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the grid or it exceeds the coordinate range.
    pub fn advance(&mut self, terminal: &Terminal) -> Result<bool, Error> {
        self.advance_until(terminal, Instant::now() + BUDGET)
    }

    fn advance_until(&mut self, terminal: &Terminal, deadline: Instant) -> Result<bool, Error> {
        if self.pattern.needle.is_empty() {
            return Ok(true);
        }
        let grid = if let Some(grid) = self.grid {
            grid
        } else {
            let grid = grid::Size::read(terminal)?;
            self.grid = Some(grid);
            grid
        };
        if self.row == grid.rows {
            self.recheck
                .advance(terminal, grid, &mut self.active_rows, deadline)?;
            if !self.begin_repair(grid.active) {
                return Ok(!self.recheck.pending());
            }
        }
        self.cells.resize(usize::from(grid.cols), grid::Cell(0));
        let mut rows = grid::Rows::at(terminal, self.row)?;
        self.save_checkpoint(grid.active);
        while self.row < grid.rows {
            let row = rows.read(&mut self.cells, grid.cols)?;
            if !self.row_started && self.row >= grid.active {
                let i = (self.row - grid.active) as usize;
                let current = row.stamp(self.progress.is_empty())?;
                if let Some(stamp) = self.active_rows.get_mut(i) {
                    *stamp = current;
                } else {
                    self.active_rows.push(current);
                }
                self.row_started = true;
            }
            if !self.scan_row(&row, deadline)? {
                return Ok(false);
            }
            if !row.data.wrapped {
                self.progress.clear();
            }
            self.row += 1;
            self.col = 0;
            self.row_started = false;
            if self.repair_from.is_some()
                && (self.row == grid.rows
                    || (self.progress.is_empty()
                        && self
                            .active_rows
                            .get((self.row - grid.active) as usize)
                            .is_some_and(|s| s.neutral && !s.dirty)))
            {
                self.next_repair = (self.row - grid.active) as usize;
                self.finish_repair(grid.rows);
                break;
            }
            self.save_checkpoint(grid.active);
            if self.row.is_multiple_of(ROW_BATCH) && Instant::now() >= deadline {
                break;
            }
        }
        Ok(self.row == grid.rows
            && !self.recheck.pending()
            && !self.active_rows.iter().any(|stamp| stamp.dirty))
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
        if trimmed == 0 && grid == old && dirty_from == grid.active {
            // Never rewind an in-flight scan. Revisit changed rows after reaching
            // the tail, even when a program redraws before every time slice.
            self.recheck.schedule(self.active_rows.len());
            if self.row_started {
                // A row resumed between writes could mix two versions even if
                // a later write restores its original contents before recheck.
                let stamp = &mut self.active_rows[(self.row - grid.active) as usize];
                grid::check_row(terminal, self.row, stamp)?;
            }
            return Ok(());
        }
        if self.repair_from.take().is_some() {
            self.row = old.rows;
            self.col = 0;
            self.replacement.clear();
        }
        self.active_rows.clear();
        self.recheck = grid::Recheck::default();
        self.next_repair = 0;
        self.row_started = false;
        if old.cols == grid.cols {
            if trimmed != 0 {
                self.trim(trimmed);
            }
            let dirty = dirty_from.min(grid.active);
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

    /// All rows have been visited; existing matches remain navigable while
    /// replacements for newly dirtied rows are prepared.
    #[must_use]
    pub fn has_full_results(&self) -> bool {
        self.repair_from.is_some() || self.grid.is_some_and(|grid| self.row == grid.rows)
    }

    fn begin_repair(&mut self, active: u32) -> bool {
        let (before, after) = self.active_rows.split_at(self.next_repair);
        let Some(dirty) = after
            .iter()
            .position(|s| s.dirty)
            .map(|i| self.next_repair + i)
            .or_else(|| before.iter().position(|s| s.dirty))
        else {
            return false;
        };
        // A neutral boundary carries neither a partial match nor pending spaces
        // from an earlier row. Otherwise use the immutable history checkpoint.
        let start = self.active_rows[..=dirty].iter().rposition(|s| s.neutral);
        self.progress.clear();
        self.row = active;
        if let Some(i) = start {
            self.row += u32::try_from(i).expect("active row index fits u32");
        } else {
            self.progress.copy_from(&self.saved_progress);
        }
        let cutoff = self.match_cutoff();
        self.repair_from = Some(self.matches.partition_point(|m| m.end < cutoff));
        true
    }

    fn finish_repair(&mut self, rows: u32) {
        let start = self.repair_from.take().expect("active repair");
        let end = self.matches.partition_point(|m| m.end.0 < self.row);
        self.matches.splice(start..end, self.replacement.drain(..));
        self.row = rows;
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
        let cutoff = self.match_cutoff();
        self.matches
            .truncate(self.matches.partition_point(|m| m.end < cutoff));
        Ok(())
    }

    fn match_cutoff(&self) -> Position {
        self.progress
            .blanks
            .front()
            .map_or((self.row, 0), |run| (run.row, run.next))
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
                if self.repair_from.is_some() {
                    &mut self.replacement
                } else {
                    &mut self.matches
                },
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
                if self.repair_from.is_some() {
                    &mut self.replacement
                } else {
                    &mut self.matches
                },
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
    fn is_empty(&self) -> bool {
        self.positions.is_empty() && self.blanks.is_empty()
    }

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
mod tests;
