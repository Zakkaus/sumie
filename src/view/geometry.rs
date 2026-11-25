//! Map window and pane coordinates to the terminal grid, and between the platform's
//! coordinate spaces and the buffer's device pixels.

use sumie_core::{CellPosition, splits::Rect};
use sumie_pty::Size;

use super::View;
use crate::platform::{CaretBounds, LogicalRect, Position};

impl View {
    /// Map a pointer to device pixels of the current buffer. The viewporter stretches the
    /// rounded buffer to the exact logical size, so each logical axis has its own ratio.
    pub(crate) fn window_pixel(&self, position: Position) -> [f64; 2] {
        match position {
            Position::DevicePx(x, y) => [x, y],
            Position::SurfaceLogical(x, y) => std::array::from_fn(|i| {
                [x, y][i] * f64::from(self.device[i]) / f64::from(self.logical[i])
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn size(&self) -> Size {
        self.pane_size(self.content_rect())
    }

    pub(crate) const fn content_rect(&self) -> Rect {
        let y = self.bar_height();
        Rect {
            x: 0,
            y,
            width: self.device[0],
            height: self.device[1] - y,
        }
    }

    pub(crate) const fn pane_padding(&self) -> [u32; 2] {
        self.device_padding
    }

    pub(super) fn pane_grid(&self, rect: Rect) -> Rect {
        // Drop padding before clipping the terminal's minimum one-cell grid.
        let x = self.device_padding[0].min(rect.width.saturating_sub(self.cell[0]) / 2);
        let y = self.device_padding[1].min(rect.height.saturating_sub(self.cell[1]) / 2);
        Rect {
            x: rect.x + x,
            y: rect.y + y,
            width: rect.width - 2 * x,
            height: rect.height - 2 * y,
        }
    }

    pub(crate) fn pane_size(&self, rect: Rect) -> Size {
        let rect = self.pane_grid(rect);
        let [cols, rows] =
            grid_size(rect, self.cell).map(|value| u16::try_from(value).expect("validated grid"));
        let [width_px, height_px] = [rect.width, rect.height]
            .map(|value| u16::try_from(value).expect("validated device size"));
        Size {
            cols,
            rows,
            width_px,
            height_px,
        }
    }

    /// `scroll` is the offset of the frame the pointer sees: content under the pointer is
    /// that many pixels further down the grid.
    pub(crate) fn pointer_position(
        &self,
        position: Position,
        rect: Rect,
        scroll: u32,
    ) -> (CellPosition, [f32; 2], bool) {
        let rect = self.pane_grid(rect);
        let relative = self.pane_pixel(position, rect);
        let grid = grid_size(rect, self.cell);
        let pixel = relative.map(|pos| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "native mouse coordinates are f32"
            )]
            {
                pos as f32
            }
        });
        let over_grid = contains_grid_pixel(relative, rect, grid, self.cell);
        let content = [relative[0], relative[1] + f64::from(scroll)];
        (self.pointer_cell(content, grid), pixel, over_grid)
    }

    fn pane_pixel(&self, position: Position, rect: Rect) -> [f64; 2] {
        let [x, y] = self.window_pixel(position);
        [x - f64::from(rect.x), y - f64::from(rect.y)]
    }

    fn pointer_cell(&self, pixel: [f64; 2], grid: [u32; 2]) -> CellPosition {
        let cell = |axis: usize| {
            let index = pixel[axis] / f64::from(self.cell[axis]);
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                index.clamp(0.0, f64::from(grid[axis] - 1)) as u16
            }
        };
        CellPosition {
            row: cell(1),
            column: cell(0),
        }
    }

    pub(crate) fn link_cell(
        &self,
        position: Position,
        rect: Rect,
        banner: bool,
        scroll: u32,
    ) -> Option<CellPosition> {
        let rect = self.pane_grid(rect);
        let pixel = self.pane_pixel(position, rect);
        let grid = grid_size(rect, self.cell);
        if !contains_grid_pixel(pixel, rect, grid, self.cell) || self.over_tab_bar(position) {
            return None;
        }
        let content = [pixel[0], pixel[1] + f64::from(scroll)];
        // The partly shown row below the viewport has no cells to link.
        if content[1] >= f64::from(grid[1] * self.cell[1]) {
            return None;
        }
        let hit = self.pointer_cell(content, grid);
        (!banner || hit.row != 0).then_some(hit)
    }
}

/// Map clipped caret edges in device pixels to surface-local logical coordinates, rounding
/// outward. The viewporter stretches the rounded buffer, not the nominal scale.
pub(crate) fn surface_logical_rect(bounds: CaretBounds) -> Option<LogicalRect> {
    let axis = |i: usize| -> Option<(i32, i32)> {
        let device = i64::from(bounds.device[i]);
        let logical = i64::from(bounds.logical[i]);
        if device == 0 {
            return None;
        }
        let left = i64::from(bounds.start[i]) * logical / device;
        let right = (bounds.end[i] * logical + device - 1) / device;
        Some((left.try_into().ok()?, (right - left).try_into().ok()?))
    };
    let (x, width) = axis(0)?;
    let (y, height) = axis(1)?;
    Some(LogicalRect {
        x,
        y,
        width,
        height,
    })
}

fn contains_grid_pixel(pixel: [f64; 2], rect: Rect, grid: [u32; 2], cell: [u32; 2]) -> bool {
    let size = [rect.width, rect.height];
    (0..2).all(|i| {
        let end = f64::from(size[i].min(grid[i] * cell[i]));
        (0.0..end).contains(&pixel[i])
    })
}

pub(super) fn scaled_padding(logical: [u16; 2], scale: u32) -> [u32; 2] {
    logical.map(|px| (u32::from(px) * scale + 60) / 120)
}

pub(super) fn grid_size(Rect { width, height, .. }: Rect, cell: [u32; 2]) -> [u32; 2] {
    [(width / cell[0]).max(1), (height / cell[1]).max(1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_covers_the_cell_on_a_rounded_fractional_buffer() {
        let rect = surface_logical_rect(CaretBounds {
            start: [100, 100],
            end: [110, 110],
            logical: [801, 599],
            device: [1001, 749],
        })
        .unwrap();
        assert_eq!([rect.x, rect.y, rect.width, rect.height], [80, 79, 9, 9]);
    }
}
