use skrifa::Font;

use crate::FontError;

/// Grid geometry in physical pixels. The baseline is measured down from the cell top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    pub baseline: i32,
    pub(crate) underline: StrokeMetrics,
    pub(crate) strike: StrokeMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StrokeMetrics {
    pub top: u32,
    pub thickness: u32,
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
        let baseline = (line.ascender + line.line_gap * 0.5).round();
        let styles = font.style_metrics().scaled_f32(ppem);
        let snap = |offset: f32, thickness: f32| StrokeMetrics {
            top: (baseline - offset).round().max(0.0) as u32,
            thickness: thickness.round().max(1.0) as u32,
        };
        let underline = styles
            .underline()
            .map_or_else(|| snap(-1.0, 1.0), |m| snap(m.position, m.thickness));
        let strike = styles.strike_through().map_or_else(
            || snap(line.ascender / 3.0, 1.0),
            |m| snap(m.position, m.thickness),
        );
        Ok(Self {
            width: width.round().max(1.0) as u32,
            height: height.round().max(1.0) as u32,
            baseline: baseline as i32,
            underline,
            strike,
        })
    }

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn fallback_ppem(self, font: &Font, primary_ppem: f32) -> f32 {
        let width_ppem = ['水', '한', 'あ', 'ア']
            .into_iter()
            .find_map(|ch| advance(font, ch, 1.0))
            .map_or(primary_ppem, |advance| 2.0 * self.width as f32 / advance);
        let mut ppem = primary_ppem.min(width_ppem);
        if let Some((ascender, descender)) = fallback_extents(font, 1.0) {
            ppem = ppem.min(self.height as f32 / (ascender - descender));
        }
        ppem
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    pub(crate) fn fallback_baseline(self, font: &Font, ppem: f32) -> i32 {
        fallback_extents(font, ppem).map_or(self.baseline, |(ascender, descender)| {
            ((self.height as f32 + ascender + descender) * 0.5).round() as i32
        })
    }
}

// A CJK font's hhea line can include space for accents outside the ideographic em.
// Size and center the same typographic line rather than charging that space to each glyph.
fn fallback_extents(font: &Font, ppem: f32) -> Option<(f32, f32)> {
    let metrics = font.metrics().scaled_f32(ppem);
    let line = metrics
        .typo_line()
        .filter(|line| line.ascender > line.descender)
        .or_else(|| metrics.h_line())?;
    (line.ascender > line.descender).then_some((line.ascender, line.descender))
}

fn advance(font: &Font, ch: char, ppem: f32) -> Option<f32> {
    let glyph = font.charmap().map_unicode(ch)?;
    let width = font.glyph_metrics().scaled_f32(ppem).h_advance(glyph);
    (width > 0.0).then_some(width)
}
