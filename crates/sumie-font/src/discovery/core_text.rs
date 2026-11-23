#![allow(unsafe_code)]

use std::{cell::OnceCell, collections::HashMap, path::PathBuf, ptr};

use objc2_core_foundation::{
    CFArray, CFDictionary, CFRange, CFRetained, CFString, CFURL, CFURLPathStyle,
};
use objc2_core_text::{CTFont, CTFontDescriptor, kCTFontFamilyNameAttribute, kCTFontURLAttribute};
use skrifa::{
    FontRef, MetadataProvider as _,
    font::Blob,
    raw::{FileRef, TableProvider as _, tables::head::Flags},
    string::StringId,
};

use super::{Font, FontError, FontStyle, Match, Request, Style};

pub(crate) struct Discovery {
    primary: CFRetained<CTFont>,
    weight: Option<u16>,
    files: HashMap<PathBuf, FontFile>,
    families: HashMap<String, Descriptors>,
    cascade: OnceCell<Descriptors>,
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
            families: HashMap::new(),
            cascade: OnceCell::new(),
        })
    }

    pub fn find(&mut self, ch: Option<char>, style: FontStyle) -> Result<Match, FontError> {
        let request = Request::new(self.weight, style);
        let mut candidate =
            |font: &CTFont| matching_face(font, request, ch, &mut self.files, &mut self.families);
        let Some(ch) = ch else {
            return candidate(&self.primary);
        };
        let text = CFString::from_str(ch.encode_utf8(&mut [0; 4]));
        let range = CFRange {
            location: 0,
            length: text.length(),
        };
        // SAFETY: the range covers this live CFString in UTF-16 units.
        let preferred = unsafe { self.primary.for_string(&text, range) };
        if let Ok(matched) = candidate(&preferred) {
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
                    && let Ok(matched) = candidate(&font)
                {
                    return Ok(matched);
                }
            }
        }
        Err(FontError::MissingGlyph(u32::from(ch)))
    }
}

type Descriptors = Option<CFRetained<CFArray<CTFontDescriptor>>>;

fn matching_face(
    font: &CTFont,
    request: Request,
    ch: Option<char>,
    files: &mut HashMap<PathBuf, FontFile>,
    families: &mut HashMap<String, Descriptors>,
) -> Result<Match, FontError> {
    let original = file_match(font, files, request)?;
    if request.style == FontStyle::default() && request.weight.is_none() {
        return require_coverage(original, ch);
    }
    let original_rank = rank(&original, ch);
    if original_rank.is_some_and(|(slant, bold, distance)| !slant && !bold && distance <= 0.0) {
        return Ok(original);
    }

    let mut best_rank = original_rank;
    let mut best = None;
    if let Some(descriptors) = families
        .entry(original.family.clone())
        .or_insert_with_key(|family| family_descriptors(family))
    {
        for descriptor in descriptors.iter() {
            // SAFETY: the descriptor is retained and null selects the identity transform.
            let font = unsafe { CTFont::with_font_descriptor(&descriptor, 0.0, ptr::null()) };
            if let Ok(matched) = file_match(&font, files, request)
                && matched.family == original.family
                && let Some(rank) = rank(&matched, ch)
                && best_rank.is_none_or(|current| rank < current)
            {
                best_rank = Some(rank);
                best = Some(matched);
            }
        }
    }
    best.map_or_else(|| require_coverage(original, ch), Ok)
}

fn family_descriptors(family: &str) -> Descriptors {
    // SAFETY: The dictionary contains the documented family-name string and is retained
    // throughout the call. Matching returns an immutable array of CTFontDescriptor objects.
    unsafe {
        let family = CFString::from_str(family);
        let attributes = CFDictionary::<CFString, CFString>::from_slices(
            &[kCTFontFamilyNameAttribute],
            &[&family],
        );
        CTFontDescriptor::with_attributes(attributes.as_opaque())
            .matching_font_descriptors(None)
            .map(|list| CFRetained::cast_unchecked(list))
    }
}

fn rank(matched: &Match, ch: Option<char>) -> Option<(bool, bool, f32)> {
    if ch.is_some_and(|ch| !covers(matched, ch)) {
        return None;
    }
    let font = Font::new(matched.bytes.clone(), matched.index)?;
    let attrs = matched.attributes(&font, matched.overrides(&font));
    Some((
        matched.request.style.italic != (attrs.style != Style::Normal),
        matched.request.synthetic(attrs).bold,
        (f32::from(matched.request.weight.unwrap_or(400)) - attrs.weight.value()).abs(),
    ))
}

fn require_coverage(matched: Match, ch: Option<char>) -> Result<Match, FontError> {
    if let Some(ch) = ch
        && !covers(&matched, ch)
    {
        return Err(FontError::MissingGlyph(u32::from(ch)));
    }
    Ok(matched)
}

fn covers(matched: &Match, ch: char) -> bool {
    let Ok(face) = FontRef::from_index(&matched.bytes, matched.index) else {
        return false;
    };
    face.head()
        .is_ok_and(|head| !head.flags().contains(Flags::LAST_RESORT_FONT))
        && face
            .charmap()
            .map(ch)
            .is_some_and(|glyph| face.outline_glyphs().get(glyph).is_some())
}

fn file_match(
    font: &CTFont,
    files: &mut HashMap<PathBuf, FontFile>,
    request: Request,
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
        request,
        bytes: file.bytes.clone(),
    })
}
