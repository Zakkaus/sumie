#![allow(unsafe_code)]

use std::{cell::OnceCell, collections::HashMap, ffi::c_void, path::PathBuf, ptr};

use objc2_core_foundation::{
    CFArray, CFCharacterSet, CFDictionary, CFRange, CFRetained, CFSet, CFString, CFURL,
    CFURLPathStyle, Type, kCFTypeSetCallBacks,
};
use objc2_core_text::{
    CTFont, CTFontDescriptor, CTFontSymbolicTraits, kCTFontCharacterSetAttribute,
    kCTFontFamilyNameAttribute, kCTFontURLAttribute,
};
use skrifa::{
    FontRef, MetadataProvider as _,
    font::Blob,
    raw::{FileRef, TableProvider as _, tables::head::Flags},
    string::StringId,
};

use super::{Font, FontError, FontStyle, Match, Request, Style};
use crate::Rejection;

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
        let Some(ch) = ch else {
            return matching_face(
                &self.primary,
                request,
                None,
                &mut self.files,
                &mut self.families,
            );
        };
        let mut rejected = Vec::new();
        self.preferred(ch, request, &mut rejected)
            .or_else(|| self.cascade(ch, request, &mut rejected))
            .or_else(|| self.system_wide(ch, request, &mut rejected))
            .ok_or_else(|| missing_glyph(ch, rejected))
    }

    /// Starts fallback at the system-wide step, skipping Core Text's preferred font and cascade.
    #[cfg(test)]
    pub fn find_system_wide(&mut self, ch: char, style: FontStyle) -> Result<Match, FontError> {
        let mut rejected = Vec::new();
        self.system_wide(ch, Request::new(self.weight, style), &mut rejected)
            .ok_or_else(|| missing_glyph(ch, rejected))
    }

    fn preferred(
        &mut self,
        ch: char,
        request: Request,
        rejected: &mut Vec<Rejection>,
    ) -> Option<Match> {
        let text = CFString::from_str(ch.encode_utf8(&mut [0; 4]));
        let range = CFRange {
            location: 0,
            length: text.length(),
        };
        // SAFETY: the range covers this live CFString in UTF-16 units.
        let preferred = unsafe { self.primary.for_string(&text, range) };
        self.candidate(&preferred, ch, request, rejected)
    }

    fn cascade(
        &mut self,
        ch: char,
        request: Request,
        rejected: &mut Vec<Rejection>,
    ) -> Option<Match> {
        self.cascade_list()?.iter().find_map(|descriptor| {
            // SAFETY: the descriptor is retained and null selects the identity transform.
            let font = unsafe { CTFont::with_font_descriptor(&descriptor, 0.0, ptr::null()) };
            // SAFETY: font is retained; character_set returns an owned CFCharacterSet.
            unsafe { font.character_set() }
                .is_long_character_member(u32::from(ch))
                .then(|| self.candidate(&font, ch, request, rejected))
                .flatten()
        })
    }

    fn cascade_list(&self) -> Descriptors {
        self.cascade
            .get_or_init(|| {
                // SAFETY: null selects system language preferences. Core Text returns an
                // immutable array of CTFontDescriptor objects, not unboxed values.
                unsafe {
                    self.primary
                        .default_cascade_list_for_languages(None)
                        .map(|list| CFRetained::cast_unchecked(list))
                }
            })
            .clone()
    }

    /// Asks Core Text for every installed face that claims the character, for fonts that the
    /// cascade omits or cannot load. Families earlier in the cascade, which follows the user's
    /// language order, come first; families absent from it come last. Ties prefer the requested
    /// italic and bold traits, then the PostScript name.
    fn system_wide(
        &mut self,
        ch: char,
        request: Request,
        rejected: &mut Vec<Rejection>,
    ) -> Option<Match> {
        let range = CFRange {
            location: u32::from(ch).try_into().ok()?,
            length: 1,
        };
        // SAFETY: the default allocator is valid and the range is one Unicode scalar value.
        let characters = unsafe { CFCharacterSet::with_characters_in_range(None, range) }?;
        // SAFETY: the set holds the exported, immutable character-set key, and the CFType
        // callbacks retain it for the set's lifetime.
        let mandatory = unsafe {
            let mut keys =
                [ptr::from_ref::<CFString>(kCTFontCharacterSetAttribute).cast::<c_void>()];
            CFSet::new(None, keys.as_mut_ptr(), 1, &raw const kCFTypeSetCallBacks)
        }?;
        // SAFETY: the dictionary maps the documented character-set key to a CFCharacterSet,
        // and the mandatory set names that key.
        let descriptors = unsafe {
            matching_descriptors(
                &CFDictionary::<CFString, CFCharacterSet>::from_slices(
                    &[kCTFontCharacterSetAttribute],
                    &[&characters],
                ),
                Some(&mandatory),
            )
        }?;
        let cascade: Vec<_> = self
            .cascade_list()
            .iter()
            .flat_map(|list| list.iter())
            .map(|descriptor| {
                // SAFETY: the descriptor is retained; the exported family-name key is immutable.
                unsafe { descriptor.attribute(kCTFontFamilyNameAttribute) }
                    .and_then(|family| family.downcast::<CFString>().ok())
                    .map(|family| family.to_string())
            })
            .collect();
        let italic = request.style.italic;
        let bold = request.weight.unwrap_or(400) >= 600;
        let mut fonts: Vec<_> = descriptors
            .iter()
            .map(|descriptor| {
                // SAFETY: the descriptor is retained and null selects the identity transform.
                // The font is retained for the family, trait and name queries.
                let (font, family, traits, name) = unsafe {
                    let font = CTFont::with_font_descriptor(&descriptor, 0.0, ptr::null());
                    let family = font.family_name().to_string();
                    let traits = font.symbolic_traits();
                    let name = font.post_script_name().to_string();
                    (font, family, traits, name)
                };
                let key = (
                    cascade
                        .iter()
                        .position(|cascaded| cascaded.as_deref() == Some(family.as_str()))
                        .unwrap_or(usize::MAX),
                    traits.contains(CTFontSymbolicTraits::TraitItalic) != italic,
                    traits.contains(CTFontSymbolicTraits::TraitBold) != bold,
                    name,
                );
                (key, font)
            })
            .collect();
        fonts.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        fonts
            .iter()
            .find_map(|(_, font)| self.candidate(font, ch, request, rejected))
    }

    fn candidate(
        &mut self,
        font: &CTFont,
        ch: char,
        request: Request,
        rejected: &mut Vec<Rejection>,
    ) -> Option<Match> {
        // SAFETY: font is retained throughout.
        let name = unsafe { font.post_script_name() }.to_string();
        // Earlier steps can offer the same face again.
        if rejected.iter().any(|tried| tried.font == name) {
            return None;
        }
        matching_face(font, request, Some(ch), &mut self.files, &mut self.families)
            .map_err(|error| {
                rejected.push(Rejection {
                    path: font_path(font),
                    font: name,
                    error,
                });
            })
            .ok()
    }
}

fn missing_glyph(ch: char, rejected: Vec<Rejection>) -> FontError {
    FontError::MissingGlyph {
        codepoint: u32::from(ch),
        rejected,
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
    let family = CFString::from_str(family);
    // SAFETY: the dictionary maps the documented family-name key to a string.
    unsafe {
        matching_descriptors(
            &CFDictionary::<CFString, CFString>::from_slices(
                &[kCTFontFamilyNameAttribute],
                &[&family],
            ),
            None,
        )
    }
}

/// Matches installed faces against `attributes`. Faces must match every attribute named in
/// `mandatory` exactly; Core Text may relax the others.
///
/// # Safety
///
/// Each key must be a Core Text descriptor attribute and its value must have that key's type.
/// `mandatory` must contain only such keys.
unsafe fn matching_descriptors<V: Type>(
    attributes: &CFDictionary<CFString, V>,
    mandatory: Option<&CFSet>,
) -> Descriptors {
    // SAFETY: the caller guarantees the attribute types and keys, and both collections are
    // retained throughout the call. Matching returns an immutable array of CTFontDescriptor
    // objects.
    unsafe {
        CTFontDescriptor::with_attributes(attributes.as_opaque())
            .matching_font_descriptors(mandatory)
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
        matched.request.synthetic_bold(attrs.weight, None),
        (f32::from(matched.request.weight.unwrap_or(400)) - attrs.weight.value()).abs(),
    ))
}

fn require_coverage(matched: Match, ch: Option<char>) -> Result<Match, FontError> {
    if let Some(ch) = ch
        && !covers(&matched, ch)
    {
        return Err(missing_glyph(ch, Vec::new()));
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
    // SAFETY: font is retained throughout.
    let (name, family) = unsafe { (font.post_script_name(), font.family_name()) };
    let path = font_path(font).ok_or_else(|| FontError::MissingFontUrl(name.to_string()))?;
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

fn font_path(font: &CTFont) -> Option<PathBuf> {
    // SAFETY: font is retained throughout; the exported URL key is immutable.
    let url = unsafe { font.attribute(kCTFontURLAttribute) }?;
    let path = url
        .downcast::<CFURL>()
        .ok()?
        .file_system_path(CFURLPathStyle::CFURLPOSIXPathStyle)?;
    Some(PathBuf::from(path.to_string()))
}
