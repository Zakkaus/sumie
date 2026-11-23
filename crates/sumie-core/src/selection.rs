use std::ops::RangeInclusive;

/// Viewport coordinates in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CellPosition {
    pub row: u16,
    pub column: u16,
}

/// Selection coordinates in reading order, relative to the viewport's top row.
/// A logical line or word can extend outside the visible rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SelectionPoint {
    pub row: i64,
    pub column: u16,
}

impl From<CellPosition> for SelectionPoint {
    fn from(cell: CellPosition) -> Self {
        Self {
            row: i64::from(cell.row),
            column: cell.column,
        }
    }
}

/// Inclusive endpoints, ordered and expanded to whole cell groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRange {
    pub start: SelectionPoint,
    pub end: SelectionPoint,
}

impl CellRange {
    /// Expand a hit to include both columns of a wide glyph.
    #[must_use]
    pub fn group(hit: CellPosition, width: u8) -> Self {
        let mut start = SelectionPoint::from(hit);
        let mut end = start;
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
        let row = i64::from(row);
        (self.start.row..=self.end.row).contains(&row).then(|| {
            let left = if row == self.start.row {
                u32::from(self.start.column)
            } else {
                0
            };
            let right = if row == self.end.row {
                u32::from(self.end.column)
            } else {
                u32::MAX
            };
            left..=right
        })
    }
}

struct Drag {
    origin: CellPosition,
    head: CellPosition,
    group: Option<CellRange>,
}

#[derive(Default)]
pub struct Selection {
    drag: Option<Drag>,
    range: Option<CellRange>,
}

impl Selection {
    pub fn press(&mut self, hit: CellPosition, group: Option<CellRange>, select: bool) {
        self.drag = Some(Drag {
            origin: hit,
            head: hit,
            group,
        });
        self.range = if select { group } else { None };
    }

    /// Resolve the group at the moved endpoint towards the original press.
    ///
    /// # Errors
    /// Returns a group resolution error unchanged.
    pub fn drag<E>(
        &mut self,
        hit: CellPosition,
        mut resolve: impl FnMut(CellPosition, CellPosition) -> Result<Option<CellRange>, E>,
    ) -> Result<(), E> {
        let Some(drag) = self.drag.as_mut().filter(|drag| drag.head != hit) else {
            return Ok(());
        };
        self.range = if let Some(group) = resolve(hit, drag.origin)? {
            let anchor = match drag.group {
                Some(group) => Some(group),
                None => resolve(drag.origin, hit)?,
            };
            anchor.map(|anchor| CellRange {
                start: anchor.start.min(group.start),
                end: anchor.end.max(group.end),
            })
        } else {
            None
        };
        drag.head = hit;
        Ok(())
    }

    pub fn release(&mut self) -> Option<CellRange> {
        self.drag.take().and(self.range)
    }

    #[must_use]
    pub fn range(&self) -> Option<CellRange> {
        self.range
    }

    /// Include the pressed cell before the pointer has crossed a cell boundary.
    #[must_use]
    pub fn affected_rows(&self) -> Option<RangeInclusive<i64>> {
        if let Some(range) = self.range.or_else(|| self.drag.as_ref()?.group) {
            Some(range.start.row..=range.end.row)
        } else {
            let row = i64::from(self.drag.as_ref()?.origin.row);
            Some(row..=row)
        }
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
            let group = |x| CellRange::group(position(x), widths[usize::from(x)]);
            selection.press(position(press), Some(group(press)), false);
            selection
                .drag(position(end), |hit, _| {
                    Ok::<_, std::convert::Infallible>(Some(group(hit.column)))
                })
                .unwrap();
            assert_eq!(
                selection.release().map(|r| (r.start.column, r.end.column)),
                expected
            );
        }
    }
}
