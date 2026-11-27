use std::time::{Duration, Instant};

use super::{Discovery, drawable};
use crate::{FontError, FontOptions, FontStyle, FontSystem};

impl Discovery {
    /// Repeats the system-wide step until the worker answers.
    pub fn resolve_system_wide(
        &mut self,
        ch: char,
        style: FontStyle,
    ) -> Result<super::Match, FontError> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match self.find_system_wide(ch, style) {
                Err(FontError::MissingGlyph { pending: true, .. }) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                found => return found,
            }
        }
    }
}

#[test]
fn drawable_excludes_noncharacters_unassigned_and_private_use() {
    for ch in [
        '\u{fdd0}',
        '\u{fffe}',
        '\u{1ffff}',
        '\u{10ffff}',
        '\u{378}',
        '\u{e0080}',
        '\u{e000}',
        '\u{f0000}',
        '\u{10fffd}',
    ] {
        assert!(!drawable(ch), "U+{:04X}", u32::from(ch));
    }
    for ch in ['A', '水', '\u{1f9ff}', '\u{2a6d6}'] {
        assert!(drawable(ch), "U+{:04X}", u32::from(ch));
    }
}

#[test]
fn undrawable_misses_skip_the_system_wide_step() {
    let mut fonts = FontSystem::new("Menlo", 20.0, 1.0, FontOptions::default()).unwrap();
    // Counted only: the step never reaches Core Text, so the count alone shows whether it ran.
    fonts.discovery.system_wide_counted_only = true;
    for ch in ['\u{fdd0}', '\u{10ffff}', '\u{378}', '\u{e000}'] {
        let _ = fonts.face_for(ch, FontStyle::default());
    }
    assert_eq!(fonts.discovery.system_wide_lookups, 0);
}

#[test]
fn a_slow_system_wide_lookup_is_pending_after_the_wait() {
    let mut discovery = Discovery::new("Menlo", None).unwrap();
    discovery.system_wide_delay = Duration::from_millis(500);
    let style = FontStyle::default();
    // Neither lookup may wait for the answer. A hosted runner's scheduler adds over 100 ms
    // on its own, so the bound is half the delay, not the wait itself.
    let limit = discovery.system_wide_delay / 2;
    for ch in ['水', '火'] {
        // The second character queues behind the first, so it does not wait.
        let start = Instant::now();
        let found = discovery.find_system_wide(ch, style);
        let elapsed = start.elapsed();
        assert!(
            matches!(found, Err(FontError::MissingGlyph { pending: true, .. })),
            "{ch}: {:?}",
            found.map(|matched| matched.family)
        );
        assert!(elapsed < limit, "{ch}: {elapsed:?}");
    }
    for ch in ['水', '火'] {
        let matched = discovery
            .resolve_system_wide(ch, style)
            .unwrap_or_else(|error| panic!("{ch}: {error}"));
        assert!(matched.family.contains(char::is_alphabetic), "{ch}");
    }
}
