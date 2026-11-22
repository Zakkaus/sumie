use harfrust::{Buffer, BufferFlags, Direction, Feature, GlyphInfo, GlyphPosition, ShapeOptions};
use skrifa::{Font, GlyphId, Tag};
use zeno::{Bounds, Point};

use crate::{CellMetrics, Face, FaceId, FontError, FontSystem, GlyphKey};

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

#[derive(Debug)]
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
    /// coverage, or a failed discovery or shaping operation.
    pub fn shape(&mut self, text: &str, widths: &[u8]) -> Result<ShapedRun, FontError> {
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
                let Some(next_face) = self.face_for_continuation(ch, face)? else {
                    continue;
                };
                face = next_face;
            } else {
                face = self.face_for(ch)?;
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
        self.layout.shape(&self.faces, self.metrics, columns)
    }

    fn face_for_continuation(
        &mut self,
        ch: char,
        previous: FaceId,
    ) -> Result<Option<FaceId>, FontError> {
        match ch {
            // Pass shaping controls to HarfRust without requiring a cmap entry.
            '\u{034f}'
            | '\u{180b}'..='\u{180d}'
            | '\u{180f}'
            | '\u{200c}'
            | '\u{200d}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{e0100}'..='\u{e01ef}' => Ok(Some(previous)),
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
            _ => self.face_for(ch).map(Some),
        }
    }
}

impl Layout {
    fn shape(
        &mut self,
        faces: &[Face],
        metrics: CellMetrics,
        columns: u32,
    ) -> Result<ShapedRun, FontError> {
        let mut glyphs = Vec::with_capacity(self.input.len());
        let mut start = 0;
        while start < self.input.len() {
            let first = &self.input[start];
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
            shape_segment(&mut self.buffer, &face.font, &self.input[start..end])?;
            let end_column = self.input.get(end).map_or(columns, |next| {
                // A run of detached marks starts in the preceding run's last cell.
                next.column + u32::from(next.continuation) * u32::from(next.width)
            });
            let placement = Segment { face, metrics };
            placement.place(
                &self.buffer,
                &self.input[start..end],
                end_column,
                &mut glyphs,
            );
            start = end;
        }
        Ok(ShapedRun {
            cells: columns,
            glyphs,
        })
    }
}

fn shape_segment(buffer: &mut Buffer, font: &Font, input: &[Input]) -> Result<(), FontError> {
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
    let font = harfrust::ShaperFont::new(font);
    harfrust::shape(&font, buffer, ShapeOptions::new().features(&features))
        .map_err(FontError::Shape)
}

struct Segment<'a> {
    face: &'a Face,
    metrics: CellMetrics,
}

impl Segment<'_> {
    #[allow(clippy::cast_possible_truncation)]
    fn place(
        &self,
        buffer: &Buffer,
        input: &[Input],
        end_column: u32,
        glyphs: &mut Vec<ShapedGlyph>,
    ) {
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
            let (ppem, dx, baseline) = if centered {
                self.center_ink(cell, &infos[start..end], &positions[start..end])
            } else {
                (self.face.ppem, 0, self.metrics.baseline)
            };
            let scale = ppem / f32::from(self.face.font.units_per_em());
            if !infos[start].unsafe_to_break() || centered {
                pen = 0;
            }
            let mut relative = pen;
            for (info, pos) in infos[start..end].iter().zip(&positions[start..end]) {
                glyphs.push(ShapedGlyph {
                    key: GlyphKey::new(cell.face, GlyphId::new(info.glyph_id), ppem),
                    cluster: cell.byte,
                    column: cell.column,
                    cells,
                    x: anchor + dx + px(relative + pos.x_offset, scale).round() as i32,
                    y: baseline - px(pos.y_offset, scale).round() as i32,
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
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn center_ink(
        &self,
        cell: &Input,
        infos: &[GlyphInfo],
        positions: &[GlyphPosition],
    ) -> (f32, i32, i32) {
        let width = (u32::from(cell.width) * self.metrics.width) as f32;
        let height = self.metrics.height as f32;
        let mut ppem = self.face.ppem;
        let mut bounds = ink_bounds(&self.face.font, ppem, infos, positions);
        if bounds.width() > width || bounds.height() > height {
            // Leave one pixel for the two ends to round outwards.
            ppem *= (width / (bounds.width() + 1.0)).min(height / (bounds.height() + 1.0));
            bounds = ink_bounds(&self.face.font, ppem, infos, positions);
        }
        let x = ((width - bounds.width()) * 0.5 - bounds.min.x).round() as i32;
        let y = if cell.continuation {
            self.metrics.baseline
        } else {
            ((height - bounds.height()) * 0.5 - bounds.min.y).round() as i32
        };
        (ppem, x, y)
    }
}

fn ink_bounds(font: &Font, ppem: f32, infos: &[GlyphInfo], positions: &[GlyphPosition]) -> Bounds {
    let scale = ppem / f32::from(font.units_per_em());
    let metrics = font.glyph_metrics().scaled_f32(ppem);
    let mut pen = 0;
    let corners = infos.iter().zip(positions).filter_map(|(info, pos)| {
        let x = px(pen + pos.x_offset, scale).round();
        let y = -px(pos.y_offset, scale).round();
        pen += pos.x_advance;
        let ink = metrics.extents(GlyphId::new(info.glyph_id))?;
        if ink.width <= 0.0 || ink.height <= 0.0 {
            return None;
        }
        let x = x + ink.x_bearing;
        let y = y - ink.y_bearing;
        Some([
            Point::new(x.floor(), y.floor()),
            Point::new((x + ink.width).ceil(), (y + ink.height).ceil()),
        ])
    });
    Bounds::from_points(corners.flatten())
}

#[allow(clippy::cast_precision_loss)]
fn px(units: i32, scale: f32) -> f32 {
    units as f32 * scale
}
