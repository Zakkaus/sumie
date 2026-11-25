//! Compose one owned viewport snapshot into a reusable, pane-local grid.

use std::sync::Arc;

use sumie_core::CellRange;
use sumie_core::graphics::Placement;
use sumie_core::splits::Rect;
use sumie_font::{FontStyle, FontSystem};
use sumie_render::{Cursor, CursorShape, DrawList, PixelRect, Rgba, Scroll};
use sumie_vt::{CursorStyle, Rgb, Snapshot, codepoint_width};

use super::attributes::{Decorations, Ink, LinkHighlight};
use super::overlays::{Glyphs, SearchBox};
use super::{FontErrors, Line, ViewerPaint, rgba};

#[derive(Clone, Copy)]
pub(crate) struct PaneFocus<'a> {
    pub(crate) active: bool,
    pub(crate) window_focused: bool,
    pub(crate) id: u64,
    pub(crate) dirty: bool,
    pub(crate) viewer: Option<&'a Placement>,
    /// Device pixels the content is scrolled up from its row boundary.
    pub(crate) scroll: u32,
}

pub(super) struct PanePaint {
    pub(super) id: Option<u64>,
    pub(super) active: bool,
    pub(super) rect: Rect,
    pub(super) grid: [u32; 2],
    pub(super) focused: bool,
    pub(super) backgrounds: Vec<Rgba>,
    pub(super) default_background: Rgba,
    pub(super) default_backgrounds: Vec<bool>,
    pub(super) images: Option<Arc<Vec<Placement>>>,
    pub(super) viewer: Option<ViewerPaint>,
    pub(super) image_exclusions: [Option<PixelRect>; 2],
    pub(super) glyphs: Glyphs,
    pub(super) scroll: Scroll,
    pub(super) cursor: Option<Cursor>,
    pub(super) search_box: Option<SearchBox>,
}

impl PanePaint {
    pub(super) fn new(rect: Rect) -> Self {
        Self {
            id: None,
            active: false,
            rect,
            grid: [0; 2],
            focused: false,
            backgrounds: Vec::new(),
            default_background: [0, 0, 0, 255],
            default_backgrounds: Vec::new(),
            images: None,
            viewer: None,
            image_exclusions: [None; 2],
            glyphs: Glyphs::default(),
            scroll: Scroll::default(),
            cursor: None,
            search_box: None,
        }
    }

    pub(super) fn draw_list(&self, cell_size: [u32; 2], cursor_hidden: bool) -> DrawList<'_> {
        DrawList {
            grid_size: self.grid,
            cell_size,
            origin: self.glyphs.offset,
            scroll: self.scroll,
            backgrounds: &self.backgrounds,
            images: self.images.as_ref().map_or(&[], |images| images.as_slice()),
            image_source: None,
            default_background: self.default_background,
            default_backgrounds: &self.default_backgrounds,
            image_exclusions: self.image_exclusions,
            glyphs: &self.glyphs.quads,
            cursor: self.cursor.filter(|_| !cursor_hidden || !self.focused),
            chrome: None,
        }
    }
}

pub(super) struct PanePainter<'a> {
    pub(super) pane: &'a mut PanePaint,
    pub(super) font: &'a mut FontSystem,
    pub(super) font_errors: &'a mut FontErrors,
    pub(super) line: &'a mut Line,
    pub(super) composition: &'a mut Line,
    pub(super) cell: [u32; 2],
    pub(super) background_alpha: u8,
    pub(super) link_hint: Rgba,
    pub(super) selection_foreground: Option<Rgb>,
    pub(super) selection_background: Option<Rgb>,
    pub(super) search_bar: Option<&'a str>,
    pub(super) search_selection: std::ops::Range<usize>,
    pub(super) search_highlights: &'a [(u16, u16, u16, bool)],
    pub(super) preedit: &'a str,
}

impl PanePainter<'_> {
    pub(super) fn build(
        &mut self,
        snapshot: &Snapshot,
        selection: Option<CellRange>,
        hovered_link: LinkHighlight<'_>,
        banner: Option<&str>,
    ) {
        self.pane.backgrounds.clear();
        self.pane.images = (!snapshot.images.is_empty()).then(|| Arc::clone(&snapshot.images));
        self.pane.default_background = rgba(snapshot.background);
        self.pane.default_background[3] = self.background_alpha;
        self.pane.default_backgrounds.clear();
        self.pane.glyphs.quads.clear();
        self.set_overlay_bounds(banner.is_some());
        let overlay = (self.pane.search_box.is_none() && !self.preedit.is_empty())
            .then(|| self.composition(snapshot))
            .flatten()
            .filter(|&(row, _, _)| row != 0 || banner.is_none());
        if let Some(message) = banner {
            self.draw_banner(snapshot, message);
            if self.pane.images.is_some() {
                self.pane
                    .default_backgrounds
                    .resize(self.pane.grid[0] as usize, false);
            }
        }
        self.draw_terminal(
            snapshot,
            selection,
            hovered_link,
            overlay,
            usize::from(banner.is_some()),
        );
        if let Some((row, column, _)) = overlay {
            self.composition.draw(
                self.font,
                self.font_errors,
                &mut self.pane.glyphs,
                [column, row],
                rgba(snapshot.foreground),
                FontStyle::default(),
            );
        }
        self.tint_matches(snapshot, banner.is_some());
        if let Some(search_box) = self.pane.search_box {
            self.pane.glyphs.cover = None;
            self.draw_search(snapshot, search_box);
        } else {
            self.set_cursor(snapshot, overlay, banner.is_some());
        }
    }

    fn draw_terminal(
        &mut self,
        snapshot: &Snapshot,
        selection: Option<CellRange>,
        mut hovered_link: LinkHighlight<'_>,
        overlay: Option<(u32, u32, u32)>,
        first_row: usize,
    ) {
        let mut decorations = Decorations::default();
        let rows = (0..).zip(snapshot.rows.iter().take(self.pane.grid[1] as usize));
        // The row below the viewport is only painted while a scroll offset reveals it.
        let below = snapshot
            .below
            .as_ref()
            .filter(|_| self.pane.scroll.extra_rows != 0)
            .map(|cells| (self.pane.grid[1], cells));
        for (row, cells) in rows.skip(first_row).chain(below) {
            let selected = selection.and_then(|range| range.columns(row));
            let columns = overlay
                .filter(|&(y, _, _)| y == row)
                .map(|(_, x, width)| x..x + width);
            self.line.clear();
            let mut start = 0;
            let mut run = None;
            for (column, cell) in (0..).zip(cells) {
                let highlighted = selected.as_ref().is_some_and(|cols| cols.contains(&column));
                let covered = columns
                    .as_ref()
                    .is_some_and(|range| range.contains(&column));
                let (foreground, background) = if highlighted {
                    (
                        self.selection_foreground.unwrap_or(cell.background),
                        self.selection_background.unwrap_or(cell.foreground),
                    )
                } else {
                    (cell.foreground, cell.background)
                };
                let default = cell.default_background && !highlighted && !covered;
                let mut color = rgba(if covered {
                    snapshot.background
                } else {
                    background
                });
                if default {
                    color[3] = self.background_alpha;
                }
                self.pane.backgrounds.push(color);
                if self.pane.images.is_some() {
                    self.pane.default_backgrounds.push(default);
                }
                if cell.width == 0 {
                    continue;
                }
                let ink = Ink::new(foreground, cell.style);
                if run != Some((highlighted, ink)) {
                    if let Some((_, previous)) = run {
                        self.finish_run([start, row], previous);
                    }
                    start = column;
                    run = Some((highlighted, ink));
                }
                let intersects = columns.as_ref().is_some_and(|columns| {
                    columns.start < column + u32::from(cell.width) && column < columns.end
                });
                if !intersects {
                    let hovered = hovered_link.at(row, column);
                    decorations.draw(self, [column, row], cell, ink.color, hovered);
                }
                self.line
                    .push(if intersects { "" } else { &cell.text }, cell.width);
            }
            if let Some((_, ink)) = run {
                self.finish_run([start, row], ink);
            }
        }
    }

    fn set_cursor(&mut self, snapshot: &Snapshot, overlay: Option<(u32, u32, u32)>, banner: bool) {
        self.pane.cursor = snapshot.cursor.and_then(|cursor| {
            if (banner && cursor.row == 0) || (!cursor.visible && overlay.is_none()) {
                return None;
            }
            let [mut column, row] = [u32::from(cursor.column), u32::from(cursor.row)];
            let (origin, size, shape) = if let Some((_, _, width)) = overlay {
                let [x, y] = self.origin([column, row]);
                (
                    [x, y + self.cell[1].cast_signed() - 2],
                    [width * self.cell[0], 2],
                    CursorShape::Solid,
                )
            } else {
                let cells = &snapshot.rows[row as usize];
                if self.pane.focused
                    && cursor.style == CursorStyle::Block
                    && column > 0
                    && cells[column as usize - 1].width == 2
                {
                    column -= 1;
                }
                let width = cells[column as usize].width.max(1);
                let shape = match (self.pane.focused, cursor.style) {
                    (true, CursorStyle::Block) => CursorShape::Block {
                        cell: [column, row],
                        columns: width,
                    },
                    (true, CursorStyle::Bar) => CursorShape::Bar,
                    (true, CursorStyle::Underline) => CursorShape::Underline,
                    _ => CursorShape::Hollow,
                };
                (
                    self.origin([column, row]),
                    [u32::from(width) * self.cell[0], self.cell[1]],
                    shape,
                )
            };
            Some(Cursor {
                rect: PixelRect { origin, size },
                color: rgba(cursor.color),
                shape,
            })
        });
    }

    fn composition(&mut self, snapshot: &Snapshot) -> Option<(u32, u32, u32)> {
        self.composition.clear();
        let cursor = snapshot.cursor?;
        let column = u32::from(cursor.column);
        let available = self.pane.grid[0].saturating_sub(column);
        let mut cells = 0;
        for ch in self.preedit.chars().filter(|ch| !ch.is_control()) {
            let width = codepoint_width(ch);
            if cells + u32::from(width) > available {
                break;
            }
            if width != 0 || cells != 0 {
                self.composition.text.push(ch);
                self.composition.widths.push(width);
            }
            cells += u32::from(width);
        }
        (cells > 0).then_some((u32::from(cursor.row), column, cells))
    }

    pub(super) fn origin(&self, [column, row]: [u32; 2]) -> [i32; 2] {
        [
            self.pane.glyphs.offset[0] + (column * self.cell[0]).cast_signed(),
            self.pane.glyphs.offset[1] + (row * self.cell[1]).cast_signed(),
        ]
    }
}
