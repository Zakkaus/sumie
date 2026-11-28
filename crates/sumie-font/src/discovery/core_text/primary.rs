use std::ptr;

use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_text::{CTFont, kCTFontFamilyNameKey};

/// The monospaced system face that stands in for a family Core Text cannot find. Core Text's
/// own substitute is Helvetica, which is proportional, so its glyphs overlap in the grid.
pub(super) const MISSING_FAMILY_SUBSTITUTE: &str = "Menlo";

/// Core Text has no generic families, so these name the faces that stand in for them.
const GENERIC_FAMILIES: [(&str, &str); 2] = [
    ("monospace", MISSING_FAMILY_SUBSTITUTE),
    ("sans-serif", "Helvetica"),
];

/// Returns the primary font for `family`, and whether the family was not found and
/// [`MISSING_FAMILY_SUBSTITUTE`] replaced it.
pub(super) fn resolve(family: &str) -> (CFRetained<CTFont>, bool) {
    let family = GENERIC_FAMILIES
        .iter()
        .find(|(generic, _)| generic.eq_ignore_ascii_case(family))
        .map_or(family, |(_, face)| face);
    let named = named_font(family);
    if names(&named).any(|name| name.eq_ignore_ascii_case(family)) {
        (named, false)
    } else {
        (named_font(MISSING_FAMILY_SUBSTITUTE), true)
    }
}

/// Core Text returns a substitute rather than failing for a name it cannot find.
fn named_font(name: &str) -> CFRetained<CTFont> {
    // SAFETY: the name outlives the call; null selects the identity transform.
    unsafe { CTFont::with_name(&CFString::from_str(name), 0.0, ptr::null()) }
}

/// The names that Core Text matches a requested name against: family, PostScript and full
/// names, in English and in the user's language.
fn names(font: &CTFont) -> impl Iterator<Item = String> {
    // SAFETY: font is retained; the exported name key is immutable and null skips reporting
    // the matched language.
    let names = unsafe {
        [
            Some(font.family_name()),
            Some(font.post_script_name()),
            Some(font.full_name()),
            Some(font.display_name()),
            font.localized_name(kCTFontFamilyNameKey, ptr::null_mut()),
        ]
    };
    names.into_iter().flatten().map(|name| name.to_string())
}
