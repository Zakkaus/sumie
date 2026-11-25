mod atlas;
mod attributes;
use atlas::{collect_dirty, upload};

use std::{
    collections::HashSet,
    mem::{Discriminant, discriminant},
};

use sumie_core::graphics::ImageSource;
use sumie_core::splits::Rect;
use sumie_core::{CellRange, FontZoom};
use sumie_font::{FontError, FontStyle, FontSystem};
#[cfg(test)]
use sumie_render::DrawList;
use sumie_render::{
    AtlasRect, AtlasUpload, Fill, FrameDraw, PaneDraw, PixelRect, Rgba, Scroll, wgpu,
};
use sumie_ui::Palette;
use sumie_vt::{Rgb, Snapshot};

use crate::{
    Result,
    platform::{CaretBounds, LogicalRect},
};
pub(crate) use attributes::LinkHighlight;
pub(crate) use paint::PaneFocus;
use paint::{PanePaint, PanePainter};

mod geometry;
pub(crate) use geometry::surface_logical_rect;
use geometry::{grid_size, scaled_padding};
mod images;
use images::{PresentedImages, ViewerPaint};
mod overlays;
use overlays::{Glyphs, PresentedSearch};
mod edge;
mod paint;
mod settings;
mod tabs;
#[cfg(test)]
mod tests;
pub(crate) use settings::FontSettings;
use tabs::Tabs;

struct WindowSize {
    scale: u32,
    logical: [u32; 2],
    device: [u32; 2],
}

pub(crate) struct View {
    font: FontSystem,
    font_settings: FontSettings,
    zoom_points: Option<f32>,
    font_errors: FontErrors,
    logical: [u32; 2],
    scale: u32,
    pending_resize: Option<WindowSize>,
    padding: [u16; 2],
    device_padding: [u32; 2],
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
    presented_images: PresentedImages,
    panes: Vec<PanePaint>,
    pane_count: usize,
    active_pane: Option<usize>,
    fills: Vec<Fill>,
    frame_background: Rgba,
    background_alpha: u8,
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

/// Distinct missing code points whose errors are logged; terminal output can show many more.
const MISSING_GLYPH_WARNINGS: usize = 32;

#[derive(Default)]
struct FontErrors {
    kinds: HashSet<Discriminant<FontError>>,
    missing: HashSet<u32>,
}

impl FontErrors {
    fn warn(&mut self, error: &FontError) {
        let first = match error {
            // The first error for a code point lists the fonts that fallback rejected.
            FontError::MissingGlyph { codepoint, .. } => {
                self.missing.len() < MISSING_GLYPH_WARNINGS && self.missing.insert(*codepoint)
            }
            _ => self.kinds.insert(discriminant(error)),
        };
        if first {
            tracing::warn!(reason = %error, "could not draw terminal glyphs");
        }
    }
}

impl View {
    pub(crate) fn new(
        scale: u32,
        logical: [u32; 2],
        device: [u32; 2],
        font_settings: FontSettings,
        colors: &sumie_config::Colors,
        padding: [u16; 2],
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
            pending_resize: None,
            padding,
            device_padding: scaled_padding(padding, scale),
            tabs: Tabs::default(),
            logical,
            device,
            cell,
            preedit: String::new(),
            search_bar: None,
            search_selection: 0..0,
            search_highlights: Vec::new(),
            presented_search: None,
            presented_images: PresentedImages::default(),
            panes: Vec::new(),
            pane_count: 0,
            active_pane: None,
            fills: Vec::new(),
            frame_background: [0, 0, 0, 255],
            background_alpha: 255,
            palette: Palette::for_luminance(0.0),
            selection_foreground: colors.selection_foreground(),
            selection_background: colors.selection_background(),
            atlas_dirty: Vec::new(),
            line: Line::default(),
            composition: Line::default(),
        })
    }

    pub(crate) const fn scale(&self) -> u32 {
        self.scale
    }

    pub(crate) fn request_resize(&mut self, scale: u32, logical: [u32; 2], device: [u32; 2]) {
        self.pending_resize = Some(WindowSize {
            scale,
            logical,
            device,
        });
        self.reset_hits();
        self.presented_images.clear();
    }

    pub(crate) const fn resize_pending(&self) -> bool {
        self.pending_resize.is_some()
    }

    pub(crate) fn apply_pending_resize(&mut self) -> Result<bool> {
        let Some(WindowSize {
            scale,
            logical,
            device,
        }) = self.pending_resize.take()
        else {
            return Ok(false);
        };
        u16::try_from(device[0])?;
        u16::try_from(device[1])?;
        if scale != self.scale {
            let font = self.font_settings.at_scale(scale, self.zoom_points)?;
            self.tabs.rescale(scale)?;
            self.font = font;
            self.scale = scale;
            self.device_padding = scaled_padding(self.padding, scale);
            self.cell = [self.font.metrics().width, self.font.metrics().height];
            self.invalidate_paint();
        }
        self.logical = logical;
        self.device = device;
        self.resize_strip();
        Ok(true)
    }

    pub(crate) fn set_padding(&mut self, padding: [u16; 2]) {
        self.padding = padding;
        self.device_padding = scaled_padding(padding, self.scale);
        self.presented_search = None;
        self.presented_images.clear();
        self.invalidate_paint();
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
        self.presented_images.clear();
        self.invalidate_paint();
        self.cell = [font.metrics().width, font.metrics().height];
        self.font = font;
    }

    pub(crate) fn set_selection_colors(&mut self, colors: &sumie_config::Colors) {
        self.selection_foreground = colors.selection_foreground();
        self.selection_background = colors.selection_background();
        self.invalidate_paint();
    }

    pub(crate) fn set_background_alpha(&mut self, alpha: u8) -> bool {
        if alpha == self.background_alpha {
            return false;
        }
        self.background_alpha = alpha;
        self.invalidate_paint();
        true
    }

    fn invalidate_paint(&mut self) {
        for pane in &mut self.panes {
            pane.id = None;
        }
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
        self.frame_background[3] = self.background_alpha;
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
        hovered_link: LinkHighlight<'_>,
        banner: Option<&str>,
        focus: PaneFocus<'_>,
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
        // Moving within a row only changes the offset; the extra row needs a repaint to appear.
        let extra = focus.scroll != 0 && snapshot.below.is_some();
        let scroll = Scroll {
            offset: if extra { focus.scroll } else { 0 },
            extra_rows: u32::from(extra),
        };
        let pane = &mut self.panes[index];
        let repaint = pane.scroll.extra_rows != scroll.extra_rows;
        pane.scroll = scroll;
        let focused = focus.active && focus.window_focused;
        if !focus.dirty
            && !repaint
            && pane.id == Some(focus.id)
            && pane.rect == rect
            && pane.active == focus.active
            && pane.focused == focused
        {
            return;
        }
        let grid = self.pane_grid(rect);
        let pane = &mut self.panes[index];
        pane.id = Some(focus.id);
        pane.rect = rect;
        pane.grid = grid_size(grid, self.cell);
        pane.glyphs.offset = [grid.x.cast_signed(), grid.y.cast_signed()];
        pane.active = focus.active;
        pane.focused = focused;
        pane.viewer = focus
            .viewer
            .and_then(|p| ViewerPaint::new(p, grid, self.scale));
        PanePainter {
            pane,
            font: &mut self.font,
            font_errors: &mut self.font_errors,
            line: &mut self.line,
            composition: &mut self.composition,
            cell: self.cell,
            background_alpha: self.background_alpha,
            link_hint: self.palette.link_hint(),
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
            pane.viewer = None;
        }
        collect_dirty(&self.font, &mut self.atlas_dirty);
    }

    pub(crate) fn frame<'a>(
        &'a self,
        cursor_hidden: bool,
        image_source: impl Clone + Fn(u64) -> Option<&'a dyn ImageSource>,
    ) -> FrameDraw<'a, impl Clone + Iterator<Item = PaneDraw<'a>>> {
        let backdrop = self.palette.overlay;
        FrameDraw {
            background: self.frame_background,
            panes: self.panes[..self.pane_count].iter().map(move |pane| {
                let mut draw = pane.draw_list(self.cell, cursor_hidden);
                if !draw.images.is_empty() || pane.viewer.is_some() {
                    draw.image_source = pane.id.and_then(&image_source);
                }
                PaneDraw {
                    rect: pane.rect,
                    draw,
                    thumbnails: self.thumbnail_chrome(pane),
                    viewer: pane.viewer.as_ref().map(|viewer| viewer.draw(backdrop)),
                }
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
            LinkHighlight {
                ranges: hovered_link,
                control: true,
            },
            banner,
            PaneFocus {
                active: true,
                window_focused: focused,
                id: 0,
                dirty: true,
                viewer: None,
                scroll: 0,
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
        project: impl FnOnce(CaretBounds) -> Option<LogicalRect>,
    ) -> Option<LogicalRect> {
        let pane = &self.panes[self.active_pane?];
        if pane.rect != rect {
            return None;
        }
        let [x, y] = if pane.search_box.is_some() {
            pane.cursor?.rect.origin
        } else {
            let cursor = cursor?;
            [
                pane.glyphs.offset[0] + (u32::from(cursor.column) * self.cell[0]).cast_signed(),
                pane.glyphs.offset[1] + (u32::from(cursor.row) * self.cell[1]).cast_signed()
                    - pane.scroll.offset.cast_signed(),
            ]
        };
        let ends = [
            i64::from(rect.x + rect.width),
            i64::from(rect.y + rect.height),
        ];
        let start = [x, y];
        let end =
            std::array::from_fn(|i| (i64::from(start[i]) + i64::from(self.cell[i])).min(ends[i]));
        if (0..2).any(|i| i64::from(start[i]) >= end[i]) {
            return None;
        }
        project(CaretBounds {
            start,
            end,
            logical: self.logical,
            device: self.device,
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
                    let FontError::MissingGlyph {
                        codepoint: value, ..
                    } = error
                    else {
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
                glyphs.push(attributes::glyph_quad(
                    image,
                    [x + glyph.x, y + glyph.y],
                    [col + glyph.column, row],
                    foreground,
                ));
            }
        }
    }
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
