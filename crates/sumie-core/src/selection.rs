use std::ops::RangeInclusive;

/// Viewport coordinates in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CellPosition {
    pub row: u16,
    pub column: u16,
}

/// Inclusive endpoints, ordered and expanded to whole cell groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRange {
    pub start: CellPosition,
    pub end: CellPosition,
}

impl CellRange {
    fn group(hit: CellPosition, width: u8) -> Self {
        let mut start = hit;
        let mut end = hit;
        // A zero-width cell is the trailing column of a wide glyph in the snapshot.
        if width == 0 {
            start.column -= 1;
        } else if width == 2 {
            end.column += 1;
        }
        Self { start, end }
    }

    /// Columns to highlight on this row, bounded by the caller's grid.
    #[must_use]
    pub fn columns(self, row: u32) -> Option<RangeInclusive<u32>> {
        let first = u32::from(self.start.row);
        let last = u32::from(self.end.row);
        (first..=last).contains(&row).then(|| {
            let left = if row == first {
                u32::from(self.start.column)
            } else {
                0
            };
            let right = if row == last {
                u32::from(self.end.column)
            } else {
                u32::MAX
            };
            left..=right
        })
    }
}

#[derive(Default)]
pub struct Selection {
    anchor: Option<(CellPosition, CellRange)>,
    range: Option<CellRange>,
}

impl Selection {
    pub fn press(&mut self, hit: CellPosition, width: u8) {
        self.anchor = Some((hit, CellRange::group(hit, width)));
        self.range = None;
    }

    pub fn drag(&mut self, hit: CellPosition, width: u8) {
        if let Some((origin, anchor)) = self.anchor
            && (hit != origin || self.range.is_some())
        {
            let other = CellRange::group(hit, width);
            self.range = Some(CellRange {
                start: anchor.start.min(other.start),
                end: anchor.end.max(other.end),
            });
        }
    }

    pub fn release(&mut self) -> Option<CellRange> {
        self.anchor.take().and(self.range)
    }

    #[must_use]
    pub fn range(&self) -> Option<CellRange> {
        self.range
    }

    /// Include the pressed cell before the pointer has crossed a cell boundary.
    #[must_use]
    pub fn affected_rows(&self) -> Option<RangeInclusive<usize>> {
        let range = self.range.or_else(|| self.anchor.map(|(_, group)| group))?;
        Some(usize::from(range.start.row)..=usize::from(range.end.row))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_selects_whole_cell_groups() {
        let position = |column| CellPosition { row: 0, column };
        let widths = [1, 2, 0, 1, 1];
        for (press, end, expected) in [
            (0, 1, Some((0, 2))),
            (4, 2, Some((1, 4))),
            (2, 1, Some((1, 2))),
            (3, 3, None),
        ] {
            let mut selection = Selection::default();
            selection.press(position(press), widths[usize::from(press)]);
            selection.drag(position(end), widths[usize::from(end)]);
            assert_eq!(
                selection.release().map(|r| (r.start.column, r.end.column)),
                expected
            );
        }
    }
}
