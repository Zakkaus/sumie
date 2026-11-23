use std::collections::{HashMap, hash_map::Entry};

use skrifa::instance::Size;
use skrifa::outline::{
    DrawSettings, Engine, Hinting as DrawHinting, HintingInstance, HintingOptions,
    OutlineGlyphCollection, SmoothMode, Target,
};
use skrifa::{DrawMemory, Font, PathSink, ScalerFont};
use zeno::{Command, Fill, Mask, Origin, Placement, Scratch};

use crate::{Atlas, AtlasGlyph, CellMetrics, Face, FaceId, FontError, GlyphKey, Hinting, sprite};

pub(crate) struct Rasterizer {
    target: Option<Target>,
    instances: HashMap<(FaceId, u32), HintingInstance>,
    outline_memory: Vec<u8>,
    path: Path,
    scratch: Scratch,
}

impl Rasterizer {
    pub fn new(hinting: Hinting) -> Self {
        let target = match hinting {
            Hinting::Off => None,
            Hinting::Light => Some(Target::Smooth {
                mode: SmoothMode::Light,
                symmetric_rendering: true,
                preserve_linear_metrics: true,
            }),
            Hinting::Normal => Some(SmoothMode::Normal.into()),
        };
        Self {
            target,
            instances: HashMap::new(),
            outline_memory: Vec::new(),
            path: Path::default(),
            scratch: Scratch::new(),
        }
    }

    pub fn draw(
        &mut self,
        faces: &[Face],
        metrics: CellMetrics,
        key: GlyphKey,
        atlas: &mut Atlas,
    ) -> Result<AtlasGlyph, FontError> {
        if key.face == sprite::FACE {
            return sprite::draw(key, metrics, atlas, &mut self.scratch, &mut self.path.0);
        }
        let face = faces.get(key.face.0).ok_or(FontError::UnknownFace)?;
        let font = &face.font;
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
        self.path.0.clear();
        let has_outline = if let Some(target) = self.target
            && let Some(sfnt) = face.sfnt()
        {
            self.draw_hinted(font, key, target, &OutlineGlyphCollection::new(&sfnt))?
        } else {
            self.draw_unhinted(font, key)?
        };
        if !has_outline {
            return Ok(atlas.insert_blank(key));
        }
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

    fn draw_unhinted(&mut self, font: &Font, key: GlyphKey) -> Result<bool, FontError> {
        let scaler = ScalerFont::force_unhinted(font.clone(), Some(key.ppem()));
        let Some(outline) = scaler.outline(key.glyph) else {
            return Ok(false);
        };
        let required = outline.required_memory_size();
        self.outline_memory
            .resize(self.outline_memory.len().max(required), 0);
        outline
            .draw(DrawMemory::Buffer(&mut self.outline_memory), &mut self.path)
            .map_err(FontError::Outline)?;
        Ok(true)
    }

    fn draw_hinted(
        &mut self,
        font: &Font,
        key: GlyphKey,
        target: Target,
        outlines: &OutlineGlyphCollection<'_>,
    ) -> Result<bool, FontError> {
        let Some(outline) = outlines.get(key.glyph) else {
            return Ok(false);
        };
        let instance = match self.instances.entry((key.face, key.ppem_bits)) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(HintingInstance::new(
                outlines,
                Size::new(key.ppem()),
                font.normalized_coords(),
                HintingOptions {
                    engine: Engine::AutoFallback,
                    target,
                },
            )?),
        };
        let required = outline.draw_memory_size(DrawHinting::Embedded);
        self.outline_memory
            .resize(self.outline_memory.len().max(required), 0);
        let settings =
            DrawSettings::hinted(instance, false).with_memory(Some(&mut self.outline_memory));
        outline.draw(settings, &mut self.path)?;
        Ok(true)
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
