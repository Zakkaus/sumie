use std::path::PathBuf;

use skrifa::font::{
    Source,
    attribute::{Attributes, Style, Weight},
};
use skrifa::{Font, FontRef, Tag};

use crate::{FontError, FontStyle};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Request {
    style: FontStyle,
    weight: Option<u16>,
}

impl Request {
    fn new(weight: Option<u16>, style: FontStyle) -> Self {
        let weight = if style.bold {
            Some((weight.unwrap_or(400) + 300).min(900))
        } else {
            weight
        };
        Self { style, weight }
    }

    fn synthetic_bold(self, weight: Weight, regular_weight: Option<Weight>) -> bool {
        // A real semibold can stand in for bold, but only if it is heavier than the base face.
        self.style.bold
            && (weight.value() < f32::from(self.weight.unwrap_or(700).min(600))
                || regular_weight.is_some_and(|regular| weight <= regular))
    }
}

#[cfg_attr(target_os = "linux", derive(Debug))]
pub(crate) struct Match {
    pub family: String,
    path: PathBuf,
    index: u32,
    request: Request,
    #[cfg(target_os = "macos")]
    bytes: skrifa::font::Blob,
    #[cfg(target_os = "macos")]
    postscript: String,
    #[cfg(target_os = "macos")]
    color_bitmap: bool,
    #[cfg(target_os = "linux")]
    color_face: std::cell::OnceCell<cairo::FontFace>,
}

impl Match {
    pub fn load(
        &self,
        faces: &[Face],
        regular_weight: Option<Weight>,
    ) -> Result<(Font, Weight, FontStyle), FontError> {
        // Different faces of a collection share its file bytes.
        let source = if let Some(face) = faces.iter().find(|face| face.matched.path == self.path) {
            face.font.source().clone()
        } else {
            #[cfg(target_os = "linux")]
            {
                std::fs::read(&self.path)
                    .map_err(|source| FontError::ReadFont {
                        path: self.path.clone(),
                        source,
                    })?
                    .into()
            }
            #[cfg(target_os = "macos")]
            {
                self.bytes.clone().into()
            }
        };
        let font = Font::new(source, self.face_index())
            .ok_or_else(|| FontError::InvalidFont(self.path.clone()))?;
        let overrides = self.overrides(&font);
        let attributes = self.attributes(&font, overrides);
        let synthetic = FontStyle {
            bold: self
                .request
                .synthetic_bold(attributes.weight, regular_weight),
            italic: self.request.style.italic && attributes.style == Style::Normal,
        };
        let named = self.named_instance();
        let font = if let Some(index) = named {
            font.instance_builder()
                .named_instance_with_overrides(index, overrides.into_iter().flatten())
                .build()
        } else if overrides.iter().any(Option::is_some) {
            font.instance_builder()
                .variations(overrides.into_iter().flatten())
                .build()
        } else {
            font
        };
        Ok((font, attributes.weight, synthetic))
    }

    fn overrides(&self, font: &Font) -> [Option<(Tag, f32)>; 2] {
        let axes = font.axes();
        let wght = Tag::new(b"wght");
        let weight = self
            .request
            .weight
            .filter(|_| axes.get_by_tag(wght).is_some())
            .map(|weight| (wght, f32::from(weight)));
        let mut posture = None;
        if self.request.style.italic {
            let ital = Tag::new(b"ital");
            let slnt = Tag::new(b"slnt");
            if axes
                .get_by_tag(ital)
                .is_some_and(|axis| axis.max_value() >= 0.5)
            {
                posture = Some((ital, 1.0));
            } else if axes.get_by_tag(slnt).is_some() {
                // OpenType slant angles are counterclockwise from vertical.
                posture = Some((slnt, -12.0));
            }
        }
        [weight, posture]
    }

    fn attributes(&self, font: &Font, overrides: [Option<(Tag, f32)>; 2]) -> Attributes {
        let variations = overrides.into_iter().flatten();
        if let Some(index) = self.named_instance() {
            Attributes::from_named_instance_with_overrides(font, index, variations)
        } else if overrides.iter().any(Option::is_some) {
            Attributes::from_variations(font, variations)
        } else {
            font.default_attributes()
        }
    }

    fn named_instance(&self) -> Option<u32> {
        // Fontconfig's upper index bits select a one-based named instance.
        (self.index >> 16).checked_sub(1)
    }

    const fn face_index(&self) -> u32 {
        self.index & 0xffff
    }
}

pub(crate) struct Face {
    pub matched: Match,
    pub font: Font,
    pub weight: Weight,
    pub ppem: f32,
    pub baseline: i32,
    pub synthetic: FontStyle,
}

impl Face {
    pub fn color_glyph(&self, glyph: skrifa::GlyphId) -> bool {
        #[cfg(target_os = "macos")]
        if self.matched.color_bitmap {
            // Core Text also decodes Apple's private sbix image formats.
            return true;
        }
        self.font.color_glyphs().get(glyph).is_some()
            || self
                .font
                .bitmap_strikes()
                .select_bitmap(glyph, Some(self.ppem))
                .is_some()
    }

    pub fn matches(&self, matched: &Match, regular_weight: Option<Weight>) -> bool {
        self.matched.path == matched.path
            && self.matched.index == matched.index
            && self.matched.request == matched.request
            && self.synthetic.bold == matched.request.synthetic_bold(self.weight, regular_weight)
    }

    pub fn sfnt(&self) -> Option<FontRef<'_>> {
        let Source::Blob(blob) = self.font.source() else {
            return None;
        };
        FontRef::from_index(blob, self.matched.face_index()).ok()
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct Discovery {
    fc: fontconfig::Fontconfig,
    family: std::ffi::CString,
    weight: Option<u16>,
}

#[cfg(target_os = "linux")]
impl Discovery {
    pub fn new(family: &str, weight: Option<u16>) -> Result<Self, FontError> {
        Ok(Self {
            fc: fontconfig::Fontconfig::new().ok_or(FontError::FontconfigInit)?,
            family: std::ffi::CString::new(family).map_err(|_| FontError::InvalidFamily)?,
            weight,
        })
    }

    /// Fontconfig's own rules pick the face for a family that is not installed.
    pub const fn substituted(&self) -> bool {
        false
    }

    pub fn find(&mut self, codepoint: Option<char>, style: FontStyle) -> Result<Match, FontError> {
        let request = Request::new(self.weight, style);
        if let Some(ch) = codepoint.filter(|&ch| is_private_use(ch))
            && let Some(matched) = self.find_nerd(ch, request)?
        {
            return Ok(matched);
        }
        to_match(&self.pattern(codepoint, request)?.font_match()?, request)
    }

    fn find_nerd(&self, ch: char, request: Request) -> Result<Option<Match>, FontError> {
        use fontconfig::{ObjectSet, UnicodeCoverage, list_fonts};

        // FontSort ranks non-covering faces too. FontList matches the requested charset.
        let mut objects = ObjectSet::new(&self.fc)?;
        for object in [c"family", c"file", c"index"] {
            objects.add(object)?;
        }
        let covering = list_fonts(&self.charset_pattern(Some(ch))?, Some(&objects))?;
        let is_nerd =
            |font: &fontconfig::Pattern<'_>| font.get_string(c"family").is_ok_and(is_nerd_family);
        if !covering.iter().any(|font| is_nerd(&font)) {
            return Ok(None);
        }
        let ranked = self
            .pattern(Some(ch), request)?
            .sort_fonts(UnicodeCoverage::NoTrim)?;
        for font in ranked.iter().filter(is_nerd) {
            let (file, index) = (font.filename()?, font.face_index()?);
            if covering.iter().any(|other| {
                other.face_index().is_ok_and(|value| value == index)
                    && other.filename().is_ok_and(|value| value == file)
            }) {
                return to_match(&font, request).map(Some);
            }
        }
        Ok(None)
    }

    fn pattern(
        &self,
        codepoint: Option<char>,
        request: Request,
    ) -> Result<fontconfig::Pattern<'_>, FontError> {
        let mut pattern = self.charset_pattern(codepoint)?;
        pattern.add_string(c"family", &self.family)?;
        if let Some(weight) = request.weight {
            pattern.add_integer(fontconfig::FC_WEIGHT, fontconfig_weight(weight))?;
        }
        if request.style.italic {
            pattern.add_integer(fontconfig::FC_SLANT, fontconfig::FC_SLANT_ITALIC)?;
        }
        Ok(pattern)
    }

    fn charset_pattern(
        &self,
        codepoint: Option<char>,
    ) -> Result<fontconfig::Pattern<'_>, FontError> {
        use fontconfig::{CharSet, Pattern};

        let mut pattern = Pattern::new(&self.fc)?;
        if let Some(codepoint) = codepoint {
            let mut charset = CharSet::new(&self.fc)?;
            charset.add_char(codepoint)?;
            pattern.add_charset(charset)?;
        }
        Ok(pattern)
    }
}

#[cfg(target_os = "linux")]
fn to_match(matched: &fontconfig::Pattern<'_>, request: Request) -> Result<Match, FontError> {
    let path = PathBuf::from(matched.filename()?);
    let index =
        u32::try_from(matched.face_index()?).map_err(|_| FontError::InvalidFont(path.clone()))?;
    Ok(Match {
        family: matched.get_string(c"family")?.to_owned(),
        path,
        index,
        request,
        color_face: std::cell::OnceCell::new(),
    })
}

#[cfg(target_os = "linux")]
impl Match {
    pub(crate) fn draw_color(
        &self,
        key: crate::GlyphKey,
        height: u32,
        atlas: &mut crate::Atlas,
    ) -> Result<crate::AtlasGlyph, FontError> {
        use cairo::{Context, FontFace, Format, Glyph, ImageSurface, freetype::Library};
        let fail = |_| FontError::ColorGlyph;
        let face = if let Some(face) = self.color_face.get() {
            face
        } else {
            let library = Library::init().map_err(|_| FontError::ColorGlyph)?;
            let ft = library
                .new_face(
                    &self.path,
                    isize::try_from(self.index).map_err(|_| FontError::ColorGlyph)?,
                )
                .map_err(|_| FontError::ColorGlyph)?;
            let native = FontFace::create_from_ft(&ft).map_err(fail)?;
            self.color_face.get_or_init(|| native)
        };
        let width = key.color_width;
        let surface =
            ImageSurface::create(Format::ARgb32, width.cast_signed(), height.cast_signed())
                .map_err(fail)?;
        let ctx = Context::new(&surface).map_err(fail)?;
        ctx.set_font_face(face);
        ctx.set_font_size(f64::from(height));
        let glyph = Glyph::new(key.glyph_id().into(), 0.0, 0.0);
        let bounds = ctx.glyph_extents(&[glyph]).map_err(fail)?;
        let (w, h) = (f64::from(width), f64::from(height));
        let scale = (w / bounds.width()).max(h / bounds.height());
        if !scale.is_finite() {
            return Err(FontError::ColorGlyph);
        }
        ctx.translate(w * 0.5, h * 0.5);
        ctx.scale(scale, scale);
        ctx.translate(
            -bounds.x_bearing() - bounds.width() * 0.5,
            -bounds.y_bearing() - bounds.height() * 0.5,
        );
        ctx.show_glyphs(&[glyph]).map_err(fail)?;
        drop(ctx);
        let src = surface.take_data().map_err(|_| FontError::ColorGlyph)?;
        atlas.insert(
            key,
            zeno::Placement {
                width,
                height,
                ..Default::default()
            },
            |dst, pitch| {
                for (src, dst) in src
                    .chunks_exact(width as usize * 4)
                    .zip(dst.chunks_mut(pitch))
                {
                    for (argb, rgba) in src
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(dst.as_chunks_mut::<4>().0)
                    {
                        *rgba = u32::from_ne_bytes(*argb).rotate_left(8).to_be_bytes();
                    }
                }
            },
        )
    }
}

#[cfg(target_os = "linux")]
fn fontconfig_weight(weight: u16) -> i32 {
    use fontconfig::{
        FC_WEIGHT_BLACK, FC_WEIGHT_BOLD, FC_WEIGHT_BOOK, FC_WEIGHT_DEMIBOLD, FC_WEIGHT_EXTRABOLD,
        FC_WEIGHT_EXTRALIGHT, FC_WEIGHT_LIGHT, FC_WEIGHT_MEDIUM, FC_WEIGHT_REGULAR, FC_WEIGHT_THIN,
    };

    // FcWeightFromOpenType interpolates between these points, then rounds to the nearest integer.
    const TABLE: [(i32, i32); 11] = [
        (100, FC_WEIGHT_THIN),
        (200, FC_WEIGHT_EXTRALIGHT),
        (300, FC_WEIGHT_LIGHT),
        (350, 55), // FC_WEIGHT_DEMILIGHT is not exported by the bindings.
        (380, FC_WEIGHT_BOOK),
        (400, FC_WEIGHT_REGULAR),
        (500, FC_WEIGHT_MEDIUM),
        (600, FC_WEIGHT_DEMIBOLD),
        (700, FC_WEIGHT_BOLD),
        (800, FC_WEIGHT_EXTRABOLD),
        (900, FC_WEIGHT_BLACK),
    ];
    let weight = i32::from(weight);
    let upper = TABLE.partition_point(|&(ot, _)| ot < weight).max(1);
    let (lo, lo_fc) = TABLE[upper - 1];
    let (hi, hi_fc) = TABLE[upper];
    lo_fc + ((weight - lo) * (hi_fc - lo_fc) + (hi - lo) / 2) / (hi - lo)
}

const fn is_private_use(ch: char) -> bool {
    matches!(ch, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{10fffd}')
}

#[cfg(target_os = "linux")]
pub(crate) fn is_nerd_family(family: &str) -> bool {
    family.contains("Nerd Font") || family.split_whitespace().any(|word| word == "NF")
}

#[cfg(target_os = "macos")]
mod core_text;

#[cfg(target_os = "macos")]
pub(crate) use core_text::Discovery;
