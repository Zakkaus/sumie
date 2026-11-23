#![allow(unsafe_code)]

use std::{collections::HashMap, path::PathBuf, ptr};

use objc2_core_foundation::{CFRange, CFRetained, CFString, CFURL, CFURLPathStyle};
use objc2_core_text::{CTFont, kCTFontURLAttribute};
use skrifa::{MetadataProvider as _, font::Blob, raw::FileRef, string::StringId};

use super::{FontError, Match};

pub(crate) struct Discovery {
    primary: CFRetained<CTFont>,
    weight: Option<u16>,
    files: HashMap<PathBuf, FontFile>,
}

struct FontFile {
    bytes: Blob,
    // Core Text identifies a face by name, not its TTC directory index.
    collection: Option<HashMap<String, u32>>,
}

impl FontFile {
    fn read(path: &std::path::Path) -> Result<Self, FontError> {
        let bytes: Blob = std::fs::read(path)
            .map_err(|source| FontError::ReadFont {
                path: path.to_owned(),
                source,
            })?
            .into();
        let file = FileRef::new(&bytes).map_err(|_| FontError::InvalidFont(path.to_owned()))?;
        let collection = match file {
            FileRef::Font(_) => None,
            FileRef::Collection(ttc) => {
                let mut names = HashMap::new();
                for (index, face) in (0..ttc.len()).zip(ttc.iter()) {
                    let face = face.map_err(|_| FontError::InvalidFont(path.to_owned()))?;
                    if let Some(name) = face.localized_strings(StringId::POSTSCRIPT_NAME).next() {
                        names.insert(name.to_string(), index);
                    }
                }
                Some(names)
            }
        };
        Ok(Self { bytes, collection })
    }
}

impl Discovery {
    pub fn new(family: &str, weight: Option<u16>) -> Result<Self, FontError> {
        if family.contains('\0') {
            return Err(FontError::InvalidFamily);
        }
        // SAFETY: the name outlives the call; null selects the identity transform.
        let primary = unsafe { CTFont::with_name(&CFString::from_str(family), 0.0, ptr::null()) };
        Ok(Self {
            primary,
            weight,
            files: HashMap::new(),
        })
    }

    pub fn find(&mut self, ch: Option<char>) -> Result<Match, FontError> {
        let fallback = ch.map(|ch| {
            let text = CFString::from_str(ch.encode_utf8(&mut [0; 4]));
            let range = CFRange {
                location: 0,
                length: text.length(),
            };
            // SAFETY: the range covers exactly this live CFString, in UTF-16 units.
            unsafe { self.primary.for_string(&text, range) }
        });
        let font = fallback.as_deref().unwrap_or(&self.primary);
        // SAFETY: font is retained throughout; the exported URL key is immutable.
        let (name, family, url) = unsafe {
            (
                font.post_script_name(),
                font.family_name(),
                font.attribute(kCTFontURLAttribute),
            )
        };
        let path = url
            .and_then(|value| value.downcast::<CFURL>().ok())
            .and_then(|url| url.file_system_path(CFURLPathStyle::CFURLPOSIXPathStyle))
            .map(|path| PathBuf::from(path.to_string()))
            .ok_or_else(|| FontError::MissingFontUrl(name.to_string()))?;
        let file = match self.files.entry(path.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(FontFile::read(&path)?)
            }
        };
        let index = match &file.collection {
            Some(names) => *names
                .get(&name.to_string())
                .ok_or_else(|| FontError::InvalidFont(path.clone()))?,
            None => 0,
        };
        Ok(Match {
            family: family.to_string(),
            path,
            index,
            weight: self.weight,
            bytes: file.bytes.clone(),
        })
    }
}
