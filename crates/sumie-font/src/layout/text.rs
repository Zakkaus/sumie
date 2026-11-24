use skrifa::GlyphId;

use crate::{CellMetrics, Face, FontError, FontStyle, FontSystem, GlyphKey, sprite};

use super::{Input, Layout, shape_segment};

/// An outline origin in a proportional text run, in device pixels from its top left.
#[derive(Clone, Copy, Debug)]
pub struct TextGlyph {
    pub key: GlyphKey,
    pub x: i32,
    pub y: i32,
}

#[derive(Default)]
pub struct TextRun {
    pub glyphs: Vec<TextGlyph>,
    // Each pair is the pen position and exclusive glyph index after a complete cluster.
    clusters: Vec<(f64, usize)>,
    advance: f64,
}

impl TextRun {
    #[must_use]
    pub fn width(&self) -> u32 {
        ceil_width(self.advance)
    }

    /// Keep as many whole shaping clusters as fit, reserving space for a suffix if clipped.
    /// Returns the prefix glyphs, optional suffix origin, and total displayed advance.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "run advance is bounded during shaping"
    )]
    pub fn fit(&self, width: u32, suffix: &Self) -> (&[TextGlyph], Option<i32>, u32) {
        let full = self.width();
        if full <= width {
            return (&self.glyphs, None, full);
        }
        if suffix.width() > width {
            return (&[], None, 0);
        }
        let available = f64::from(width) - suffix.advance;
        let (advance, count) = self
            .clusters
            .iter()
            .rfind(|&&(x, _)| x <= available)
            .copied()
            .unwrap_or_default();
        (
            &self.glyphs[..count],
            Some(advance.round() as i32),
            ceil_width(advance + suffix.advance),
        )
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "run advance is bounded during shaping"
)]
fn ceil_width(advance: f64) -> u32 {
    advance.ceil().max(0.0) as u32
}

impl FontSystem {
    /// Shape ordered left-to-right text with natural advances. Pass graphemes in display order;
    /// the font may merge them further (for example into a ligature).
    ///
    /// # Errors
    /// Returns an error on missing coverage, failed discovery or shaping, or coordinate overflow.
    /// The output is cleared on error.
    pub fn shape_text_into<'a>(
        &mut self,
        graphemes: impl Iterator<Item = &'a str>,
        output: &mut TextRun,
    ) -> Result<(), FontError> {
        output.clear();
        self.layout.input.clear();
        let style = FontStyle::default();
        let mut face = self.primary_face();
        for (index, grapheme) in graphemes.enumerate() {
            let mut continuation = false;
            for ch in grapheme.chars() {
                let Some(next) = self.face_for_continuation(ch, face, style)? else {
                    continue;
                };
                face = next;
                self.layout.input.push(Input {
                    ch,
                    byte: index,
                    column: 0,
                    width: 0,
                    continuation,
                    face,
                });
                continuation = true;
            }
        }
        if u32::try_from(self.layout.input.len()).is_err() {
            return Err(FontError::RunTooLong);
        }
        self.layout
            .shape_text(&self.faces, self.metrics, output)
            .inspect_err(|_| output.clear())
    }
}

impl Layout {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "bounded, rounded device pixel placement"
    )]
    fn shape_text(
        &mut self,
        faces: &[Face],
        metrics: CellMetrics,
        output: &mut TextRun,
    ) -> Result<(), FontError> {
        let ppem = faces[0].ppem;
        let mut start = 0;
        let mut last = None;
        while let Some(first) = self.input.get(start) {
            let end = if first.face == sprite::FACE {
                output.glyphs.push(TextGlyph {
                    key: GlyphKey::new(sprite::FACE, GlyphId::new(u32::from(first.ch)), ppem),
                    x: output.advance.round() as i32,
                    y: 0,
                });
                output.advance += f64::from(metrics.width);
                output.record_cluster(&mut last, first.byte);
                start + 1
            } else {
                let end = start
                    + 1
                    + self.input[start + 1..]
                        .iter()
                        .take_while(|next| next.face == first.face)
                        .count();
                let input = &self.input[start..end];
                let font = &faces[first.face.0].font;
                shape_segment(&mut self.buffer, &mut self.plans, font, input)?;
                let scale = f64::from(ppem) / f64::from(font.units_per_em());
                for (info, pos) in self
                    .buffer
                    .glyph_infos()
                    .iter()
                    .zip(self.buffer.glyph_positions())
                {
                    let x = output.advance + f64::from(pos.x_offset) * scale;
                    output.glyphs.push(TextGlyph {
                        key: GlyphKey::new(first.face, GlyphId::new(info.glyph_id), ppem),
                        x: x.round() as i32,
                        y: metrics.baseline - (f64::from(pos.y_offset) * scale).round() as i32,
                    });
                    output.advance += f64::from(pos.x_advance) * scale;
                    output.record_cluster(&mut last, input[info.cluster as usize].byte);
                }
                end
            };
            if output.advance.abs() > f64::from(i32::MAX / 2) {
                return Err(FontError::RunTooLong);
            }
            start = end;
        }
        Ok(())
    }
}

impl TextRun {
    fn clear(&mut self) {
        self.glyphs.clear();
        self.clusters.clear();
        self.advance = 0.0;
    }

    fn record_cluster(&mut self, last: &mut Option<usize>, cluster: usize) {
        let end = (self.advance, self.glyphs.len());
        if last.replace(cluster) == Some(cluster) {
            *self.clusters.last_mut().expect("cluster already recorded") = end;
        } else {
            self.clusters.push(end);
        }
    }
}
