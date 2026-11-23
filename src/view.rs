mod atlas;
mod attributes;
use atlas::{collect_dirty, upload};

use std::{
    collections::HashSet,
    mem::{Discriminant, discriminant},
};

use sumie_core::graphics::Placement;
use sumie_core::{CellPosition, CellRange, FontZoom};
use sumie_font::{FontError, FontStyle, FontSystem};
use sumie_pty::Size;
use sumie_render::{
    AtlasRect, AtlasUpload, Cursor, CursorShape, DrawList, GlyphQuad, PixelRect, Rgba, wgpu,
};
use sumie_vt::{CursorStyle, Rgb, Snapshot, codepoint_width};
use sumie_wayland::CursorRect;

use crate::Result;
use attributes::{Decorations, Ink, glyph_quad};

mod overlays;
use overlays::{Glyphs, SearchBox};
mod settings;
mod tabs;
pub(crate) use settings::FontSettings;
use tabs::Tabs;

pub(crate) struct View {
    font: FontSystem,
    font_settings: FontSettings,
    zoom_points: Option<f32>,
    font_errors: FontErrors,
    logical: [u32; 2],
    scale: u32,
    tabs: Tabs,
    pub(crate) device: [u32; 2],
    pub(crate) cell: [u32; 2],
    pub(crate) preedit: String,
    /// Formatted query and tab-separated count, without IME preedit.
    pub(crate) search_bar: Option<String>,
    /// Viewport row, inclusive first/last columns, and whether this is the current match.
    pub(crate) search_highlights: Vec<(u16, u16, u16, bool)>,
    search_box: Option<SearchBox>,
    presented_search: Option<overlays::PresentedSearch>,
    grid: [u32; 2],
    backgrounds: Vec<Rgba>,
    default_background: Rgba,
    default_backgrounds: Vec<bool>,
    images: std::sync::Arc<Vec<Placement>>,
    image_exclusions: [Option<PixelRect>; 2],
    selection_foreground: Option<Rgb>,
    selection_background: Option<Rgb>,
    glyphs: Glyphs,
    atlas_dirty: Vec<AtlasRect>,
    cursor: Option<Cursor>,
    line: Line,
    composition: Line,
}

#[derive(Default)]
struct Line {
    text: String,
    widths: Vec<u8>,
    shaped: sumie_font::ShapedRun,
}

#[derive(Default)]
struct FontErrors(HashSet<Discriminant<FontError>>);

impl FontErrors {
    fn warn(&mut self, error: &FontError) {
        if !self.0.insert(discriminant(error)) {
            return;
        }
        // A missing code point comes from terminal output or IME input.
        let reason: &dyn std::fmt::Display = if matches!(error, FontError::MissingGlyph(_)) {
            &"no font covers a character"
        } else {
            error
        };
        tracing::warn!(%reason, "could not draw terminal glyphs");
    }
}

impl View {
    pub(crate) fn new(
        scale: u32,
        logical: [u32; 2],
        device: [u32; 2],
        font_settings: FontSettings,
        colors: &sumie_config::Colors,
    ) -> Result<Self> {
        u16::try_from(device[0])?;
        u16::try_from(device[1])?;
        let font = font_settings.at_scale(scale, None)?;
        let cell = [font.metrics().width, font.metrics().height];
        let grid = grid_size(device, cell);
        Ok(Self {
            font,
            font_settings,
            zoom_points: None,
            font_errors: FontErrors::default(),
            scale,
            tabs: Tabs::default(),
            logical,
            device,
            cell,
            grid,
            preedit: String::new(),
            search_bar: None,
            search_highlights: Vec::new(),
            search_box: None,
            presented_search: None,
            backgrounds: Vec::new(),
            default_background: [0, 0, 0, 255],
            default_backgrounds: Vec::new(),
            images: std::sync::Arc::default(),
            image_exclusions: [None; 2],
            selection_foreground: colors.selection_foreground(),
            selection_background: colors.selection_background(),
            glyphs: Glyphs::default(),
            atlas_dirty: Vec::new(),
            cursor: None,
            line: Line::default(),
            composition: Line::default(),
        })
    }

    pub(crate) fn resize(&mut self, scale: u32, logical: [u32; 2], device: [u32; 2]) -> Result<()> {
        u16::try_from(device[0])?;
        u16::try_from(device[1])?;
        if scale != self.scale {
            let font = self.font_settings.at_scale(scale, self.zoom_points)?;
            self.tabs.rescale(scale)?;
            self.font = font;
            self.scale = scale;
            self.cell = [self.font.metrics().width, self.font.metrics().height];
        }
        self.logical = logical;
        self.device = device;
        self.resize_strip();
        self.grid = grid_size(self.terminal_device(), self.cell);
        self.reset_hits();
        self.search_box = None;
        Ok(())
    }

    /// Replace the font and its atlas at the current scale, leaving the view intact on error.
    pub(crate) fn set_font(&mut self, config: sumie_config::Font) -> Result<bool> {
        let settings = FontSettings::from_config(config)?;
        let changed = !settings.matches(&self.font_settings, self.zoom_points);
        if changed {
            self.replace_font(settings.at_scale(self.scale, None)?);
        }
        self.font_settings = settings;
        self.zoom_points = None;
        Ok(changed)
    }

    pub(crate) fn zoom(&mut self, action: FontZoom) -> Result<bool> {
        let points = self.font_settings.zoom(self.zoom_points, action);
        let changed = self.font_settings.px(points).to_bits()
            != self.font_settings.px(self.zoom_points).to_bits();
        if changed {
            self.replace_font(self.font_settings.at_scale(self.scale, points)?);
        }
        self.zoom_points = points;
        Ok(changed)
    }

    fn replace_font(&mut self, font: FontSystem) {
        let cell = [font.metrics().width, font.metrics().height];
        self.grid = grid_size(self.terminal_device(), cell);
        self.reset_hits();
        self.search_box = None;
        self.cell = cell;
        self.font = font;
    }

    pub(crate) fn set_selection_colors(&mut self, colors: &sumie_config::Colors) {
        self.selection_foreground = colors.selection_foreground();
        self.selection_background = colors.selection_background();
    }

    pub(crate) fn size(&self) -> Size {
        let [cols, rows] = self
            .grid
            .map(|value| u16::try_from(value).expect("validated grid"));
        let [width_px, height_px] = self
            .terminal_device()
            .map(|value| u16::try_from(value).expect("validated device size"));
        Size {
            cols,
            rows,
            width_px,
            height_px,
        }
    }

    pub(crate) fn pointer_position(&self, (x, y): (f64, f64)) -> (CellPosition, [f32; 2]) {
        // Viewporter stretches the rounded buffer to the exact logical size.
        let pos = self.window_pixel((x, y));
        let relative = [pos[0], pos[1] - f64::from(self.bar_height())];
        let pixel = relative.map(|pos| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "native mouse coordinates are f32"
            )]
            {
                pos as f32
            }
        });
        let cell = |axis: usize| {
            let index = f64::from(pixel[axis]) / f64::from(self.cell[axis]);
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                index.clamp(0.0, f64::from(self.grid[axis] - 1)) as u16
            }
        };
        (
            CellPosition {
                row: cell(1),
                column: cell(0),
            },
            pixel,
        )
    }

    pub(crate) fn link_cell(&self, position: (f64, f64), banner: bool) -> Option<CellPosition> {
        let (hit, [x, y]) = self.pointer_position(position);
        let [width, height] =
            std::array::from_fn::<_, 2, _>(|i| f64::from(self.grid[i] * self.cell[i]));
        (!self.over_tab_bar(position)
            && (0.0..width).contains(&f64::from(x))
            && (0.0..height).contains(&f64::from(y))
            && (!banner || hit.row != 0))
            .then_some(hit)
    }

    pub(crate) fn build(
        &mut self,
        snapshot: &Snapshot,
        selection: Option<CellRange>,
        hovered_link: &[CellRange],
        banner: Option<&str>,
        focused: bool,
    ) {
        self.backgrounds.clear();
        self.images.clone_from(&snapshot.images);
        self.default_background = rgba(snapshot.background);
        self.default_backgrounds.clear();
        self.glyphs.quads.clear();
        self.glyphs.offset = [0, self.bar_height().cast_signed()];
        self.set_overlay_bounds(banner.is_some());
        let overlay = self
            .search_box
            .is_none()
            .then(|| self.composition(snapshot))
            .flatten()
            .filter(|&(row, _, _)| row != 0 || banner.is_none());
        if let Some(message) = banner {
            self.draw_banner(snapshot, message);
            if !self.images.is_empty() {
                self.default_backgrounds
                    .resize(self.grid[0] as usize, false);
            }
        }
        self.draw_terminal(
            snapshot,
            selection,
            hovered_link,
            overlay,
            usize::from(banner.is_some())..self.grid[1] as usize,
        );
        if let Some((row, column, _)) = overlay {
            self.composition.draw(
                &mut self.font,
                &mut self.font_errors,
                &mut self.glyphs,
                [column, row],
                rgba(snapshot.foreground),
                FontStyle::default(),
            );
        }
        if let Some(search_box) = self.search_box {
            self.glyphs.cover = None;
            self.draw_search(snapshot, search_box, banner.is_some());
        } else {
            self.set_cursor(snapshot, overlay, banner.is_some(), focused);
        }
        collect_dirty(&self.font, &mut self.atlas_dirty);
    }

    fn draw_terminal(
        &mut self,
        snapshot: &Snapshot,
        selection: Option<CellRange>,
        mut hovered_link: &[CellRange],
        overlay: Option<(u32, u32, u32)>,
        visible: std::ops::Range<usize>,
    ) {
        let mut decorations = Decorations::default();
        let rows = (0..).zip(snapshot.rows.iter().take(visible.end));
        for (row, cells) in rows.skip(visible.start) {
            let selected = selection.and_then(|range| range.columns(row));
            let highlighted = |column| selected.as_ref().is_some_and(|cols| cols.contains(&column));
            let columns = overlay
                .filter(|&(y, _, _)| y == row)
                .map(|(_, x, width)| x..x + width);
            self.line.clear();
            let mut start = 0;
            let mut run = None;
            for (column, cell) in (0..).zip(cells) {
                let highlighted = highlighted(column);
                let covered = columns
                    .as_ref()
                    .is_some_and(|columns| columns.contains(&column));
                let (foreground, background) = if highlighted {
                    (
                        self.selection_foreground.unwrap_or(cell.background),
                        self.selection_background.unwrap_or(cell.foreground),
                    )
                } else {
                    (cell.foreground, cell.background)
                };
                self.backgrounds.push(rgba(if covered {
                    snapshot.background
                } else {
                    background
                }));
                if !self.images.is_empty() {
                    self.default_backgrounds
                        .push(cell.default_background && !highlighted && !covered);
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
                    let hovered = attributes::link_at(&mut hovered_link, row, column);
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

    fn set_cursor(
        &mut self,
        snapshot: &Snapshot,
        overlay: Option<(u32, u32, u32)>,
        banner: bool,
        focused: bool,
    ) {
        self.cursor = snapshot.cursor.and_then(|cursor| {
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
                if focused
                    && cursor.style == CursorStyle::Block
                    && column > 0
                    && cells[column as usize - 1].width == 2
                {
                    column -= 1;
                }
                let width = cells[column as usize].width.max(1);
                let shape = match (focused, cursor.style) {
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
        let available = self.grid[0].saturating_sub(column);
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

    pub(crate) fn draw_list(&self, cursor_hidden: bool) -> DrawList<'_> {
        DrawList {
            grid_size: self.grid,
            cell_size: self.cell,
            origin: [0, self.bar_height().cast_signed()],
            backgrounds: &self.backgrounds,
            images: &self.images,
            default_background: self.default_background,
            default_backgrounds: &self.default_backgrounds,
            image_exclusions: self.image_exclusions,
            glyphs: &self.glyphs.quads,
            cursor: self.cursor.filter(|_| !cursor_hidden),
            chrome: self.tabs.layer(),
        }
    }

    pub(crate) fn atlas(&self) -> AtlasUpload<'_> {
        upload(&self.font, &self.atlas_dirty)
    }

    pub(crate) fn atlas_uploaded(&mut self) {
        self.font.clear_dirty_regions();
    }

    fn origin(&self, [column, row]: [u32; 2]) -> [i32; 2] {
        [
            (column * self.cell[0]).cast_signed(),
            (self.bar_height() + row * self.cell[1]).cast_signed(),
        ]
    }

    pub(crate) fn ime_rectangle(&self, cursor: Option<sumie_vt::Cursor>) -> Option<CursorRect> {
        let [x, y] = if self.search_bar.is_some() {
            self.cursor?.rect.origin
        } else {
            let cursor = cursor?;
            self.origin([u32::from(cursor.column), u32::from(cursor.row)])
        };
        // Match the actual viewporter stretch, including buffers rounded at fractional scale.
        let bounds = |axis: usize, start: i32| -> Option<(i32, i32)> {
            let device = i64::from(self.device[axis]);
            if device == 0 {
                return None;
            }
            let logical = i64::from(self.logical[axis]);
            let start = i64::from(start);
            let left = start * logical / device;
            let right = ((start + i64::from(self.cell[axis])) * logical + device - 1) / device;
            Some((left.try_into().ok()?, (right - left).try_into().ok()?))
        };
        let (x, width) = bounds(0, x)?;
        let (y, height) = bounds(1, y)?;
        Some(CursorRect {
            x,
            y,
            width,
            height,
        })
    }
}

impl Line {
    fn clear(&mut self) {
        self.text.clear();
        self.widths.clear();
    }

    fn push(&mut self, cluster: &str, width: u8) {
        let cluster = if cluster.is_empty() { " " } else { cluster };
        self.text.push_str(cluster);
        self.widths.push(width);
        self.widths
            .extend(std::iter::repeat_n(0, cluster.chars().count() - 1));
    }

    fn trim_spaces(&mut self) {
        let len = self.text.trim_end_matches(' ').len();
        self.widths
            .truncate(self.widths.len() - (self.text.len() - len));
        self.text.truncate(len);
    }

    fn draw(
        &mut self,
        font: &mut FontSystem,
        errors: &mut FontErrors,
        glyphs: &mut Glyphs,
        [col, row]: [u32; 2],
        foreground: Rgba,
        face: FontStyle,
    ) {
        if self.text.is_empty() {
            return;
        }
        loop {
            match font.shape_styled_into(&self.text, &self.widths, face, &mut self.shaped) {
                Ok(()) => break,
                Err(error) => {
                    errors.warn(&error);
                    let FontError::MissingGlyph(value) = error else {
                        return;
                    };
                    let Some(ch) = char::from_u32(value).filter(|&ch| ch != '\u{fffd}') else {
                        return;
                    };
                    self.text = self.text.replace(ch, "\u{fffd}");
                }
            }
        }
        let metrics = font.metrics();
        let x = glyphs.offset[0] + (col * metrics.width).cast_signed();
        let y = glyphs.offset[1] + (row * metrics.height).cast_signed();
        for glyph in &self.shaped.glyphs {
            let image = match font.rasterize(glyph.key) {
                Ok(image) => image,
                Err(error) => {
                    errors.warn(&error);
                    continue;
                }
            };
            if image.rect.width != 0 && image.rect.height != 0 {
                glyphs.push(glyph_quad(
                    image,
                    [x + glyph.x, y + glyph.y],
                    [col + glyph.column, row],
                    foreground,
                ));
            }
        }
    }
}

fn grid_size([width, height]: [u32; 2], cell: [u32; 2]) -> [u32; 2] {
    [(width / cell[0]).max(1), (height / cell[1]).max(1)]
}

fn rgba([r, g, b]: Rgb) -> Rgba {
    [r, g, b, 255]
}

pub(crate) fn clear_color([r, g, b]: Rgb) -> wgpu::Color {
    let linear = |value| {
        let encoded = f64::from(value) / 255.0;
        if encoded <= 0.04045 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    };
    wgpu::Color {
        r: linear(r),
        g: linear(g),
        b: linear(b),
        a: 1.0,
    }
}
