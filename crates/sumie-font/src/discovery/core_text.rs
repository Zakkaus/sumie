#![allow(unsafe_code)]

use std::{cell::OnceCell, collections::HashMap, path::PathBuf, ptr};

use objc2_core_foundation::{CFArray, CFRange, CFRetained, CFString, CFURL, CFURLPathStyle};
use objc2_core_text::{CTFont, CTFontDescriptor, kCTFontURLAttribute};
use skrifa::{
    FontRef, MetadataProvider as _,
    font::Blob,
    raw::{FileRef, TableProvider as _, tables::head::Flags},
    string::StringId,
};

use super::{FontError, Match};

pub(crate) struct Discovery {
    primary: CFRetained<CTFont>,
    weight: Option<u16>,
    files: HashMap<PathBuf, FontFile>,
    cascade: OnceCell<Option<CFRetained<CFArray<CTFontDescriptor>>>>,
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
                    if let Ok(face) = face
                        && let Some(name) = face.localized_strings(StringId::POSTSCRIPT_NAME).next()
                    {
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
            cascade: OnceCell::new(),
        })
    }

    pub fn find(&mut self, ch: Option<char>) -> Result<Match, FontError> {
        let Some(ch) = ch else {
            return file_match(&self.primary, &mut self.files, self.weight);
        };
        let text = CFString::from_str(ch.encode_utf8(&mut [0; 4]));
        let range = CFRange {
            location: 0,
            length: text.length(),
        };
        // SAFETY: the range covers this live CFString in UTF-16 units.
        let preferred = unsafe { self.primary.for_string(&text, range) };
        let mut candidate = |font: &CTFont| {
            let matched = file_match(font, &mut self.files, self.weight).ok()?;
            let face = FontRef::from_index(&matched.bytes, matched.index).ok()?;
            if face.head().ok()?.flags().contains(Flags::LAST_RESORT_FONT) {
                return None;
            }
            let glyph = face.charmap().map(ch)?;
            face.outline_glyphs().get(glyph)?;
            Some(matched)
        };
        if let Some(matched) = candidate(&preferred) {
            return Ok(matched);
        }

        let descriptors = self.cascade.get_or_init(|| {
            // SAFETY: null selects system language preferences. Core Text returns an
            // immutable array of CTFontDescriptor objects, not unboxed values.
            unsafe {
                self.primary
                    .default_cascade_list_for_languages(None)
                    .map(|list| CFRetained::cast_unchecked(list))
            }
        });
        if let Some(descriptors) = descriptors {
            for descriptor in descriptors.iter() {
                // SAFETY: the descriptor is retained and null selects the identity transform.
                let font = unsafe { CTFont::with_font_descriptor(&descriptor, 0.0, ptr::null()) };
                // SAFETY: font is retained; character_set returns an owned CFCharacterSet.
                if unsafe { font.character_set() }.is_long_character_member(u32::from(ch))
                    && let Some(matched) = candidate(&font)
                {
                    return Ok(matched);
                }
            }
        }
        Err(FontError::MissingGlyph(u32::from(ch)))
    }
}

fn file_match(
    font: &CTFont,
    files: &mut HashMap<PathBuf, FontFile>,
    weight: Option<u16>,
) -> Result<Match, FontError> {
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
    let file = match files.entry(path.clone()) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => entry.insert(FontFile::read(&path)?),
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
        weight,
        bytes: file.bytes.clone(),
    })
}
