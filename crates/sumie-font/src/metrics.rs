use skrifa::Font;

use crate::FontError;

/// Grid geometry in physical pixels. The baseline is measured down from the cell top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    pub baseline: i32,
}

impl CellMetrics {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub(crate) fn measure(font: &Font, ppem: f32) -> Result<Self, FontError> {
        let width = advance(font, '0', ppem).ok_or(FontError::MissingMetrics)?;
        let line = font
            .metrics()
            .scaled_f32(ppem)
            .h_line()
            .ok_or(FontError::MissingMetrics)?;
        let height = line.ascender - line.descender + line.line_gap;
        if height <= 0.0 {
            return Err(FontError::MissingMetrics);
        }
        Ok(Self {
            width: width.round().max(1.0) as u32,
            height: height.round().max(1.0) as u32,
            baseline: (line.ascender + line.line_gap * 0.5).round() as i32,
        })
    }

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn fallback_ppem(self, font: &Font, ppem: f32) -> f32 {
        advance(font, '水', 1.0).map_or(ppem, |ic_width| 2.0 * self.width as f32 / ic_width)
    }
}

fn advance(font: &Font, ch: char, ppem: f32) -> Option<f32> {
    let glyph = font.charmap().map_unicode(ch)?;
    let width = font.glyph_metrics().scaled_f32(ppem).h_advance(glyph);
    (width > 0.0).then_some(width)
}
