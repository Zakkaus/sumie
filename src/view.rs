use std::{
    collections::HashSet,
    mem::{Discriminant, discriminant},
};

use sumie_font::{FontError, FontSystem};
use sumie_pty::Size;
use sumie_render::{AtlasRect, AtlasUpload, Cursor, DrawList, GlyphQuad, PixelRect, Rgba, wgpu};
use sumie_vt::{Rgb, Snapshot, codepoint_width};
use sumie_wayland::CursorRect;

use crate::Result;

pub(crate) struct View {
    font: FontSystem,
    font_errors: FontErrors,
    scale: u32,
    pub(crate) device: [u32; 2],
    pub(crate) cell: [u32; 2],
    pub(crate) preedit: String,
    grid: [u32; 2],
    backgrounds: Vec<Rgba>,
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
    pub(crate) fn new(scale: u32, device: [u32; 2]) -> Result<Self> {
        let font = font_at_scale(scale)?;
        let cell = [font.metrics().width, font.metrics().height];
        let grid = grid_size(device, cell)?;
        Ok(Self {
            font,
            font_errors: FontErrors::default(),
            scale,
            device,
            cell,
            grid,
            preedit: String::new(),
            backgrounds: Vec::new(),
            glyphs: Vec::new(),
            atlas_dirty: Vec::new(),
            cursor: None,
            line: Line::default(),
            composition: Line::default(),
        })
    }

    pub(crate) fn resize(&mut self, scale: u32, device: [u32; 2]) -> Result<()> {
        if scale != self.scale {
            self.font = font_at_scale(scale)?;
            self.scale = scale;
            self.cell = [self.font.metrics().width, self.font.metrics().height];
        }
        self.device = device;
        self.grid = grid_size(device, self.cell)?;
        Ok(())
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

    pub(crate) fn build(&mut self, snapshot: &Snapshot) {
        self.backgrounds.clear();
        self.glyphs.clear();
        let overlay = self.composition(snapshot);
        for (row, cells) in (0..).zip(&snapshot.rows) {
            let columns = overlay
                .filter(|&(y, _, _)| y == row)
                .map(|(_, x, width)| x..x + width);
            self.line.clear();
            for (column, cell) in (0..).zip(cells) {
                let covered = columns
                    .as_ref()
                    .is_some_and(|columns| columns.contains(&column));
                self.backgrounds.push(rgba(if covered {
                    snapshot.background
                } else {
                    cell.background
                }));
                if cell.width == 0 {
                    continue;
                }
                let intersects = columns.as_ref().is_some_and(|columns| {
                    columns.start < column + u32::from(cell.width) && column < columns.end
                });
                self.line
                    .push(if intersects { "" } else { &cell.text }, cell.width);
            }
            self.line.trim_spaces();
            self.line.draw(
                &mut self.font,
                &mut self.font_errors,
                &mut self.glyphs,
                [0, row],
                |column| rgba(cells[column as usize].foreground),
            );
        }
        if let Some((row, column, _)) = overlay {
            self.composition.draw(
                &mut self.font,
                &mut self.font_errors,
                &mut self.glyphs,
                [column, row],
                |_| rgba(snapshot.foreground),
            );
        }
        self.cursor = snapshot.cursor.and_then(|cursor| {
            let [column, row] = [u32::from(cursor.column), u32::from(cursor.row)];
            let [x, y] = self.origin([column, row]);
            let rect = if let Some((_, _, width)) = overlay {
                PixelRect {
                    origin: [x, y + self.cell[1].cast_signed() - 2],
                    size: [width * self.cell[0], 2],
                }
            } else if cursor.visible {
                PixelRect {
                    origin: [x, y],
                    size: [2, self.cell[1]],
                }
            } else {
                return None;
            };
            Some(Cursor {
                rect,
                color: rgba(cursor.color),
            })
        });
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

    pub(crate) fn draw_list(&self) -> DrawList<'_> {
        DrawList {
            grid_size: self.grid,
            cell_size: self.cell,
            origin: [0, 0],
            backgrounds: &self.backgrounds,
            glyphs: &self.glyphs,
            cursor: self.cursor,
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
        foreground: impl Fn(u32) -> Rgba,
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
                    color: foreground(column),
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

fn font_at_scale(scale: u32) -> Result<FontSystem> {
    let scale_units = u16::try_from(scale)?;
    // P0 comparison overrides until the TOML config lands in P1.
    let family = std::env::var("SUMIE_FONT").unwrap_or_else(|_| "JetBrains Mono".into());
    let px = std::env::var("SUMIE_FONT_PX").map_or(Ok(20.0), |px| px.parse::<f32>())?;
    let font = FontSystem::new(&family, px, f32::from(scale_units) / 120.0)?;
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
