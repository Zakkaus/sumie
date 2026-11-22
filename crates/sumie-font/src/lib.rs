//! Owns font discovery and fallback, shaping, rasterization, and glyph atlas
//! data. Does not issue GPU calls or read or change terminal state.

#![forbid(unsafe_code)]

mod atlas;
mod discovery;
mod layout;
mod metrics;
mod raster;

#[cfg(test)]
mod tests;

use std::{collections::HashMap, path::PathBuf};

use discovery::{Discovery, Face};
use skrifa::GlyphId;
use thiserror::Error;

pub use atlas::{Atlas, AtlasGlyph, Rect};
pub use layout::{ShapedGlyph, ShapedRun};
pub use metrics::CellMetrics;

/// Identifies a face, including its font collection index and variation instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceId(usize);

/// Atlas key scoped to the `FontSystem` that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    face: FaceId,
    glyph: GlyphId,
    ppem_bits: u32,
}

impl GlyphKey {
    pub(crate) const fn new(face: FaceId, glyph: GlyphId, ppem: f32) -> Self {
        Self {
            face,
            glyph,
            ppem_bits: ppem.to_bits(),
        }
    }

    #[must_use]
    pub const fn face(self) -> FaceId {
        self.face
    }

    #[must_use]
    pub const fn glyph_id(self) -> u32 {
        self.glyph.to_u32()
    }

    /// Raster size in physical pixels per em.
    #[must_use]
    pub const fn ppem(self) -> f32 {
        f32::from_bits(self.ppem_bits)
    }
}

#[derive(Debug, Error)]
pub enum FontError {
    #[error("font discovery is not supported on this platform")]
    UnsupportedPlatform,
    #[error("could not initialize Fontconfig")]
    FontconfigInit,
    #[cfg(target_os = "linux")]
    #[error("Fontconfig: {0}")]
    Fontconfig(#[from] fontconfig::FontconfigError),
    #[error("a font family cannot contain NUL")]
    InvalidFamily,
    #[error(
        "logical size and scale must be positive and finite, with a device size no greater than 4096 px"
    )]
    InvalidSize,
    #[error("could not read font {path:?}: {source}")]
    ReadFont {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not load OpenType face from {0:?}")]
    InvalidFont(PathBuf),
    #[error("primary font has no usable zero advance or horizontal line metrics")]
    MissingMetrics,
    #[error("no font covers U+{0:04X}")]
    MissingGlyph(u32),
    #[error("expected one width in 0..=2 per Unicode scalar, with a nonzero first width")]
    InvalidWidths,
    #[error("run exceeds the supported column or pixel coordinate range")]
    RunTooLong,
    #[error("HarfRust: {0:?}")]
    Shape(harfrust::ShapeError),
    #[error("glyph key refers to an unknown face")]
    UnknownFace,
    #[error("color and bitmap glyphs are not supported")]
    UnsupportedGlyph,
    #[error("Skrifa: {0}")]
    Outline(skrifa::ScaleError),
    #[error("not enough space to grow the glyph atlas")]
    AtlasFull,
}

/// Loaded faces and their atlas at one logical size and device scale.
pub struct FontSystem {
    discovery: Discovery,
    faces: Vec<Face>,
    fallback: HashMap<char, FaceId>,
    metrics: CellMetrics,
    layout: layout::Layout,
    rasterizer: raster::Rasterizer,
    atlas: Atlas,
}

impl FontSystem {
    /// Resolves `family` using platform aliases and loads its default style.
    ///
    /// # Errors
    /// Returns an error for an invalid size, failed discovery or load, or missing grid metrics.
    pub fn new(family: &str, logical_px: f32, scale: f32) -> Result<Self, FontError> {
        let ppem = logical_px * scale;
        if logical_px <= 0.0 || scale <= 0.0 || !(0.0 < ppem && ppem <= 4096.0) {
            return Err(FontError::InvalidSize);
        }
        let discovery = Discovery::new(family)?;
        let matched = discovery.find(None)?;
        let font = matched.load(&[])?;
        let metrics = CellMetrics::measure(&font, ppem)?;
        Ok(Self {
            discovery,
            faces: vec![Face {
                matched,
                font,
                ppem,
            }],
            fallback: HashMap::new(),
            metrics,
            layout: layout::Layout::default(),
            rasterizer: raster::Rasterizer::default(),
            atlas: Atlas::new(128),
        })
    }

    #[must_use]
    pub const fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    #[must_use]
    pub const fn primary_face(&self) -> FaceId {
        FaceId(0)
    }

    /// Returns the matched platform family, which may differ from the request.
    #[must_use]
    pub fn family(&self, face: FaceId) -> Option<&str> {
        self.faces
            .get(face.0)
            .map(|face| face.matched.family.as_str())
    }

    fn face_for(&mut self, ch: char) -> Result<FaceId, FontError> {
        if self.faces[0].font.charmap().map_unicode(ch).is_some() {
            return Ok(self.primary_face());
        }
        if let Some(&face) = self.fallback.get(&ch) {
            return Ok(face);
        }

        let matched = self.discovery.find(Some(ch))?;
        let missing = FontError::MissingGlyph(u32::from(ch));
        let face = if let Some(index) = self.faces.iter().position(|face| face.matches(&matched)) {
            self.faces[index]
                .font
                .charmap()
                .map_unicode(ch)
                .ok_or(missing)?;
            FaceId(index)
        } else {
            let font = matched.load(&self.faces)?;
            font.charmap().map_unicode(ch).ok_or(missing)?;
            let ppem = self.metrics.fallback_ppem(&font, self.faces[0].ppem);
            let id = FaceId(self.faces.len());
            self.faces.push(Face {
                matched,
                font,
                ppem,
            });
            id
        };
        self.fallback.insert(ch, face);
        Ok(face)
    }

    /// Rasterizes a shaped glyph from this font system, or returns its cached atlas entry.
    ///
    /// # Errors
    /// Returns an error if the face, outline or atlas allocation is unavailable, or the glyph
    /// uses an unsupported color or bitmap format.
    pub fn rasterize(&mut self, key: GlyphKey) -> Result<AtlasGlyph, FontError> {
        if let Some(entry) = self.atlas.get(key) {
            return Ok(entry);
        }
        let face = self.faces.get(key.face.0).ok_or(FontError::UnknownFace)?;
        self.rasterizer.draw(&face.font, key, &mut self.atlas)
    }

    #[must_use]
    pub const fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// Acknowledge all dirty regions after their bytes have been consumed.
    pub fn clear_dirty_regions(&mut self) {
        self.atlas.clear_dirty_regions();
    }
}
