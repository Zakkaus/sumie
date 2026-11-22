use skrifa::{DrawMemory, Font, PathSink, ScalerFont};
use zeno::{Command, Fill, Mask, Origin, Placement, Scratch};

use crate::{Atlas, AtlasGlyph, FontError, GlyphKey};

#[derive(Default)]
pub(crate) struct Rasterizer {
    outline_memory: Vec<u8>,
    path: Path,
    scratch: Scratch,
}

impl Rasterizer {
    pub fn draw(
        &mut self,
        font: &Font,
        key: GlyphKey,
        atlas: &mut Atlas,
    ) -> Result<AtlasGlyph, FontError> {
        let glyph = key.glyph;
        let ppem = key.ppem();
        // TODO(P1): Composite COLR and color bitmaps into a separate RGBA atlas.
        if font.color_glyphs().get(glyph).is_some()
            || font
                .bitmap_strikes()
                .select_bitmap(glyph, Some(ppem))
                .is_some()
        {
            return Err(FontError::UnsupportedGlyph);
        }
        let scaler = ScalerFont::force_unhinted(font.clone(), Some(ppem));
        let Some(outline) = scaler.outline(glyph) else {
            return Ok(atlas.insert_blank(key));
        };
        self.outline_memory
            .resize(outline.required_memory_size(), 0);
        self.path.0.clear();
        outline
            .draw(DrawMemory::Buffer(&mut self.outline_memory), &mut self.path)
            .map_err(FontError::Outline)?;
        let bounds = self.scratch.bounds(&self.path.0, Fill::NonZero, None);
        let (offset, placement) = Placement::compute(Origin::TopLeft, [0.0, 0.0], &bounds);
        if placement.width == 0 || placement.height == 0 {
            return Ok(atlas.insert_blank(key));
        }
        atlas.insert(key, placement, |pixels, pitch| {
            Mask::with_scratch(&self.path.0, &mut self.scratch)
                .size(placement.width, placement.height)
                .offset(offset)
                .render_into(pixels, Some(pitch));
        })
    }
}

#[derive(Default)]
struct Path(Vec<Command>);

impl PathSink for Path {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(Command::MoveTo([x, -y].into()));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(Command::LineTo([x, -y].into()));
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0
            .push(Command::QuadTo([cx, -cy].into(), [x, -y].into()));
    }

    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.0.push(Command::CurveTo(
            [c1x, -c1y].into(),
            [c2x, -c2y].into(),
            [x, -y].into(),
        ));
    }

    fn close(&mut self) {
        self.0.push(Command::Close);
    }
}
