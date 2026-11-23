use std::{
    borrow::Cow,
    collections::HashSet,
    mem::{Discriminant, discriminant},
};

use sumie_core::{CellPosition, CellRange};
use sumie_font::{FontError, FontOptions, FontSystem, Hinting};
use sumie_pty::Size;
use sumie_render::{
    AtlasRect, AtlasUpload, Cursor, CursorShape, DrawList, GlyphQuad, PixelRect, Rgba, wgpu,
};
use sumie_vt::{Rgb, Snapshot, codepoint_width};
use sumie_wayland::CursorRect;

use crate::Result;

pub(crate) struct View {
    font: FontSystem,
    font_settings: FontSettings,
    font_errors: FontErrors,
    logical: [u32; 2],
    scale: u32,
    pub(crate) device: [u32; 2],
    pub(crate) cell: [u32; 2],
    pub(crate) preedit: String,
    grid: [u32; 2],
    backgrounds: Vec<Rgba>,
    selection_foreground: Option<Rgb>,
    selection_background: Option<Rgb>,
    glyphs: Vec<GlyphQuad>,
    atlas_dirty: Vec<AtlasRect>,
    cursor: Option<Cursor>,
    line: Line,
    composition: Line,
}

#[derive(Default)]
struct Line {
    text: String,
    widths: Vec<u8>,
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
        let font = font_settings.at_scale(scale)?;
        let cell = [font.metrics().width, font.metrics().height];
        let grid = grid_size(device, cell)?;
        Ok(Self {
            font,
            font_settings,
            font_errors: FontErrors::default(),
            scale,
            logical,
            device,
            cell,
            grid,
            preedit: String::new(),
            backgrounds: Vec::new(),
            selection_foreground: colors.selection_foreground(),
            selection_background: colors.selection_background(),
            glyphs: Vec::new(),
            atlas_dirty: Vec::new(),
            cursor: None,
            line: Line::default(),
            composition: Line::default(),
        })
    }

    pub(crate) fn resize(&mut self, scale: u32, logical: [u32; 2], device: [u32; 2]) -> Result<()> {
        if scale != self.scale {
            self.font = self.font_settings.at_scale(scale)?;
            self.scale = scale;
            self.cell = [self.font.metrics().width, self.font.metrics().height];
        }
        self.logical = logical;
        self.device = device;
        self.grid = grid_size(device, self.cell)?;
        Ok(())
    }

    /// Replace the font and its atlas at the current scale, leaving the view intact on error.
    pub(crate) fn set_font(&mut self, config: sumie_config::Font) -> Result<bool> {
        let settings = FontSettings::from_config(config)?;
        if settings.same_as(&self.font_settings) {
            return Ok(false);
        }
        let font = settings.at_scale(self.scale)?;
        let cell = [font.metrics().width, font.metrics().height];
        self.grid = grid_size(self.device, cell)?;
        self.cell = cell;
        self.font = font;
        self.font_settings = settings;
        Ok(true)
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
            .device
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
        let pixel = std::array::from_fn(|axis| {
            let pos = [x, y][axis] * f64::from(self.device[axis]) / f64::from(self.logical[axis]);
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

    pub(crate) fn build(
        &mut self,
        snapshot: &Snapshot,
        selection: Option<CellRange>,
        banner: Option<&str>,
        focused: bool,
    ) {
        self.backgrounds.clear();
        self.glyphs.clear();
        let overlay = self
            .composition(snapshot)
            .filter(|&(row, _, _)| row != 0 || banner.is_none());
        if let Some(message) = banner {
            self.draw_banner(snapshot, message);
        }
        let rows = (0..).zip(&snapshot.rows);
        for (row, cells) in rows.skip(usize::from(banner.is_some())) {
            let selected = selection.and_then(|range| range.columns(row));
            let highlighted = |column| selected.as_ref().is_some_and(|cols| cols.contains(&column));
            let columns = overlay
                .filter(|&(y, _, _)| y == row)
                .map(|(_, x, width)| x..x + width);
            self.line.clear();
            let mut start = 0;
            let mut run = None;
            let mut draw = |line: &mut Line, column, color| {
                line.trim_spaces();
                line.draw(
                    &mut self.font,
                    &mut self.font_errors,
                    &mut self.glyphs,
                    [column, row],
                    color,
                );
                line.clear();
            };
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
                if cell.width == 0 {
                    continue;
                }
                let color = rgba(foreground);
                if run != Some((highlighted, color)) {
                    if let Some((_, previous)) = run {
                        draw(&mut self.line, start, previous);
                    }
                    start = column;
                    run = Some((highlighted, color));
                }
                let intersects = columns.as_ref().is_some_and(|columns| {
                    columns.start < column + u32::from(cell.width) && column < columns.end
                });
                self.line
                    .push(if intersects { "" } else { &cell.text }, cell.width);
            }
            if let Some((_, color)) = run {
                draw(&mut self.line, start, color);
            }
        }
        if let Some((row, column, _)) = overlay {
            self.composition.draw(
                &mut self.font,
                &mut self.font_errors,
                &mut self.glyphs,
                [column, row],
                rgba(snapshot.foreground),
            );
        }
        self.set_cursor(snapshot, overlay, banner.is_some(), focused);
        self.collect_atlas_dirty();
    }

    fn draw_banner(&mut self, snapshot: &Snapshot, message: &str) {
        self.backgrounds.extend(std::iter::repeat_n(
            rgba(snapshot.foreground),
            self.grid[0] as usize,
        ));
        self.line.ellipsize(message, self.grid[0]);
        self.line.draw(
            &mut self.font,
            &mut self.font_errors,
            &mut self.glyphs,
            [0, 0],
            rgba(snapshot.background),
        );
    }

    fn collect_atlas_dirty(&mut self) {
        self.atlas_dirty.clear();
        self.atlas_dirty.extend(
            self.font
                .atlas()
                .dirty_regions()
                .iter()
                .map(|rect| AtlasRect {
                    origin: [rect.x, rect.y],
                    size: [rect.width, rect.height],
                }),
        );
    }

    fn set_cursor(
        &mut self,
        snapshot: &Snapshot,
        overlay: Option<(u32, u32, u32)>,
        banner: bool,
        focused: bool,
    ) {
        self.cursor = snapshot.cursor.and_then(|cursor| {
            if banner && cursor.row == 0 {
                return None;
            }
            let [column, row] = [u32::from(cursor.column), u32::from(cursor.row)];
            let [x, y] = self.origin([column, row]);
            let (rect, shape) = if let Some((_, _, width)) = overlay {
                (
                    PixelRect {
                        origin: [x, y + self.cell[1].cast_signed() - 2],
                        size: [width * self.cell[0], 2],
                    },
                    CursorShape::Block,
                )
            } else if cursor.visible {
                let shape = if focused {
                    match cursor.style {
                        sumie_vt::CursorStyle::Block => CursorShape::Block,
                        sumie_vt::CursorStyle::Bar => CursorShape::Bar,
                        sumie_vt::CursorStyle::Underline => CursorShape::Underline,
                        sumie_vt::CursorStyle::Hollow => CursorShape::Hollow,
                    }
                } else {
                    CursorShape::Hollow
                };
                let width = snapshot.rows[row as usize][column as usize].width.max(1);
                (
                    PixelRect {
                        origin: [x, y],
                        size: [u32::from(width) * self.cell[0], self.cell[1]],
                    },
                    shape,
                )
            } else {
                return None;
            };
            Some(Cursor {
                rect,
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
            origin: [0, 0],
            backgrounds: &self.backgrounds,
            glyphs: &self.glyphs,
            cursor: self.cursor.filter(|_| !cursor_hidden),
        }
    }

    pub(crate) fn atlas(&self) -> AtlasUpload<'_> {
        let atlas = self.font.atlas();
        let [width, height] = atlas.dimensions();
        AtlasUpload {
            width,
            height,
            bytes: atlas.pixels(),
            dirty: &self.atlas_dirty,
        }
    }

    pub(crate) fn atlas_uploaded(&mut self) {
        self.font.clear_dirty_regions();
    }

    fn origin(&self, [column, row]: [u32; 2]) -> [i32; 2] {
        [
            (column * self.cell[0]).cast_signed(),
            (row * self.cell[1]).cast_signed(),
        ]
    }

    pub(crate) fn ime_rectangle(&self, cursor: sumie_vt::Cursor) -> CursorRect {
        let [x, y] = self.origin([u32::from(cursor.column), u32::from(cursor.row)]);
        let scale = self.scale.cast_signed();
        let left = x * 120 / scale;
        let top = y * 120 / scale;
        let right = ((x + self.cell[0].cast_signed()) * 120 + scale - 1) / scale;
        let bottom = ((y + self.cell[1].cast_signed()) * 120 + scale - 1) / scale;
        CursorRect {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
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

    fn ellipsize(&mut self, text: &str, columns: u32) {
        self.clear();
        let mut cells = 0;
        let mut last = (0, 0);
        for ch in text.chars() {
            let ch = if ch.is_control() { ' ' } else { ch };
            let width = codepoint_width(ch);
            if cells + u32::from(width) > columns {
                if cells == columns {
                    self.text.truncate(last.0);
                    self.widths.truncate(last.1);
                }
                self.push("…", 1);
                break;
            }
            if width != 0 {
                last = (self.text.len(), self.widths.len());
            } else if cells == 0 {
                continue;
            }
            self.text.push(ch);
            self.widths.push(width);
            cells += u32::from(width);
        }
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
        glyphs: &mut Vec<GlyphQuad>,
        [col, row]: [u32; 2],
        foreground: Rgba,
    ) {
        if self.text.is_empty() {
            return;
        }
        let run = loop {
            match font.shape(&self.text, &self.widths) {
                Ok(run) => break run,
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
        };
        let metrics = font.metrics();
        let x = (col * metrics.width).cast_signed();
        let y = (row * metrics.height).cast_signed();
        for glyph in run.glyphs {
            let image = match font.rasterize(glyph.key) {
                Ok(image) => image,
                Err(error) => {
                    errors.warn(&error);
                    continue;
                }
            };
            let rect = image.rect;
            if rect.width != 0 && rect.height != 0 {
                let column = glyph.column + col;
                let size = [rect.width, rect.height];
                glyphs.push(GlyphQuad {
                    atlas: AtlasRect {
                        origin: [rect.x, rect.y],
                        size,
                    },
                    dest: PixelRect {
                        origin: [x + glyph.x + image.left, y + glyph.y + image.top],
                        size,
                    },
                    color: foreground,
                    cell: [column, row],
                });
            }
        }
    }
}

fn grid_size(device: [u32; 2], cell: [u32; 2]) -> Result<[u32; 2]> {
    let [width, height] = device;
    u16::try_from(width)?;
    u16::try_from(height)?;
    Ok([(width / cell[0]).max(1), (height / cell[1]).max(1)])
}

pub(crate) struct FontSettings {
    family: Cow<'static, str>,
    px: f32,
    options: FontOptions,
}

impl FontSettings {
    pub(crate) fn from_config(config: sumie_config::Font) -> Result<Self> {
        let px = std::env::var("SUMIE_FONT_PX")
            .map_or_else(|_| Ok(config.logical_px()), |px| px.parse::<f32>())?;
        let hinting = match std::env::var("SUMIE_FONT_HINTING").as_deref() {
            Err(std::env::VarError::NotPresent) => match config.hinting() {
                sumie_config::Hinting::Off => Hinting::Off,
                sumie_config::Hinting::Light => Hinting::Light,
                sumie_config::Hinting::Normal => Hinting::Normal,
            },
            Ok("off") => Hinting::Off,
            Ok("light") => Hinting::Light,
            Ok("normal") => Hinting::Normal,
            _ => return Err("SUMIE_FONT_HINTING must be off, light, or normal".into()),
        };
        let weight = match std::env::var("SUMIE_FONT_WEIGHT") {
            Err(std::env::VarError::NotPresent) => config.weight(),
            value => Some(value?.parse().map_err(|_| FontError::InvalidWeight)?),
        };
        let family = std::env::var("SUMIE_FONT").map_or_else(|_| config.into_family(), Cow::Owned);
        Ok(Self {
            family,
            px,
            options: FontOptions { hinting, weight },
        })
    }

    fn same_as(&self, other: &Self) -> bool {
        self.family == other.family
            && self.px.to_bits() == other.px.to_bits()
            && self.options.weight == other.options.weight
            && discriminant(&self.options.hinting) == discriminant(&other.options.hinting)
    }

    fn at_scale(&self, scale: u32) -> Result<FontSystem> {
        let scale_units = u16::try_from(scale)?;
        let font = FontSystem::new(
            &self.family,
            self.px,
            f32::from(scale_units) / 120.0,
            self.options,
        )?;
        let metrics = font.metrics();
        tracing::info!(
            family = font.family(font.primary_face()),
            scale_units,
            cell_width = metrics.width,
            cell_height = metrics.height,
            "terminal font"
        );
        Ok(font)
    }
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
