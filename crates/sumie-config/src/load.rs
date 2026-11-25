use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use serde::Deserialize as _;
use toml::de::{DeTable, Deserializer};

use crate::{Config, Problem, problem};

pub(crate) const MAX_INCLUDE_DEPTH: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    /// `message` is typed; its `Display` is the English diagnostic.
    #[error("{path}:{line}:{column}: {message}")]
    Invalid {
        path: PathBuf,
        line: usize,
        column: usize,
        message: Problem,
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
/// Returns the path of an unreadable file or the first problem in the first invalid file.
pub fn load(path: &Path) -> Result<Config, Error> {
    load_visited(path, &mut |_| {})
}

/// Load like [`load`], but report every problem in the first invalid file, in file order.
///
/// # Errors
/// Returns one or more errors, all from the same file.
pub fn check(path: &Path) -> Result<Config, Vec<Error>> {
    check_visited(path, &mut |_| {})
}

pub(crate) fn load_visited(path: &Path, visit: &mut impl FnMut(&Path)) -> Result<Config, Error> {
    check_visited(path, visit)
        .map_err(|errors| errors.into_iter().next().expect("errors are never empty"))
}

fn check_visited(path: &Path, visit: &mut impl FnMut(&Path)) -> Result<Config, Vec<Error>> {
    check_with(path, visit, &mut |path| fs::read_to_string(path))
}

/// Reads a settings file; the editor substitutes the text it is about to write.
pub(crate) type Read<'a> = &'a mut dyn FnMut(&Path) -> io::Result<String>;

pub(crate) fn check_with(
    path: &Path,
    visit: &mut impl FnMut(&Path),
    read: Read<'_>,
) -> Result<Config, Vec<Error>> {
    visit(path);
    match read(path) {
        Ok(text) => load_document(path, &text, &mut Vec::new(), visit, read),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(source) => Err(vec![Error::Read {
            path: path.to_owned(),
            source,
        }]),
    }
}

fn load_document(
    path: &Path,
    text: &str,
    active: &mut Vec<PathBuf>,
    visit: &mut impl FnMut(&Path),
    read: Read<'_>,
) -> Result<Config, Vec<Error>> {
    let syntax = |error: toml::de::Error| {
        let offset = error.span().map_or(0, |span| span.start);
        vec![invalid(
            path,
            text,
            offset,
            Problem::Syntax(error.message().into()),
        )]
    };
    let document = DeTable::parse(text).map_err(syntax)?;
    let problems = problem::audit(document.get_ref());
    if !problems.is_empty() {
        return Err(problems
            .into_iter()
            .map(|(offset, problem)| invalid(path, text, offset, problem))
            .collect());
    }
    let mut file = Config::deserialize(Deserializer::from(document)).map_err(syntax)?;
    let include = std::mem::take(&mut file.include);
    if include.is_empty() {
        return Ok(file);
    }

    if active.is_empty() {
        let identity = fs::canonicalize(path).map_err(|source| {
            vec![Error::Read {
                path: path.to_owned(),
                source,
            }]
        })?;
        active.push(identity);
    }
    let mut merged = Config::default();
    for entry in include {
        let included = path.with_file_name(entry.get_ref());
        visit(&included);
        let at_entry = |problem| vec![invalid(path, text, entry.span().start, problem)];
        let unreadable = |error: io::Error| {
            at_entry(Problem::Read {
                path: included.clone(),
                kind: error.kind(),
                detail: error.to_string(),
            })
        };
        let identity = fs::canonicalize(&included).map_err(unreadable)?;
        if active.contains(&identity) {
            return Err(at_entry(Problem::IncludeCycle { path: included }));
        }
        if active.len() == MAX_INCLUDE_DEPTH {
            return Err(at_entry(Problem::IncludeDepth));
        }
        let source = read(&included).map_err(unreadable)?;
        active.push(identity);
        merged.overlay(load_document(&included, &source, active, visit, read)?);
        active.pop();
    }
    merged.overlay(file);
    Ok(merged)
}

fn invalid(path: &Path, text: &str, offset: usize, message: Problem) -> Error {
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
