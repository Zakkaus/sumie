#![allow(unsafe_code)]

use std::{
    cell::OnceCell,
    collections::{HashMap, HashSet},
    ffi::c_void,
    path::PathBuf,
    ptr,
    sync::mpsc,
    thread,
    time::Duration,
};

use objc2_core_foundation::{
    CFArray, CFCharacterSet, CFCharacterSetPredefinedSet, CFDictionary, CFRange, CFRetained, CFSet,
    CFString, CFURL, CFURLPathStyle, Type, kCFTypeSetCallBacks,
};
use objc2_core_text::{
    CTFont, CTFontCollection, CTFontDescriptor, CTFontSymbolicTraits, kCTFontCharacterSetAttribute,
    kCTFontFamilyNameAttribute, kCTFontURLAttribute,
};
use skrifa::{
    FontRef, MetadataProvider as _,
    raw::{TableProvider as _, tables::head::Flags},
};

use super::{Font, FontError, FontStyle, Match, Request, Style, is_nerd_family, is_private_use};
use crate::Rejection;
use file::FontFile;

mod file;
mod primary;
#[cfg(test)]
mod tests;

/// Longest a lookup waits for the system-wide step. Core Text's query has no time limit (on a
/// hosted macOS runner, one missing glyph kept a test busy for minutes), and the caller may be
/// the main thread, so the step runs on a worker thread.
const SYSTEM_WIDE_WAIT: Duration = Duration::from_millis(50);

pub(crate) struct Discovery {
    family: String,
    matcher: Matcher,
    system_wide: Option<SystemWide>,
    /// System-wide lookups, counted for tests of the undrawable-character guard.
    #[cfg(test)]
    pub system_wide_lookups: usize,
    /// Counts system-wide lookups without asking Core Text: on macOS 15 a query for an
    /// undrawable character can stay unanswered for minutes.
    #[cfg(test)]
    pub system_wide_counted_only: bool,
    /// Extra time the worker takes per lookup, for tests of the wait bound.
    #[cfg(test)]
    pub system_wide_delay: Duration,
}

struct Matcher {
    primary: CFRetained<CTFont>,
    /// Whether the family was not found and [`primary::MISSING_FAMILY_SUBSTITUTE`] replaced it.
    substituted: bool,
    weight: Option<u16>,
    files: HashMap<PathBuf, FontFile>,
    families: HashMap<String, Descriptors>,
    cascade: OnceCell<Descriptors>,
    pua_fonts: OnceCell<Vec<(CFRetained<CTFont>, CFRetained<CFCharacterSet>)>>,
}

impl Discovery {
    pub fn new(family: &str, weight: Option<u16>) -> Result<Self, FontError> {
        if family.contains('\0') {
            return Err(FontError::InvalidFamily);
        }
        Ok(Self {
            family: family.to_owned(),
            matcher: Matcher::new(family, weight),
            system_wide: None,
            #[cfg(test)]
            system_wide_lookups: 0,
            #[cfg(test)]
            system_wide_counted_only: false,
            #[cfg(test)]
            system_wide_delay: Duration::ZERO,
        })
    }

    pub const fn substituted(&self) -> bool {
        self.matcher.substituted
    }

    pub fn find(&mut self, ch: Option<char>, style: FontStyle) -> Result<Match, FontError> {
        let request = Request::new(self.matcher.weight, style);
        let Some(ch) = ch else {
            return self.matcher.primary(request);
        };
        let mut rejected = Vec::new();
        if is_private_use(ch) {
            let matched = self.matcher.installed(ch, request, &mut rejected);
            return matched.ok_or_else(|| missing_glyph(ch, rejected, false));
        }
        if let Some(matched) = self
            .matcher
            .preferred(ch, request, &mut rejected)
            .or_else(|| self.matcher.cascade(ch, request, &mut rejected))
        {
            return Ok(matched);
        }
        if !drawable(ch) {
            return Err(missing_glyph(ch, rejected, false));
        }
        self.system_wide(ch, request, rejected)
    }

    /// Starts fallback at the system-wide step, skipping Core Text's preferred font and cascade.
    #[cfg(test)]
    pub fn find_system_wide(&mut self, ch: char, style: FontStyle) -> Result<Match, FontError> {
        let request = Request::new(self.matcher.weight, style);
        self.system_wide(ch, request, Vec::new())
    }

    /// Asks the worker for every installed face that claims the character. A character it has
    /// not answered within [`SYSTEM_WIDE_WAIT`] is reported as pending, and a later lookup
    /// takes the answer.
    fn system_wide(
        &mut self,
        ch: char,
        request: Request,
        mut rejected: Vec<Rejection>,
    ) -> Result<Match, FontError> {
        #[cfg(test)]
        {
            self.system_wide_lookups += 1;
            if self.system_wide_counted_only {
                return Err(missing_glyph(ch, rejected, false));
            }
        }
        if self.system_wide.is_none() {
            self.system_wide = self.spawn_system_wide();
        }
        let answer = match &mut self.system_wide {
            Some(worker) => worker.answer((ch, request)),
            None => Some(Err(Vec::new())),
        };
        match answer {
            Some(Ok(matched)) => Ok(matched),
            Some(Err(more)) => {
                // The worker does not know which faces the earlier steps rejected.
                let more: Vec<_> = more
                    .into_iter()
                    .filter(|new| rejected.iter().all(|tried| tried.font != new.font))
                    .collect();
                rejected.extend(more);
                Err(missing_glyph(ch, rejected, false))
            }
            None => Err(missing_glyph(ch, rejected, true)),
        }
    }

    fn spawn_system_wide(&self) -> Option<SystemWide> {
        let (requests, queue) = mpsc::channel::<Key>();
        let (replies, answers) = mpsc::channel();
        let family = self.family.clone();
        let weight = self.matcher.weight;
        #[cfg(test)]
        let delay = self.system_wide_delay;
        thread::Builder::new()
            .name("sumie-font-fallback".to_owned())
            .spawn(move || {
                let mut matcher = Matcher::new(&family, weight);
                for (ch, request) in queue {
                    #[cfg(test)]
                    thread::sleep(delay);
                    let mut rejected = Vec::new();
                    let answer = matcher.installed(ch, request, &mut rejected);
                    if replies
                        .send(((ch, request), answer.ok_or(rejected)))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .ok()?;
        Some(SystemWide {
            requests,
            answers,
            sent: HashSet::new(),
            answered: HashMap::new(),
        })
    }
}

type Key = (char, Request);
type Answer = Result<Match, Vec<Rejection>>;

/// The system-wide step's worker thread, which ends when this handle is dropped and its
/// current query returns. Each character and request is sent once.
struct SystemWide {
    requests: mpsc::Sender<Key>,
    answers: mpsc::Receiver<(Key, Answer)>,
    sent: HashSet<Key>,
    answered: HashMap<Key, Answer>,
}

impl SystemWide {
    /// Returns the worker's answer, or `None` while it is pending. A new request waits up to
    /// [`SYSTEM_WIDE_WAIT`], unless an earlier one is still running: that query has already
    /// overrun its wait, and this one queues behind it.
    fn answer(&mut self, key: Key) -> Option<Answer> {
        if self.sent.insert(key) {
            if self.requests.send(key).is_err() {
                return Some(Err(Vec::new()));
            }
            if self.sent.len() == 1
                && let Ok(reply) = self.answers.recv_timeout(SYSTEM_WIDE_WAIT)
            {
                self.receive(reply);
            }
        }
        loop {
            match self.answers.try_recv() {
                Ok(reply) => self.receive(reply),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Some(self.answered.remove(&key).unwrap_or(Err(Vec::new())));
                }
            }
        }
        self.answered.remove(&key)
    }

    fn receive(&mut self, (key, answer): (Key, Answer)) {
        self.sent.remove(&key);
        self.answered.insert(key, answer);
    }
}

/// Whether to run the general Core Text query. Noncharacters, unassigned characters and
/// private use skip it; Core Foundation has no assignment data for planes 4 through 13.
fn drawable(ch: char) -> bool {
    let code = u32::from(ch);
    let noncharacter = (0xfdd0..=0xfdef).contains(&code) || code & 0xfffe == 0xfffe;
    !noncharacter
        && !is_private_use(ch)
        && !CFCharacterSet::predefined(CFCharacterSetPredefinedSet::Illegal)
            .is_some_and(|unassigned| unassigned.is_long_character_member(code))
}

impl Matcher {
    fn new(family: &str, weight: Option<u16>) -> Self {
        let (primary, substituted) = primary::resolve(family);
        Self {
            primary,
            substituted,
            weight,
            files: HashMap::new(),
            families: HashMap::new(),
            cascade: OnceCell::new(),
            pua_fonts: OnceCell::new(),
        }
    }

    fn primary(&mut self, request: Request) -> Result<Match, FontError> {
        matching_face(
            &self.primary,
            request,
            None,
            &mut self.files,
            &mut self.families,
        )
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
            let font = descriptor_font(&descriptor);
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

    fn installed(
        &mut self,
        ch: char,
        request: Request,
        rejected: &mut Vec<Rejection>,
    ) -> Option<Match> {
        let private = is_private_use(ch);
        let descriptors = (!private).then(|| character_descriptors(ch)).flatten();
        let pua_fonts = private.then(|| self.pua_fonts.get_or_init(installed_character_sets));
        let cascade = descriptors.as_ref().and_then(|_| self.cascade_list());
        let cascade: Vec<_> = cascade
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
            .flat_map(|list| list.iter())
            .map(|descriptor| descriptor_font(&descriptor))
            .chain(
                pua_fonts
                    .into_iter()
                    .flatten()
                    .filter(|(_, set)| set.is_long_character_member(ch.into()))
                    .map(|(font, _)| font.clone()),
            )
            .map(|font| {
                // SAFETY: the font is retained throughout the name and trait queries.
                let key = unsafe {
                    let family = font.family_name().to_string();
                    let traits = font.symbolic_traits();
                    (
                        private && !is_nerd_family(&family),
                        cascade
                            .iter()
                            .position(|cascaded| cascaded.as_deref() == Some(family.as_str()))
                            .unwrap_or(usize::MAX),
                        traits.contains(CTFontSymbolicTraits::TraitItalic) != italic,
                        traits.contains(CTFontSymbolicTraits::TraitBold) != bold,
                        font.post_script_name().to_string(),
                    )
                };
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

fn missing_glyph(ch: char, rejected: Vec<Rejection>, pending: bool) -> FontError {
    FontError::MissingGlyph {
        codepoint: u32::from(ch),
        rejected,
        pending,
    }
}

type Descriptors = Option<CFRetained<CFArray<CTFontDescriptor>>>;

fn descriptor_font(descriptor: &CTFontDescriptor) -> CFRetained<CTFont> {
    // SAFETY: the descriptor is retained and null selects the identity transform.
    unsafe { CTFont::with_font_descriptor(descriptor, 0.0, ptr::null()) }
}

fn installed_character_sets() -> Vec<(CFRetained<CTFont>, CFRetained<CFCharacterSet>)> {
    // SAFETY: no options are passed. The collection returns an array of CTFontDescriptors;
    // each font is retained while Core Text copies its character set.
    unsafe {
        CTFontCollection::from_available_fonts(None)
            .matching_font_descriptors()
            .map(|list| CFRetained::cast_unchecked::<CFArray<CTFontDescriptor>>(list))
            .iter()
            .flat_map(|list| list.iter())
            .map(|descriptor| {
                let font = descriptor_font(&descriptor);
                let characters = font.character_set();
                (font, characters)
            })
            .collect()
    }
}

fn character_descriptors(ch: char) -> Descriptors {
    let range = CFRange {
        location: u32::from(ch).try_into().ok()?,
        length: 1,
    };
    // SAFETY: the default allocator is valid and the range is one Unicode scalar value.
    let characters = unsafe { CFCharacterSet::with_characters_in_range(None, range) }?;
    // SAFETY: the set holds the exported, immutable character-set key, and the CFType
    // callbacks retain it for the set's lifetime.
    let mandatory = unsafe {
        let mut keys = [ptr::from_ref::<CFString>(kCTFontCharacterSetAttribute).cast::<c_void>()];
        CFSet::new(None, keys.as_mut_ptr(), 1, &raw const kCFTypeSetCallBacks)
    }?;
    // SAFETY: the mandatory key is the character-set attribute and its value is a CFCharacterSet.
    unsafe {
        matching_descriptors(
            &CFDictionary::<CFString, CFCharacterSet>::from_slices(
                &[kCTFontCharacterSetAttribute],
                &[&characters],
            ),
            Some(&mandatory),
        )
    }
}

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
            let font = descriptor_font(&descriptor);
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
        return Err(missing_glyph(ch, Vec::new(), false));
    }
    Ok(matched)
}

fn covers(matched: &Match, ch: char) -> bool {
    let Ok(face) = FontRef::from_index(&matched.bytes, matched.index) else {
        return false;
    };
    face.head()
        .is_ok_and(|head| !head.flags().contains(Flags::LAST_RESORT_FONT))
        && face.charmap().map(ch).is_some_and(|glyph| {
            face.outline_glyphs().get(glyph).is_some()
                || (matched.color_name.is_some() && has_sbix(&face, glyph))
        })
}

pub(super) fn has_sbix(face: &FontRef<'_>, glyph: skrifa::GlyphId) -> bool {
    // Core Text decodes private image types too; only check that the strike has data.
    face.sbix().is_ok_and(|table| {
        table
            .strikes()
            .iter()
            .flatten()
            .any(|strike| strike.glyph_data(glyph).ok().flatten().is_some())
    })
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
        color_name: FontRef::from_index(&file.bytes, index)
            .ok()
            .filter(|face| face.sbix().is_ok())
            .map(|_| name.to_string()),
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
