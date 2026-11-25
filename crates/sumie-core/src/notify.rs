//! Desktop notification text, admission and OSC 9;4 progress. The caller supplies the clock.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

/// At most one notification per interval (Ghostty `Surface.zig:6612`).
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
/// The same content is not sent again within this window.
pub const REPEAT_WINDOW: Duration = Duration::from_secs(5);
const TITLE_LIMIT: usize = 256;
const BODY_LIMIT: usize = 4096;

/// A notification request with control characters removed and each field bounded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

impl Notification {
    /// Payloads come from the program in the pane, so they are never trusted as markup or control text.
    #[must_use]
    pub fn from_bytes(title: &[u8], body: &[u8]) -> Self {
        Self {
            title: clean(title, TITLE_LIMIT),
            body: clean(body, BODY_LIMIT),
        }
    }
}

fn clean(bytes: &[u8], limit: usize) -> String {
    let mut text: String = String::from_utf8_lossy(&bytes[..bytes.len().min(limit * 4)])
        .chars()
        .filter(|c| (!c.is_control() && !is_format(*c)) || *c == '\n')
        .collect();
    let end = text.floor_char_boundary(limit);
    text.truncate(end);
    text
}

/// Unicode category Cf: invisible characters that reorder or hide text (bidi overrides and
/// isolates, zero-width spaces, the byte order mark). The zero-width joiner and non-joiner
/// (U+200C, U+200D) and tag characters stay, because emoji sequences, flags and several
/// scripts need them to render.
fn is_format(c: char) -> bool {
    matches!(u32::from(c),
        0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x8E2 | 0x180E
        | 0x200B | 0x200E | 0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
        | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F
        | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001)
}

/// Rate limit and repeat filter for one window. Only an admitted request starts the timers.
#[derive(Default)]
pub struct Gate {
    last: Option<(Instant, u64)>,
}

impl Gate {
    pub fn admit(&mut self, now: Instant, request: &Notification) -> bool {
        let mut hasher = DefaultHasher::new();
        (&request.title, &request.body).hash(&mut hasher);
        let digest = hasher.finish();
        if let Some((at, previous)) = self.last {
            let age = now.saturating_duration_since(at);
            if age < MIN_INTERVAL || (previous == digest && age < REPEAT_WINDOW) {
                return false;
            }
        }
        self.last = Some((now, digest));
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressState {
    Set,
    Error,
    Indeterminate,
    Paused,
}

/// A visible OSC 9;4 report. Removal is the absence of one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub state: ProgressState,
    /// 0 through 100 when the program sent a value.
    pub percent: Option<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_limits_rate_and_repeats_from_the_last_admitted_request() {
        let start = Instant::now();
        let table = [
            (0, "a", true),
            (500, "b", false),
            (1000, "b", true),
            (2000, "b", false),
            (5999, "b", false),
            (6000, "b", true),
            (6500, "c", false),
            (7000, "c", true),
            (8000, "b", true),
        ];
        let mut gate = Gate::default();
        for (ms, title, admitted) in table {
            let request = Notification::from_bytes(title.as_bytes(), b"body");
            let now = start + Duration::from_millis(ms);
            assert_eq!(gate.admit(now, &request), admitted, "{title} at {ms} ms");
        }
    }

    #[test]
    fn notification_text_is_clean_and_bounded() {
        let long = "é".repeat(TITLE_LIMIT);
        let note = Notification::from_bytes(
            format!("\x1b[31m{long}").as_bytes(),
            b"line one\nline\ttwo\x07\xff",
        );
        assert_eq!(note.title.len(), TITLE_LIMIT);
        assert!(note.title.starts_with("[31m"));
        assert_eq!(note.body, "line one\nlinetwo\u{fffd}");
    }

    #[test]
    fn invisible_format_characters_are_removed() {
        // Bidi override and isolate, zero-width space, byte order mark and soft hyphen go;
        // the joiner in an emoji sequence stays.
        let text = "a\u{202e}b\u{2066}c\u{200b}d\u{feff}e\u{ad}f\u{1f469}\u{200d}\u{1f4bb}";
        let note = Notification::from_bytes(text.as_bytes(), text.as_bytes());
        let kept = "abcdef\u{1f469}\u{200d}\u{1f4bb}";
        assert_eq!((note.title.as_str(), note.body.as_str()), (kept, kept));
    }
}
