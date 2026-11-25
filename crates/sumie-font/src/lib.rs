//! Owns font discovery and fallback, shaping, rasterization, and glyph atlas
//! data. Does not issue GPU calls or read or change terminal state.

#![cfg_attr(not(target_os = "macos"), forbid(unsafe_code))]
#![deny(unsafe_code)]

mod atlas;
mod decoration;
mod discovery;
mod layout;
mod metrics;
mod raster;
mod sprite;

#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use discovery::{Discovery, Face};
use skrifa::{GlyphId, font::attribute::Weight};
use thiserror::Error;

pub use atlas::{Atlas, AtlasGlyph, Rect};
pub use decoration::Decoration;
pub use layout::{ShapedGlyph, ShapedRun, TextGlyph, TextRun};
pub use metrics::CellMetrics;

const MAX_PPEM: f32 = 4096.0;

/// Grid fitting applied to glyph outlines, independently of shaping and cell metrics.
#[derive(Clone, Copy, Debug, Default)]
pub enum Hinting {
    #[default]
    Off,
    Light,
    Normal,
}

/// Font selection and rasterization settings for one font system and atlas.
#[derive(Clone, Copy, Debug, Default)]
pub struct FontOptions {
    pub hinting: Hinting,
    /// Primary and fallback weight on the CSS scale, 100 through 900. `None` leaves matching unchanged.
    pub weight: Option<u16>,
}

/// Face style requested for a shaping run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FontStyle {
    pub bold: bool,
    pub italic: bool,
}

impl FontStyle {
    const fn index(self) -> usize {
        self.bold as usize | (self.italic as usize) << 1
    }
}

/// Identifies a face, including its collection index, variation instance, and synthetic style.
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
    #[cfg(target_os = "macos")]
    #[error("Core Text returned no font file URL for {0}")]
    MissingFontUrl(String),
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
    #[error("font weight must be an integer from 100 through 900")]
    InvalidWeight,
    #[error("could not read font {path:?}: {source}")]
    ReadFont {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not load OpenType face from {0:?}")]
    InvalidFont(PathBuf),
    #[error("primary font has no usable zero advance or horizontal line metrics")]
    MissingMetrics,
    #[error(
        "no usable glyph for U+{codepoint:04X}{}{}",
        if *.pending { " yet; the system-wide font lookup is still running" } else { "" },
        Rejections(.rejected)
    )]
    MissingGlyph {
        codepoint: u32,
        /// Fonts that fallback tried for this character, in order.
        rejected: Vec<Rejection>,
        /// Fallback has not finished, so a later lookup may find a font.
        pending: bool,
    },
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
    #[error("Skrifa hinted outline: {0}")]
    HintedOutline(#[from] skrifa::outline::DrawError),
    #[error("not enough space to grow the glyph atlas")]
    AtlasFull,
}

/// A font that fallback tried for a character and could not use.
#[derive(Debug)]
pub struct Rejection {
    /// PostScript name.
    pub font: String,
    pub path: Option<PathBuf>,
    pub error: FontError,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.font)?;
        if let Some(path) = &self.path {
            write!(f, " ({})", path.display())?;
        }
        write!(f, ": {}", self.error)
    }
}

struct Rejections<'a>(&'a [Rejection]);

impl std::fmt::Display for Rejections<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0
            .iter()
            .try_for_each(|rejection| write!(f, "; rejected {rejection}"))
    }
}

/// Loaded faces and their atlas at one logical size and device scale.
pub struct FontSystem {
    discovery: Discovery,
    faces: Vec<Face>,
    primary: [Option<FaceId>; 4],
    fallback: [HashMap<char, FaceId>; 4],
    /// Characters whose fallback lookup failed. Only the first error lists the rejected fonts.
    /// A pending lookup is not recorded, so the next use of the character asks again.
    missing: [HashSet<char>; 4],
    /// Fallback discovery calls, counted for cache tests.
    #[cfg(test)]
    fallback_finds: usize,
    metrics: CellMetrics,
    layout: layout::Layout,
    rasterizer: raster::Rasterizer,
    atlas: Atlas,
}

impl FontSystem {
    /// Resolves the primary face using platform family aliases and the requested weight.
    ///
    /// # Errors
    /// Returns an error for an invalid size or weight, failed discovery or load, or missing grid metrics.
    pub fn new(
        family: &str,
        logical_px: f32,
        scale: f32,
        options: FontOptions,
    ) -> Result<Self, FontError> {
        let ppem = logical_px * scale;
        if logical_px <= 0.0 || scale <= 0.0 || !(0.0 < ppem && ppem <= MAX_PPEM) {
            return Err(FontError::InvalidSize);
        }
        if options
            .weight
            .is_some_and(|weight| !(100..=900).contains(&weight))
        {
            return Err(FontError::InvalidWeight);
        }
        let mut discovery = Discovery::new(family, options.weight)?;
        let matched = discovery.find(None, FontStyle::default())?;
        let (font, weight, synthetic) = matched.load(&[], None)?;
        let metrics = CellMetrics::measure(&font, ppem)?;
        Ok(Self {
            discovery,
            faces: vec![Face {
                matched,
                font,
                weight,
                ppem,
                baseline: metrics.baseline,
                synthetic,
            }],
            primary: [Some(FaceId(0)), None, None, None],
            fallback: Default::default(),
            missing: Default::default(),
            #[cfg(test)]
            fallback_finds: 0,
            metrics,
            layout: layout::Layout::default(),
            rasterizer: raster::Rasterizer::new(options.hinting),
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

    /// Load the ten digit glyphs of a secondary monospaced face into this atlas's namespace.
    /// The size and returned common advance are in device pixels.
    ///
    /// # Errors
    /// Returns an error on invalid size, failed discovery or load, or missing digits or metrics.
    pub fn load_digits(
        &mut self,
        family: &str,
        ppem: f32,
    ) -> Result<([GlyphKey; 10], u32), FontError> {
        if !(0.0 < ppem && ppem <= MAX_PPEM) {
            return Err(FontError::InvalidSize);
        }
        let matched = Discovery::new(family, None)?.find(None, FontStyle::default())?;
        let (font, weight, synthetic) = matched.load(&self.faces, None)?;
        let metrics = CellMetrics::measure(&font, ppem)?;
        let id = FaceId(self.faces.len());
        let mut keys = [GlyphKey::new(id, GlyphId::new(0), ppem); 10];
        let charmap = font.charmap();
        for (key, digit) in keys.iter_mut().zip('0'..='9') {
            let glyph = charmap
                .map_unicode(digit)
                .ok_or_else(|| FontError::MissingGlyph {
                    codepoint: u32::from(digit),
                    rejected: Vec::new(),
                    pending: false,
                })?;
            *key = GlyphKey::new(id, glyph, ppem);
        }
        self.faces.push(Face {
            matched,
            font,
            weight,
            ppem,
            baseline: metrics.baseline,
            synthetic,
        });
        Ok((keys, metrics.width))
    }

    fn styled_primary(&mut self, style: FontStyle) -> Result<FaceId, FontError> {
        if let Some(face) = self.primary[style.index()] {
            return Ok(face);
        }
        let matched = self.discovery.find(None, style)?;
        let regular_weight = self.non_bold_weight(None, style);
        let (font, weight, synthetic) = matched.load(&self.faces, regular_weight)?;
        let id = FaceId(self.faces.len());
        self.faces.push(Face {
            matched,
            font,
            weight,
            ppem: self.faces[0].ppem,
            baseline: self.metrics.baseline,
            synthetic,
        });
        self.primary[style.index()] = Some(id);
        Ok(id)
    }

    fn face_for(&mut self, ch: char, style: FontStyle) -> Result<FaceId, FontError> {
        if sprite::covers(ch) {
            return Ok(sprite::FACE);
        }
        let primary = self.styled_primary(style)?;
        if self.faces[primary.0]
            .font
            .charmap()
            .map_unicode(ch)
            .is_some()
        {
            return Ok(primary);
        }
        if let Some(&face) = self.fallback[style.index()].get(&ch) {
            return Ok(face);
        }
        if self.missing[style.index()].contains(&ch) {
            return Err(FontError::MissingGlyph {
                codepoint: u32::from(ch),
                rejected: Vec::new(),
                pending: false,
            });
        }
        let face = self.load_fallback(ch, style);
        match &face {
            Ok(face) => {
                self.fallback[style.index()].insert(ch, *face);
            }
            Err(FontError::MissingGlyph { pending: true, .. }) => {}
            Err(_) => {
                self.missing[style.index()].insert(ch);
            }
        }
        face
    }

    fn load_fallback(&mut self, ch: char, style: FontStyle) -> Result<FaceId, FontError> {
        #[cfg(test)]
        {
            self.fallback_finds += 1;
        }
        let matched = self.discovery.find(Some(ch), style)?;
        let regular_weight = self.non_bold_weight(Some(ch), style);
        let missing = FontError::MissingGlyph {
            codepoint: u32::from(ch),
            rejected: Vec::new(),
            pending: false,
        };
        if let Some(index) = self
            .faces
            .iter()
            .position(|face| face.matches(&matched, regular_weight))
        {
            self.faces[index]
                .font
                .charmap()
                .map_unicode(ch)
                .ok_or(missing)?;
            Ok(FaceId(index))
        } else {
            let (font, weight, synthetic) = matched.load(&self.faces, regular_weight)?;
            font.charmap().map_unicode(ch).ok_or(missing)?;
            let ppem = self.metrics.fallback_ppem(&font, self.faces[0].ppem);
            let id = FaceId(self.faces.len());
            self.faces.push(Face {
                baseline: self.metrics.fallback_baseline(&font, ppem),
                matched,
                font,
                weight,
                ppem,
                synthetic,
            });
            Ok(id)
        }
    }

    fn non_bold_weight(&mut self, ch: Option<char>, style: FontStyle) -> Option<Weight> {
        if !style.bold {
            return None;
        }
        let style = FontStyle {
            bold: false,
            ..style
        };
        // A bold match can still be usable if its non-bold counterpart fails to load.
        let face = match ch {
            Some(ch) => self.face_for(ch, style),
            None => self.styled_primary(style),
        }
        .ok()?;
        Some(self.faces[face.0].weight)
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
        self.rasterizer
            .draw(&self.faces, self.metrics, key, &mut self.atlas)
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
