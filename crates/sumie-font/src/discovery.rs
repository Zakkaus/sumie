use std::path::PathBuf;

use skrifa::Font;

use crate::FontError;

#[derive(Debug)]
pub(crate) struct Match {
    pub family: String,
    path: PathBuf,
    index: u32,
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
        let font = Font::new(source, self.index & 0xffff)
            .ok_or_else(|| FontError::InvalidFont(self.path.clone()))?;
        // Fontconfig's upper index bits select a one-based named instance.
        Ok(match self.index >> 16 {
            0 => font,
            instance => font.instance_builder().named_instance(instance - 1).build(),
        })
    }
}

pub(crate) struct Face {
    pub matched: Match,
    pub font: Font,
    pub ppem: f32,
}

impl Face {
    pub fn matches(&self, matched: &Match) -> bool {
        self.matched.path == matched.path && self.matched.index == matched.index
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct Discovery {
    fc: fontconfig::Fontconfig,
    family: std::ffi::CString,
}

#[cfg(target_os = "linux")]
impl Discovery {
    pub fn new(family: &str) -> Result<Self, FontError> {
        Ok(Self {
            fc: fontconfig::Fontconfig::new().ok_or(FontError::FontconfigInit)?,
            family: std::ffi::CString::new(family).map_err(|_| FontError::InvalidFamily)?,
        })
    }

    pub fn find(&self, codepoint: Option<char>) -> Result<Match, FontError> {
        use fontconfig::{CharSet, Pattern};

        let mut pattern = Pattern::new(&self.fc)?;
        pattern.add_string(c"family", &self.family)?;
        if let Some(codepoint) = codepoint {
            let mut charset = CharSet::new(&self.fc)?;
            charset.add_char(codepoint)?;
            pattern.add_charset(charset)?;
        }
        let matched = pattern.font_match()?;
        let path = PathBuf::from(matched.filename()?);
        let index = u32::try_from(matched.face_index()?)
            .map_err(|_| FontError::InvalidFont(path.clone()))?;
        Ok(Match {
            family: matched.get_string(c"family")?.to_owned(),
            path,
            index,
        })
    }
}

#[cfg(target_os = "macos")]
pub(crate) enum Discovery {}

#[cfg(target_os = "macos")]
impl Discovery {
    pub fn new(_: &str) -> Result<Self, FontError> {
        // TODO(P1): Discover the primary and fallback faces through Core Text.
        Err(FontError::UnsupportedPlatform)
    }

    pub fn find(&self, _: Option<char>) -> Result<Match, FontError> {
        match *self {}
    }
}
