use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use crate::Config;

const MAX_INCLUDE_DEPTH: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("{path}:{line}:{column}: {message}")]
    Invalid {
        path: PathBuf,
        line: usize,
        column: usize,
        message: String,
    },
}

/// Path of the primary configuration, if the environment identifies a config directory.
#[must_use]
pub fn user_config_path() -> Option<PathBuf> {
    fn absolute_var(name: &str) -> Option<PathBuf> {
        env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    }

    absolute_var("XDG_CONFIG_HOME")
        .or_else(|| absolute_var("HOME").map(|home| home.join(".config")))
        .map(|dir| dir.join("sumie/sumie.toml"))
}

/// Load the primary settings and its includes. An absent main file uses defaults.
///
/// # Errors
/// Returns the path of an unreadable file or the location of invalid settings.
pub fn load(path: &Path) -> Result<Config, Error> {
    load_visited(path, &mut |_| {})
}

pub(crate) fn load_visited(path: &Path, visit: &mut impl FnMut(&Path)) -> Result<Config, Error> {
    visit(path);
    match fs::read_to_string(path) {
        Ok(text) => load_document(path, &text, &mut Vec::new(), visit),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(source) => Err(Error::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

fn load_document(
    path: &Path,
    text: &str,
    active: &mut Vec<PathBuf>,
    visit: &mut impl FnMut(&Path),
) -> Result<Config, Error> {
    let mut file: Config = toml::from_str(text).map_err(|error: toml::de::Error| {
        invalid(
            path,
            text,
            error.span().map_or(0, |span| span.start),
            error.message().into(),
        )
    })?;
    let include = std::mem::take(&mut file.include);
    if include.is_empty() {
        return Ok(file);
    }

    if active.is_empty() {
        let identity = fs::canonicalize(path).map_err(|source| Error::Read {
            path: path.to_owned(),
            source,
        })?;
        active.push(identity);
    }
    let mut merged = Config::default();
    for entry in include {
        let included = path.with_file_name(entry.get_ref());
        visit(&included);
        let at_entry = |message| invalid(path, text, entry.span().start, message);
        let identity = fs::canonicalize(&included)
            .map_err(|error| at_entry(format!("{}: {error}", included.display())))?;
        if active.contains(&identity) {
            return Err(at_entry(format!(
                "include cycle through {}",
                included.display()
            )));
        }
        if active.len() == MAX_INCLUDE_DEPTH {
            return Err(at_entry(format!(
                "includes are nested more than {MAX_INCLUDE_DEPTH} files deep"
            )));
        }
        let source = fs::read_to_string(&included)
            .map_err(|error| at_entry(format!("{}: {error}", included.display())))?;
        active.push(identity);
        merged.overlay(load_document(&included, &source, active, visit)?);
        active.pop();
    }
    merged.overlay(file);
    Ok(merged)
}

fn invalid(path: &Path, text: &str, offset: usize, message: String) -> Error {
    let (mut line, mut column) = (1, 1);
    for byte in text.bytes().take(offset) {
        if byte == b'\n' {
            line += 1;
            column = 1;
        } else if byte & 0b1100_0000 != 0b1000_0000 {
            column += 1;
        }
    }
    Error::Invalid {
        path: path.to_owned(),
        line,
        column,
        message,
    }
}
