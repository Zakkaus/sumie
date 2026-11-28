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
        if key.is_color() {
            #[cfg(target_os = "macos")]
            return face
                .matched
                .draw_color(key, metrics.height, atlas, &mut self.trial);
            #[cfg(target_os = "linux")]
            {
                let png = png_strike(&face.font, key.glyph, metrics.height)
                    .ok_or(FontError::ColorGlyph)?;
                return draw_color(png, key, metrics.height, atlas, &mut self.trial);
            }
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

#[cfg(target_os = "linux")]
pub(crate) fn png_strike(font: &Font, glyph: skrifa::GlyphId, height: u32) -> Option<&[u8]> {
    use skrifa::font::glyph::{GlyphSource, GlyphSourceOrder, bitmap::Pixels};
    const COLOR: GlyphSourceOrder = GlyphSourceOrder::new(&[GlyphSource::Cbdt]);
    font.bitmap_strikes_with(COLOR)
        .iter()
        .filter_map(|strike| match strike.get(glyph)?.pixels() {
            Pixels::Png(bytes) => Some((height.abs_diff(u32::from(strike.ppem_y())), bytes)),
            _ => None,
        })
        .min_by_key(|&(distance, _)| distance)
        .map(|(_, bytes)| bytes)
}

#[cfg(target_os = "linux")]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn draw_color(
    png: &[u8],
    key: GlyphKey,
    height: u32,
    atlas: &mut Atlas,
    scratch: &mut Vec<u8>,
) -> Result<AtlasGlyph, FontError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().map_err(|_| FontError::ColorGlyph)?;
    // Decoder limits exclude the caller's output buffer.
    let len = reader
        .output_buffer_size()
        .filter(|&n| n <= png::Limits::default().bytes)
        .ok_or(FontError::ColorGlyph)?;
    scratch.resize(len, 0);
    let frame = reader
        .next_frame(scratch)
        .map_err(|_| FontError::ColorGlyph)?;
    let channels = frame.color_type.samples();
    let width = key.color_width;
    let step = (f64::from(frame.width) / f64::from(width))
        .min(f64::from(frame.height) / f64::from(height));
    let span = |src: u32, dst: u32, i: u32| {
        let start = (f64::from(src) - f64::from(dst) * step) * 0.5 + f64::from(i) * step;
        if step < 1.0 {
            let pixel = (start + step * 0.5) as usize;
            pixel..pixel + 1
        } else {
            start.round() as usize..(start + step).round() as usize
        }
    };
    atlas.insert_color(key, height, |dst, pitch| {
        for (y, row) in (0..height).zip(dst.chunks_mut(pitch)) {
            let ys = span(frame.height, height, y);
            for (x, rgba) in (0..width).zip(row.as_chunks_mut::<4>().0) {
                let xs = span(frame.width, width, x);
                let count = (xs.len() * ys.len()) as u64;
                let mut sum = [0_u64; 4];
                for sy in ys.clone() {
                    let src = &scratch[sy * frame.line_size..];
                    for p in src[xs.start * channels..xs.end * channels].chunks_exact(channels) {
                        let a = u64::from(p[channels - 1]);
                        let [r, g, b] = if let [r, g, b, _] = *p {
                            [r, g, b]
                        } else {
                            [p[0]; 3]
                        };
                        sum[0] += u64::from(r) * a;
                        sum[1] += u64::from(g) * a;
                        sum[2] += u64::from(b) * a;
                        sum[3] += 255 * a;
                    }
                }
                *rgba = sum.map(|s| ((s + 255 * count / 2) / (255 * count)) as u8);
            }
        }
    })
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn emoji_downsampling_averages_premultiplied_ink() {
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, 2, 2);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&[255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 0, 0, 255, 0])
        .unwrap();
    let mut key = GlyphKey::new(FaceId(0), skrifa::GlyphId::new(1), 1.0);
    key.color_width = 1;
    let mut atlas = Atlas::new(1, 4);
    draw_color(&png, key, 1, &mut atlas, &mut Vec::new()).unwrap();
    assert_eq!(atlas.pixels(), [64, 32, 0, 96]);
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
