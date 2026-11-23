use std::path::PathBuf;

use skrifa::font::Source;
use skrifa::{Font, FontRef, Tag};

use crate::FontError;

#[derive(Debug)]
pub(crate) struct Match {
    pub family: String,
    path: PathBuf,
    index: u32,
    weight: Option<u16>,
}

impl Match {
    pub fn load(&self, faces: &[Face]) -> Result<Font, FontError> {
        // Different faces of a collection share its file bytes.
        let source = if let Some(face) = faces.iter().find(|face| face.matched.path == self.path) {
            face.font.source().clone()
        } else {
            std::fs::read(&self.path)
                .map_err(|source| FontError::ReadFont {
                    path: self.path.clone(),
                    source,
                })?
                .into()
        };
        let font = Font::new(source, self.face_index())
            .ok_or_else(|| FontError::InvalidFont(self.path.clone()))?;
        // Fontconfig's upper index bits select a one-based named instance.
        let named = self.index >> 16;
        let axis = Tag::new(b"wght");
        if let Some(weight) = self
            .weight
            .filter(|_| font.axes().get_by_tag(axis).is_some())
        {
            let variation = [(axis, f32::from(weight))];
            let instance = font.instance_builder();
            return Ok(match named {
                0 => instance.variations(variation),
                index => instance.named_instance_with_overrides(index - 1, variation),
            }
            .build());
        }
        Ok(match named {
            0 => font,
            index => font.instance_builder().named_instance(index - 1).build(),
        })
    }

    const fn face_index(&self) -> u32 {
        self.index & 0xffff
    }
}

pub(crate) struct Face {
    pub matched: Match,
    pub font: Font,
    pub ppem: f32,
    pub baseline: i32,
}

impl Face {
    pub fn matches(&self, matched: &Match) -> bool {
        self.matched.path == matched.path && self.matched.index == matched.index
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

    pub fn find(&self, codepoint: Option<char>) -> Result<Match, FontError> {
        if let Some(ch) = codepoint.filter(|&ch| is_private_use(ch))
            && let Some(matched) = self.find_nerd(ch)?
        {
            return Ok(matched);
        }
        to_match(&self.pattern(codepoint)?.font_match()?, self.weight)
    }

    fn find_nerd(&self, ch: char) -> Result<Option<Match>, FontError> {
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
            .pattern(Some(ch))?
            .sort_fonts(UnicodeCoverage::NoTrim)?;
        for font in ranked.iter().filter(is_nerd) {
            let (file, index) = (font.filename()?, font.face_index()?);
            if covering.iter().any(|other| {
                other.face_index().is_ok_and(|value| value == index)
                    && other.filename().is_ok_and(|value| value == file)
            }) {
                return to_match(&font, self.weight).map(Some);
            }
        }
        Ok(None)
    }

    fn pattern(&self, codepoint: Option<char>) -> Result<fontconfig::Pattern<'_>, FontError> {
        let mut pattern = self.charset_pattern(codepoint)?;
        pattern.add_string(c"family", &self.family)?;
        if let Some(weight) = self.weight {
            pattern.add_integer(fontconfig::FC_WEIGHT, fontconfig_weight(weight))?;
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
fn to_match(matched: &fontconfig::Pattern<'_>, weight: Option<u16>) -> Result<Match, FontError> {
    let path = PathBuf::from(matched.filename()?);
    let index =
        u32::try_from(matched.face_index()?).map_err(|_| FontError::InvalidFont(path.clone()))?;
    Ok(Match {
        family: matched.get_string(c"family")?.to_owned(),
        path,
        index,
        weight,
    })
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

#[cfg(target_os = "linux")]
const fn is_private_use(ch: char) -> bool {
    matches!(ch, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{10fffd}')
}

#[cfg(target_os = "linux")]
pub(crate) fn is_nerd_family(family: &str) -> bool {
    family.contains("Nerd Font") || family.split_whitespace().any(|word| word == "NF")
}

#[cfg(target_os = "macos")]
pub(crate) enum Discovery {}

#[cfg(target_os = "macos")]
impl Discovery {
    pub fn new(_: &str, _: Option<u16>) -> Result<Self, FontError> {
        // TODO(P1): Discover the primary and fallback faces through Core Text.
        Err(FontError::UnsupportedPlatform)
    }

    pub fn find(&self, _: Option<char>) -> Result<Match, FontError> {
        match *self {}
    }
}
