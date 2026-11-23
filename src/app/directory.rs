use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

pub(super) struct Directory {
    uri: Vec<u8>,
    pub(super) path: Option<PathBuf>,
    pub(super) label: String,
}

impl Directory {
    pub(super) fn at_spawn(cwd: &Path) -> Self {
        Self {
            uri: Vec::new(),
            path: None,
            label: basename(cwd),
        }
    }

    pub(super) fn update(&mut self, uri: &[u8], host: &[u8]) -> bool {
        if self.uri == uri {
            return false;
        }
        self.uri.clear();
        self.uri.extend_from_slice(uri);
        self.path = local_path(uri, host);
        self.label = self.path.as_deref().map_or_else(String::new, basename);
        true
    }
}

fn basename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn local_path(uri: &[u8], host: &[u8]) -> Option<PathBuf> {
    let location = uri.strip_prefix(b"file://")?;
    let slash = location.iter().position(|byte| *byte == b'/')?;
    let authority = &location[..slash];
    if !(authority.is_empty()
        || authority.eq_ignore_ascii_case(b"localhost")
        || authority.eq_ignore_ascii_case(host))
    {
        return None;
    }
    let mut bytes = location[slash..].iter().copied();
    let mut path = Vec::with_capacity(bytes.len());
    while let Some(byte) = bytes.next() {
        let byte = match byte {
            b'%' => {
                let hex = |b| u8::try_from(char::from(b).to_digit(16)?).ok();
                hex(bytes.next()?)? * 16 + hex(bytes.next()?)?
            }
            b'#' | b'?' => return None,
            b => b,
        };
        if byte == 0 {
            return None;
        }
        path.push(byte);
    }
    Some(PathBuf::from(OsString::from_vec(path)))
}

pub(super) fn shell_name() -> String {
    std::env::var_os("SHELL")
        .as_deref()
        .and_then(|shell| Path::new(shell).file_name())
        .map_or_else(
            || "shell".into(),
            |name| name.to_string_lossy().into_owned(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherits_only_local_file_paths() {
        for (uri, expected) in [
            ("file://workstation/a%20b", Some("/a b")),
            ("file:///", Some("/")),
            ("file://remote/tmp", None),
            ("file://localhost/a%00b", None),
            ("file:///bad%2", None),
        ] {
            assert_eq!(
                local_path(uri.as_bytes(), b"workstation").as_deref(),
                expected.map(Path::new)
            );
        }
    }
}
