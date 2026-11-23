const SCHEMES: &[&str] = &[
    "http", "https", "ftp", "file", "mailto", "ssh", "git", "tel", "magnet", "ipfs", "ipns",
    "gemini", "gopher", "news",
];

/// A bounded, control-free URI for the desktop opener, never a bare path or option.
#[derive(Clone)]
pub struct LinkUri(String);

impl LinkUri {
    /// Strip control characters and admit only the schemes recognized by the pinned URL matcher.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.len() > 4096 {
            return None;
        }
        let (scheme, body) = raw.split_once(':')?;
        if !SCHEMES.iter().any(|s| scheme.eq_ignore_ascii_case(s))
            || !body.chars().any(|c| !c.is_control() && !c.is_whitespace())
        {
            return None;
        }
        Some(Self(raw.chars().filter(|c| !c.is_control()).collect()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
