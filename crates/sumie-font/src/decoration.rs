use std::f32::consts::{FRAC_1_SQRT_2, PI};

use skrifa::GlyphId;
use zeno::{Command, Fill, Mask, PathBuilder, Placement, Scratch, Stroke, Style};

use crate::{Atlas, AtlasGlyph, CellMetrics, FontError, FontSystem, GlyphKey, sprite};

/// One cell-wide mark, positioned relative to the top of the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoration {
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
    Strikethrough,
    Overline,
}

impl FontSystem {
    /// Returns a decoration mask in the glyph atlas, independent of text and face style.
    ///
    /// # Errors
    /// Returns an error if the atlas cannot grow to hold the mask.
    pub fn decoration(&mut self, mark: Decoration) -> Result<AtlasGlyph, FontError> {
        // Sprite codepoints fit in Unicode; decoration IDs follow them.
        let key = GlyphKey::new(sprite::FACE, GlyphId::new(0x11_0000 + mark as u32), 0.0);
        if let Some(image) = self.atlas.get(key) {
            return Ok(image);
        }
        self.rasterizer
            .decoration(mark, key, self.metrics, &mut self.atlas)
    }
}

impl Decoration {
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub(crate) fn draw(
        self,
        key: GlyphKey,
        metrics: CellMetrics,
        atlas: &mut Atlas,
        scratch: &mut Scratch,
        path: &mut Vec<Command>,
    ) -> Result<AtlasGlyph, FontError> {
        let line = if self == Self::Strikethrough {
            metrics.strike
        } else {
            metrics.underline
        };
        let w = metrics.width as f32;
        let t = line.thickness.min(metrics.height) as f32;
        path.clear();
        let mut style = Style::Fill(Fill::NonZero);
        let (offset, height) = match self {
            Self::Single | Self::Overline | Self::Strikethrough => {
                path.add_rect([0.0, 0.0], w, t);
                (0, t)
            }
            Self::Double => {
                let t = t.min((metrics.height / 3).max(1) as f32);
                path.add_rect([0.0, 0.0], w, t)
                    .add_rect([0.0, 2.0 * t], w, t);
                (t as u32, 3.0 * t)
            }
            Self::Dashed => {
                let dash = metrics.width / 3 + 1;
                for x in (0..metrics.width).step_by(2 * dash as usize) {
                    path.add_rect([x as f32, 0.0], dash.min(metrics.width - x) as f32, t);
                }
                (0, t)
            }
            Self::Dotted => {
                let r = t * FRAC_1_SQRT_2;
                let count = (w / (4.0 * r))
                    .ceil()
                    .min((w / (3.0 * r)).floor())
                    .min((w / (2.0 * r + 1.0)).floor())
                    .max(1.0);
                let offset = (r - 0.5 * t).ceil();
                for i in 0..count as u32 {
                    path.add_circle([(i as f32 + 0.5) * w / count, offset + 0.5 * t], r);
                }
                (offset as u32, offset + 0.5 * t + r)
            }
            Self::Curly => {
                let offset = (t / 2.0).ceil();
                // Preserve the font's underline position when the descender space is short.
                let descent = metrics.height.saturating_sub(line.top) as f32;
                let amplitude = (w / PI).min((descent - t * 0.5).max(t));
                let low = offset + amplitude;
                // Ghostty uses two cubic segments per column, with a crest at its center.
                path.move_to([0.0, low])
                    .curve_to([0.2 * w, low], [0.3 * w, offset], [0.5 * w, offset])
                    .curve_to([0.7 * w, offset], [0.8 * w, low], [w, low]);
                style = Stroke::new(t).into();
                (offset as u32, low + 0.5 * t)
            }
        };
        let height = (height.ceil() as u32).min(metrics.height);
        let top = if self == Self::Overline {
            0
        } else {
            line.top.saturating_sub(offset).min(metrics.height - height)
        };
        let placement = Placement {
            left: 0,
            top: top.cast_signed(),
            width: metrics.width,
            height,
        };
        atlas.insert(key, placement, |pixels, pitch| {
            Mask::with_scratch(&*path, scratch)
                .style(style)
                .size(metrics.width, height)
                .render_into(pixels, Some(pitch));
        })
    }
}
