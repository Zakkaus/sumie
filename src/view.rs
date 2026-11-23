mod atlas;
mod attributes;
use atlas::{collect_dirty, upload};

use std::{
    collections::HashSet,
    mem::{Discriminant, discriminant},
};

use sumie_core::splits::Rect;
use sumie_core::{CellPosition, CellRange, FontZoom};
use sumie_font::{FontError, FontStyle, FontSystem};
use sumie_pty::Size;
#[cfg(test)]
use sumie_render::DrawList;
use sumie_render::{AtlasRect, AtlasUpload, Fill, FrameDraw, PaneDraw, PixelRect, Rgba, wgpu};
use sumie_ui::Palette;
use sumie_vt::{Rgb, Snapshot};
use sumie_wayland::CursorRect;

use crate::Result;
use attributes::glyph_quad;
use paint::{PanePaint, PanePainter};

mod overlays;
use overlays::{Glyphs, PresentedSearch};
mod paint;
mod settings;
mod tabs;
#[cfg(test)]
mod tests;
pub(crate) use settings::FontSettings;
use tabs::Tabs;

#[derive(Clone, Copy)]
pub(crate) struct PaneFocus {
    pub(crate) active: bool,
    pub(crate) window_focused: bool,
    pub(crate) id: u64,
    pub(crate) dirty: bool,
}

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
    /// Byte range in the formatted query. An empty range is the insertion point.
    pub(crate) search_selection: std::ops::Range<usize>,
    /// Viewport row, inclusive first/last columns, and whether this is the current match.
    pub(crate) search_highlights: Vec<(u16, u16, u16, bool)>,
    presented_search: Option<PresentedSearch>,
    panes: Vec<PanePaint>,
    pane_count: usize,
    active_pane: Option<usize>,
    fills: Vec<Fill>,
    frame_background: Rgba,
    palette: Palette,
    selection_foreground: Option<Rgb>,
    selection_background: Option<Rgb>,
    atlas_dirty: Vec<AtlasRect>,
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
            preedit: String::new(),
            search_bar: None,
            search_selection: 0..0,
            search_highlights: Vec::new(),
            presented_search: None,
            panes: Vec::new(),
            pane_count: 0,
            active_pane: None,
            fills: Vec::new(),
            frame_background: [0, 0, 0, 255],
            palette: Palette::for_luminance(0.0),
            selection_foreground: colors.selection_foreground(),
            selection_background: colors.selection_background(),
            atlas_dirty: Vec::new(),
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
            self.invalidate_paint();
        }
        self.logical = logical;
        self.device = device;
        self.resize_strip();
        self.reset_hits();
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
        self.reset_hits();
        self.invalidate_paint();
        self.cell = [font.metrics().width, font.metrics().height];
        self.font = font;
    }

    pub(crate) fn set_selection_colors(&mut self, colors: &sumie_config::Colors) {
        self.selection_foreground = colors.selection_foreground();
        self.selection_background = colors.selection_background();
        self.invalidate_paint();
    }

    fn invalidate_paint(&mut self) {
        for pane in &mut self.panes {
            pane.id = None;
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

    pub(crate) fn pane_size(&self, rect: Rect) -> Size {
        let size = [rect.width, rect.height];
        let [cols, rows] =
            grid_size(size, self.cell).map(|value| u16::try_from(value).expect("validated grid"));
        let [width_px, height_px] =
            size.map(|value| u16::try_from(value).expect("validated device size"));
        Size {
            cols,
            rows,
            width_px,
            height_px,
        }
    }

    pub(crate) fn pointer_position(
        &self,
        position: (f64, f64),
        rect: Rect,
    ) -> (CellPosition, [f32; 2]) {
        let relative = self.pane_pixel(position, rect);
        let grid = grid_size([rect.width, rect.height], self.cell);
        let pixel = relative.map(|pos| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "native mouse coordinates are f32"
            )]
            {
                pos as f32
            }
        });
        (self.pointer_cell(relative, grid), pixel)
    }

    fn pane_pixel(&self, position: (f64, f64), rect: Rect) -> [f64; 2] {
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
        position: (f64, f64),
        rect: Rect,
        banner: bool,
    ) -> Option<CellPosition> {
        let pixel = self.pane_pixel(position, rect);
        let size = [rect.width, rect.height];
        let grid = grid_size(size, self.cell);
        let inside = (0..2).all(|i| {
            let end = f64::from(size[i].min(grid[i] * self.cell[i]));
            (0.0..end).contains(&pixel[i])
        });
        if !inside || self.over_tab_bar(position) {
            return None;
        }
        let hit = self.pointer_cell(pixel, grid);
        (!banner || hit.row != 0).then_some(hit)
    }

    /// Start a composition on the event-loop thread. Panes follow in layout order.
    pub(crate) fn begin_frame(
        &mut self,
        dividers: impl IntoIterator<Item = Rect>,
        background: Rgb,
    ) {
        self.pane_count = 0;
        self.active_pane = None;
        self.fills.clear();
        self.frame_background = rgba(background);
        self.palette = palette_for_background(background);
        self.fills.extend(dividers.into_iter().map(|rect| Fill {
            rect: pixel_rect(rect),
            color: self.palette.rule,
        }));
    }

    /// Compose one viewport or retain its clean paint. Supply focused overlays before building.
    pub(crate) fn build_pane(
        &mut self,
        rect: Rect,
        snapshot: &Snapshot,
        selection: Option<CellRange>,
        hovered_link: &[CellRange],
        banner: Option<&str>,
        focus: PaneFocus,
    ) {
        let index = self.pane_count;
        self.pane_count += 1;
        if focus.active {
            self.active_pane = Some(index);
        } else {
            self.fills.push(Fill {
                rect: pixel_rect(rect),
                color: self.palette.inactive_pane,
            });
        }
        if index == self.panes.len() {
            self.panes.push(PanePaint::new(rect));
        }
        let pane = &mut self.panes[index];
        let focused = focus.active && focus.window_focused;
        if !focus.dirty
            && pane.id == Some(focus.id)
            && pane.rect == rect
            && pane.active == focus.active
            && pane.focused == focused
        {
            return;
        }
        pane.id = Some(focus.id);
        pane.rect = rect;
        pane.active = focus.active;
        pane.focused = focused;
        PanePainter {
            pane,
            font: &mut self.font,
            font_errors: &mut self.font_errors,
            line: &mut self.line,
            composition: &mut self.composition,
            cell: self.cell,
            selection_foreground: self.selection_foreground,
            selection_background: self.selection_background,
            search_bar: self.search_bar.as_deref().filter(|_| focus.active),
            search_selection: self.search_selection.clone(),
            search_highlights: if focus.active {
                &self.search_highlights
            } else {
                &[]
            },
            preedit: if focus.active { &self.preedit } else { "" },
        }
        .build(
            snapshot,
            selection,
            hovered_link,
            banner.filter(|_| focus.active),
        );
    }

    /// Collect atlas damage after the last pane; then upload before drawing the frame.
    pub(crate) fn end_frame(&mut self) {
        for pane in &mut self.panes[self.pane_count..] {
            pane.id = None;
            pane.images = None;
        }
        collect_dirty(&self.font, &mut self.atlas_dirty);
    }

    pub(crate) fn frame(
        &self,
        cursor_hidden: bool,
    ) -> FrameDraw<'_, impl Clone + Iterator<Item = PaneDraw<'_>>> {
        FrameDraw {
            background: self.frame_background,
            panes: self.panes[..self.pane_count]
                .iter()
                .map(move |pane| PaneDraw {
                    rect: pane.rect,
                    draw: pane.draw_list(self.cell, cursor_hidden),
                }),
            fills: &self.fills,
            chrome: self.tabs.layer(),
        }
    }

    #[cfg(test)]
    pub(crate) fn build(
        &mut self,
        snapshot: &Snapshot,
        selection: Option<CellRange>,
        hovered_link: &[CellRange],
        banner: Option<&str>,
        focused: bool,
    ) {
        self.begin_frame([], snapshot.background);
        self.build_pane(
            self.content_rect(),
            snapshot,
            selection,
            hovered_link,
            banner,
            PaneFocus {
                active: true,
                window_focused: focused,
                id: 0,
                dirty: true,
            },
        );
        self.end_frame();
    }

    #[cfg(test)]
    pub(crate) fn draw_list(&self, cursor_hidden: bool) -> DrawList<'_> {
        let mut draw = self.panes[0].draw_list(self.cell, cursor_hidden);
        draw.chrome = self.tabs.layer();
        draw
    }

    pub(crate) fn atlas(&self) -> AtlasUpload<'_> {
        upload(&self.font, &self.atlas_dirty)
    }

    pub(crate) fn atlas_uploaded(&mut self) {
        self.font.clear_dirty_regions();
    }

    pub(crate) fn ime_rectangle(
        &self,
        cursor: Option<sumie_vt::Cursor>,
        rect: Rect,
    ) -> Option<CursorRect> {
        let pane = &self.panes[self.active_pane?];
        if pane.rect != rect {
            return None;
        }
        let [x, y] = if pane.search_box.is_some() {
            pane.cursor?.rect.origin
        } else {
            let cursor = cursor?;
            [
                (rect.x + u32::from(cursor.column) * self.cell[0]).cast_signed(),
                (rect.y + u32::from(cursor.row) * self.cell[1]).cast_signed(),
            ]
        };
        let ends = [
            i64::from(rect.x + rect.width),
            i64::from(rect.y + rect.height),
        ];
        // Match the actual viewporter stretch, including buffers rounded at fractional scale.
        let bounds = |axis: usize, start: i32| -> Option<(i32, i32)> {
            let device = i64::from(self.device[axis]);
            if device == 0 {
                return None;
            }
            let logical = i64::from(self.logical[axis]);
            let start = i64::from(start);
            let left = start * logical / device;
            let end = (start + i64::from(self.cell[axis])).min(ends[axis]);
            if start >= end {
                return None;
            }
            let right = (end * logical + device - 1) / device;
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

fn pixel_rect(rect: Rect) -> PixelRect {
    PixelRect {
        origin: [rect.x.cast_signed(), rect.y.cast_signed()],
        size: [rect.width, rect.height],
    }
}

fn palette_for_background(background: Rgb) -> Palette {
    let c = clear_color(background);
    Palette::for_luminance(0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b)
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
