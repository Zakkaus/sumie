use std::collections::HashMap;

use harfrust::{
    Buffer, BufferFlags, Direction, Feature, GlyphInfo, GlyphPosition, Script, ShapeOptions,
    ShapePlan, ShaperFont,
};
use skrifa::{Font, GlyphId, Tag};
use zeno::{Bounds, Point};

use crate::raster::Rasterizer;
use crate::{Atlas, CellMetrics, Face, FaceId, FontError, FontStyle, FontSystem, GlyphKey, sprite};

mod text;

pub use text::{TextGlyph, TextRun};

/// Glyph placement in an input run, in integer device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapedGlyph {
    pub key: GlyphKey,
    /// UTF-8 byte offset of the shaping cluster in the input text.
    pub cluster: usize,
    /// First grid column and span of the cluster, shared by all its glyphs.
    pub column: u32,
    pub cells: u32,
    /// Glyph origin, measured right and down from the run's top-left corner.
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Default)]
pub struct ShapedRun {
    pub cells: u32,
    pub glyphs: Vec<ShapedGlyph>,
}

struct Input {
    ch: char,
    byte: usize,
    column: u32,
    width: u8,
    continuation: bool,
    face: FaceId,
}

#[derive(Default)]
pub(crate) struct Layout {
    buffer: Buffer,
    input: Vec<Input>,
    // Face IDs distinguish variation instances; direction, language and features are fixed.
    plans: HashMap<(FaceId, Option<Script>), ShapePlan>,
}

impl FontSystem {
    /// Shapes an ordered left-to-right run with `calt` and `liga` enabled.
    ///
    /// Pass one width per Unicode scalar: 1 or 2 for the first scalar of a cell,
    /// 0 for continuations. Use the grid's widths rather than recomputing them.
    /// Continuations share the preceding base's grid span, including across faces.
    /// Zero-width default ignorables do not require coverage or emit their own glyph.
    ///
    /// # Errors
    /// Returns an error for mismatched widths, an unrepresentable grid, missing font
    /// coverage, or a failed discovery, shaping or wide-glyph outline operation.
    pub fn shape(&mut self, text: &str, widths: &[u8]) -> Result<ShapedRun, FontError> {
        self.shape_styled(text, widths, FontStyle::default())
    }

    /// Shapes a styled run using the same grid widths and ligatures as [`Self::shape`].
    ///
    /// # Errors
    /// Returns the same errors as [`Self::shape`], including failures to load a styled face.
    pub fn shape_styled(
        &mut self,
        text: &str,
        widths: &[u8],
        style: FontStyle,
    ) -> Result<ShapedRun, FontError> {
        let mut output = ShapedRun::default();
        self.shape_styled_into(text, widths, style, &mut output)?;
        Ok(output)
    }

    /// Shapes a styled run into reusable output, replacing its previous contents.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::shape_styled`] and leaves `output` empty on error.
    pub fn shape_styled_into(
        &mut self,
        text: &str,
        widths: &[u8],
        style: FontStyle,
        output: &mut ShapedRun,
    ) -> Result<(), FontError> {
        output.cells = 0;
        output.glyphs.clear();
        u32::try_from(text.len()).map_err(|_| FontError::RunTooLong)?;
        if widths.first() == Some(&0) || widths.len() != text.chars().count() {
            return Err(FontError::InvalidWidths);
        }
        let mut columns = 0_u32;
        let mut base = (0, 0, 0);
        let mut face = self.primary_face();
        self.layout.input.clear();
        for ((byte, ch), &width) in text.char_indices().zip(widths) {
            if width > 2 {
                return Err(FontError::InvalidWidths);
            }
            let continuation = width == 0;
            if continuation {
                let Some(next_face) = self.face_for_continuation(ch, face, style)? else {
                    continue;
                };
                face = next_face;
            } else {
                face = self.face_for(ch, style)?;
                base = (byte, columns, width);
                columns = columns
                    .checked_add(u32::from(width))
                    .ok_or(FontError::RunTooLong)?;
            }
            let (byte, column, width) = base;
            self.layout.input.push(Input {
                ch,
                byte,
                column,
                width,
                continuation,
                face,
            });
        }
        if i64::from(columns) * i64::from(self.metrics.width) > i64::from(i32::MAX / 2) {
            return Err(FontError::RunTooLong);
        }
        self.layout
            .shape(
                &self.faces,
                self.metrics,
                columns,
                &mut self.rasterizer,
                &self.atlas,
                &mut output.glyphs,
            )
            .inspect_err(|_| output.glyphs.clear())?;
        output.cells = columns;
        Ok(())
    }

    fn face_for_continuation(
        &mut self,
        ch: char,
        previous: FaceId,
        style: FontStyle,
    ) -> Result<Option<FaceId>, FontError> {
        match ch {
            // Pass shaping controls to HarfRust without requiring a cmap entry.
            '\u{034f}'
            | '\u{180b}'..='\u{180d}'
            | '\u{180f}'
            | '\u{200c}'
            | '\u{200d}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{e0100}'..='\u{e01ef}' => Ok((previous != sprite::FACE).then_some(previous)),
            // Remaining Default_Ignorable_Code_Point ranges, Unicode 17.0.
            '\u{00ad}'
            | '\u{061c}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180e}'
            | '\u{200b}'
            | '\u{200e}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff0}'..='\u{fff8}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e00ff}'
            | '\u{e01f0}'..='\u{e0fff}' => Ok(None),
            _ => self.face_for(ch, style).map(Some),
        }
    }
}

impl Layout {
    fn shape(
        &mut self,
        faces: &[Face],
        metrics: CellMetrics,
        columns: u32,
        rasterizer: &mut Rasterizer,
        atlas: &Atlas,
        glyphs: &mut Vec<ShapedGlyph>,
    ) -> Result<(), FontError> {
        glyphs.reserve(self.input.len());
        let mut start = 0;
        while start < self.input.len() {
            let first = &self.input[start];
            if first.face == sprite::FACE {
                glyphs.push(ShapedGlyph {
                    key: GlyphKey::new(
                        sprite::FACE,
                        GlyphId::new(u32::from(first.ch)),
                        faces[0].ppem,
                    ),
                    cluster: first.byte,
                    column: first.column,
                    cells: u32::from(first.width),
                    x: (first.column * metrics.width).cast_signed(),
                    y: 0,
                });
                start += 1;
                continue;
            }
            let end = start
                + 1
                + self.input[start + 1..]
                    .iter()
                    .take_while(|next| {
                        next.face == first.face
                            && (next.column == first.column
                                || (!first.continuation && first.width == 1 && next.width == 1))
                    })
                    .count();
            let face = &faces[first.face.0];
            let input = &self.input[start..end];
            shape_segment(&mut self.buffer, &mut self.plans, &face.font, input)?;
            let end_column = self.input.get(end).map_or(columns, |next| {
                // A run of detached marks starts in the preceding run's last cell.
                next.column + u32::from(next.continuation) * u32::from(next.width)
            });
            let mut placement = Segment {
                face,
                metrics,
                rasterizer,
                atlas,
            };
            placement.place(&self.buffer, input, end_column, glyphs)?;
            start = end;
        }
        Ok(())
    }
}

fn shape_segment(
    buffer: &mut Buffer,
    plans: &mut HashMap<(FaceId, Option<Script>), ShapePlan>,
    font: &Font,
    input: &[Input],
) -> Result<(), FontError> {
    buffer.clear();
    buffer.set_direction(Direction::LeftToRight);
    let mut flags = BufferFlags::REMOVE_DEFAULT_IGNORABLES;
    if input[0].continuation {
        flags |= BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE;
    }
    buffer.set_flags(flags);
    let mut cluster = 0;
    for (index, cell) in (0_u32..).zip(input) {
        if !cell.continuation {
            cluster = index;
        }
        buffer.push(u32::from(cell.ch), cluster);
    }
    buffer.guess_segment_properties();
    let features = [
        Feature::new(Tag::new(b"calt"), 1, ..),
        Feature::new(Tag::new(b"liga"), 1, ..),
    ];
    let script = buffer.script();
    let plan = plans.entry((input[0].face, script)).or_insert_with(|| {
        ShapePlan::new(
            font,
            buffer.direction(),
            script,
            buffer.language(),
            &features,
        )
    });
    harfrust::shape(
        &ShaperFont::new(font),
        buffer,
        ShapeOptions::new().plan(Some(plan)).features(&features),
    )
    .map_err(FontError::Shape)
}

struct Segment<'a> {
    face: &'a Face,
    metrics: CellMetrics,
    rasterizer: &'a mut Rasterizer,
    atlas: &'a Atlas,
}

impl Segment<'_> {
    #[allow(clippy::cast_possible_truncation)]
    fn place(
        &mut self,
        buffer: &Buffer,
        input: &[Input],
        end_column: u32,
        glyphs: &mut Vec<ShapedGlyph>,
    ) -> Result<(), FontError> {
        let infos = buffer.glyph_infos();
        let positions = buffer.glyph_positions();
        let mut pen = 0;
        let mut start = 0;
        while start < infos.len() {
            let cluster = infos[start].cluster;
            let end = start
                + infos[start..]
                    .iter()
                    .take_while(|info| info.cluster == cluster)
                    .count();
            let cell = &input[cluster as usize];
            let next_column = infos
                .get(end)
                .map_or(end_column, |next| input[next.cluster as usize].column);
            let cells = next_column - cell.column;
            let anchor = (cell.column * self.metrics.width).cast_signed();
            let centered = cell.width == 2 || cell.continuation;
            let (ppem, dx) = if centered {
                self.fit_cluster(cell, &infos[start..end], &positions[start..end])?
            } else {
                (self.face.ppem, 0)
            };
            let scale = ppem / f32::from(self.face.font.units_per_em());
            if !infos[start].unsafe_to_break() || centered {
                pen = 0;
            }
            let mut relative = pen;
            for (info, pos) in infos[start..end].iter().zip(&positions[start..end]) {
                let id = GlyphId::new(info.glyph_id);
                let mut key = GlyphKey::new(cell.face, id, ppem);
                if crate::raster::png_strike(&self.face.font, id, self.metrics.height).is_some() {
                    key.color_width = cells.max(1) * self.metrics.width;
                }
                let (x, y) = glyph_position(
                    key,
                    dx,
                    self.face.baseline,
                    relative + pos.x_offset,
                    pos.y_offset,
                    scale,
                );
                glyphs.push(ShapedGlyph {
                    key,
                    cluster: cell.byte,
                    column: cell.column,
                    cells,
                    x: anchor + x,
                    y,
                });
                relative += pos.x_advance;
            }
            // Contextual substitutions can keep distinct cluster IDs. Keep their
            // relative advances without letting them shift the following grid anchor.
            #[allow(clippy::cast_precision_loss)]
            let grid_advance = (cells * self.metrics.width) as f32;
            pen = relative - (grid_advance / scale).round() as i32;
            start = end;
        }
        Ok(())
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn fit_cluster(
        &mut self,
        cell: &Input,
        infos: &[GlyphInfo],
        positions: &[GlyphPosition],
    ) -> Result<(f32, i32), FontError> {
        let width = (u32::from(cell.width) * self.metrics.width) as f32;
        let height = self.metrics.height as f32;
        let mut ppem = self.face.ppem;
        let baseline = self.face.baseline as f32;
        let bounds = loop {
            let bounds = self.ink_bounds(cell.face, ppem, infos, positions)?;
            // Hinting and whole-pixel mask rounding can keep the ink oversized after a reduction.
            let fit = fit_axis(bounds.width(), width)
                .min(fit_axis(-bounds.min.y, baseline))
                .min(fit_axis(bounds.max.y, height - baseline));
            if fit >= 1.0 {
                break bounds;
            }
            let smaller = ppem * fit;
            if smaller <= 0.0 || smaller >= ppem {
                break bounds;
            }
            ppem = smaller;
        };
        // Keep the original advance and bearings when the mask fits at that placement.
        if !cell.continuation && ppem.to_bits() == self.face.ppem.to_bits() {
            let units = positions.iter().map(|pos| pos.x_advance).sum();
            let advance = px(units, ppem / f32::from(self.face.font.units_per_em()));
            let x = ((width - advance) * 0.5).round();
            if bounds.min.x + x >= 0.0 && bounds.max.x + x <= width {
                return Ok((ppem, x as i32));
            }
        }
        let x = ((width - bounds.width()) * 0.5 - bounds.min.x).round() as i32;
        Ok((ppem, x))
    }

    #[allow(clippy::cast_precision_loss)]
    fn ink_bounds(
        &mut self,
        face: FaceId,
        ppem: f32,
        infos: &[GlyphInfo],
        positions: &[GlyphPosition],
    ) -> Result<Bounds, FontError> {
        let scale = ppem / f32::from(self.face.font.units_per_em());
        let mut pen = 0;
        let mut min = Point::new(f32::INFINITY, f32::INFINITY);
        let mut max = Point::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
        for (info, pos) in infos.iter().zip(positions) {
            let x = px(pen + pos.x_offset, scale).round();
            let y = -px(pos.y_offset, scale).round();
            pen += pos.x_advance;
            let key = GlyphKey::new(face, GlyphId::new(info.glyph_id), ppem);
            let (left, top, width, height) = if let Some(mask) = self.atlas.get(key) {
                (mask.left, mask.top, mask.rect.width, mask.rect.height)
            } else if crate::raster::png_strike(&self.face.font, key.glyph, self.metrics.height)
                .is_some()
            {
                continue;
            } else {
                let (_, mask) = self.rasterizer.prepare(self.face, key)?;
                (mask.left, mask.top, mask.width, mask.height)
            };
            if width == 0 || height == 0 {
                continue;
            }
            let x = x + left as f32;
            let y = y + top as f32;
            min = Point::new(min.x.min(x), min.y.min(y));
            max = Point::new(max.x.max(x + width as f32), max.y.max(y + height as f32));
        }
        let bounds = Bounds::new(min, max);
        Ok(if bounds.is_empty() {
            Bounds::default()
        } else {
            bounds
        })
    }
}

#[allow(clippy::cast_possible_truncation)]
fn glyph_position(key: GlyphKey, dx: i32, baseline: i32, x: i32, y: i32, scale: f32) -> (i32, i32) {
    if key.is_color() {
        (0, 0)
    } else {
        (
            dx + px(x, scale).round() as i32,
            baseline - px(y, scale).round() as i32,
        )
    }
}

fn fit_axis(extent: f32, available: f32) -> f32 {
    if extent > available && available > 0.0 {
        available / extent
    } else {
        1.0
    }
}

#[allow(clippy::cast_precision_loss)]
fn px(units: i32, scale: f32) -> f32 {
    units as f32 * scale
}
