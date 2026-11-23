const SCHEMES: &[&str] = &[
    "http", "https", "ftp", "file", "mailto", "ssh", "git", "tel", "magnet", "ipfs", "ipns",
    "gemini", "gopher", "news",
];

/// A bounded, control-free URI for the desktop opener, never a bare path or option.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkUri(String);

impl LinkUri {
    /// Admit a local file URI or a URI with a scheme recognized by the pinned matcher.
    #[must_use]
    pub fn parse(raw: &str, local_hostname: &str) -> Option<Self> {
        if raw.len() > 4096 || raw.chars().any(forbidden) {
            return None;
        }
        let (scheme, body) = raw.split_once(':')?;
        if !SCHEMES.iter().any(|s| scheme.eq_ignore_ascii_case(s))
            || body.trim().is_empty()
            || (scheme.eq_ignore_ascii_case("file") && !local_file(body, local_hostname))
        {
            return None;
        }
        Some(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn forbidden(ch: char) -> bool {
    ch.is_control()
        || matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

fn local_file(body: &str, hostname: &str) -> bool {
    // Do not let a desktop handler reinterpret a backslash or UNC path as a host.
    if body.contains('\\') || !body.starts_with('/') {
        return false;
    }
    let Some(location) = body.strip_prefix("//") else {
        return true;
    };
    let authority = location.split(['/', '?', '#']).next().unwrap_or("");
    !location[authority.len()..].starts_with("//")
        && (authority.is_empty()
            || authority.eq_ignore_ascii_case("localhost")
            || authority.eq_ignore_ascii_case(hostname))
}

#[cfg(test)]
mod tests {
    use super::LinkUri;

    #[test]
    fn admits_only_local_files_and_control_free_uris() {
        for (uri, allowed) in [
            ("file:///tmp/a", true),
            ("file://LOCALHOST/a", true),
            ("file://host/a", true),
            ("file://remote.invalid/a", false),
            ("file://host@remote.invalid/a", false),
            ("file:////remote.invalid/a", false),
            ("https://local/a\u{202e}exe", false),
            ("https://local/a\u{85}b", false),
        ] {
            assert_eq!(LinkUri::parse(uri, "Host").is_some(), allowed, "{uri:?}");
        }
    }
}
