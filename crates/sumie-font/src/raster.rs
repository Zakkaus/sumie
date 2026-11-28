use std::collections::{HashMap, hash_map::Entry};

use skrifa::instance::Size;
use skrifa::outline::{
    DrawSettings, Engine, Hinting as DrawHinting, HintingInstance, HintingOptions,
    OutlineGlyphCollection, SmoothMode, Target,
};
use skrifa::{DrawMemory, Font, PathSink, ScalerFont};
use zeno::{Command, Fill, Join, Mask, Origin, Placement, Scratch, Stroke, Transform, Vector};

use crate::{
    Atlas, AtlasGlyph, CellMetrics, Decoration, Face, FaceId, FontError, GlyphKey, Hinting, sprite,
};

pub(crate) struct Rasterizer {
    target: Option<Target>,
    trial: Vec<u8>,
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
            trial: Vec::new(),
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
        if key.color_width != 0 {
            return face.matched.draw_color(key, metrics.height, atlas);
        }
        let (offset, placement) = self.prepare(face, key)?;
        if placement.width == 0 || placement.height == 0 {
            return Ok(atlas.insert_blank(key));
        }
        let offset = if self.target.is_some() {
            self.fit_horizontally(offset, placement)
        } else {
            offset
        };
        atlas.insert(key, placement, |pixels, pitch| {
            Mask::with_scratch(&self.path.0, &mut self.scratch)
                .size(placement.width, placement.height)
                .offset(offset)
                .render_into(pixels, Some(pitch));
            if face.synthetic.bold {
                self.embolden(key, placement, offset, pixels, pitch);
            }
        })
    }

    pub(crate) fn decoration(
        &mut self,
        mark: Decoration,
        key: GlyphKey,
        metrics: CellMetrics,
        atlas: &mut Atlas,
    ) -> Result<AtlasGlyph, FontError> {
        mark.draw(key, metrics, atlas, &mut self.scratch, &mut self.path.0)
    }

    fn embolden(
        &mut self,
        key: GlyphKey,
        placement: Placement,
        offset: Vector,
        pixels: &mut [u8],
        pitch: usize,
    ) {
        let width = placement.width as usize;
        self.trial.clear();
        self.trial.resize(width * placement.height as usize, 0);
        Mask::with_scratch(&self.path.0, &mut self.scratch)
            .style(bold_stroke(key))
            .size(placement.width, placement.height)
            .offset(offset)
            .render_into(&mut self.trial, None);
        for (row, stroke) in pixels.chunks_mut(pitch).zip(self.trial.chunks_exact(width)) {
            for (pixel, &coverage) in row.iter_mut().zip(stroke) {
                *pixel = (*pixel).max(coverage);
            }
        }
    }

    /// Light hinting snaps only the y axis, so vertical stems can straddle pixel
    /// columns. Shift the outline by up to 3/8 px, staying inside its placement, to
    /// the phase that leaves the most pixels fully covered; ties keep the smaller shift.
    fn fit_horizontally(&mut self, origin: Vector, placement: Placement) -> Vector {
        let (width, height) = (placement.width as usize, placement.height as usize);
        let bounds = self.scratch.bounds(&self.path.0, Fill::NonZero, None);
        let (left, right) = (bounds.min.x + origin.x, bounds.max.x + origin.x);
        let limit = u16::try_from(placement.width).map_or(f32::MAX, f32::from);
        let mut best = (0, origin);
        for eighths in [0_i8, -1, 1, -2, 2, -3, 3] {
            let shift = f32::from(eighths) / 8.0;
            if left + shift < 0.0 || right + shift > limit {
                continue;
            }
            let offset = origin + Vector::new(shift, 0.0);
            self.trial.clear();
            self.trial.resize(width * height, 0);
            Mask::with_scratch(&self.path.0, &mut self.scratch)
                .size(placement.width, placement.height)
                .offset(offset)
                .render_into(&mut self.trial, None);
            let opaque: usize = self.trial.iter().map(|&a| usize::from(a == u8::MAX)).sum();
            if opaque > best.0 {
                best = (opaque, offset);
            }
        }
        best.1
    }

    pub fn prepare(
        &mut self,
        face: &Face,
        key: GlyphKey,
    ) -> Result<(Vector, Placement), FontError> {
        self.path.0.clear();
        let has_outline = if let Some(target) = self.target
            && let Some(sfnt) = face.sfnt()
        {
            self.draw_hinted(&face.font, key, target, &OutlineGlyphCollection::new(&sfnt))?
        } else {
            self.draw_unhinted(&face.font, key)?
        };
        if !has_outline {
            return Ok((Vector::ZERO, Placement::default()));
        }
        if face.synthetic.italic {
            // Slant right above the baseline in device coordinates.
            let oblique = Transform::new(1.0, 0.0, -0.2126, 1.0, 0.0, 0.0);
            for command in &mut self.path.0 {
                *command = command.transform(&oblique);
            }
        }
        let mut bounds = self.scratch.bounds(&self.path.0, Fill::NonZero, None);
        if face.synthetic.bold && !bounds.is_empty() {
            let stroke = self.scratch.bounds(&self.path.0, bold_stroke(key), None);
            bounds.min.x = bounds.min.x.min(stroke.min.x);
            bounds.min.y = bounds.min.y.min(stroke.min.y);
            bounds.max.x = bounds.max.x.max(stroke.max.x);
            bounds.max.y = bounds.max.y.max(stroke.max.y);
        }
        Ok(Placement::compute(Origin::TopLeft, [0.0, 0.0], &bounds))
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

fn bold_stroke(key: GlyphKey) -> Stroke<'static> {
    Stroke {
        width: key.ppem() / 24.0,
        join: Join::Round,
        ..Stroke::default()
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
