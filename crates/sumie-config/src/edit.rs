//! Settings write-back: the file that defines a key, rebasing over foreign saves, validation.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use toml::{Table, Value};
use toml_edit::{DocumentMut, Item, TableLike};

use crate::{
    Error, Problem,
    load::{self, MAX_INCLUDE_DEPTH},
    write::{self, Refusal, Replaced, Snapshot},
};

mod values;
pub use values::EditValues;

/// Parse an array typed in the settings editor. The loader validates its elements when saved.
#[must_use]
pub fn parse_list(input: &str) -> Option<Value> {
    input.parse().ok().filter(Value::is_array)
}

/// Encode an array for editing with quoted paths or colors.
#[must_use]
pub fn format_list(value: &Value) -> String {
    to_edit(value).to_string()
}

/// Attempts after the first when the file changes between staging and the rename.
const RESTARTS: usize = 3;

/// Why a settings edit did not complete. Only [`WriteError::Unconfirmed`] follows a rename;
/// in every other case the file is unchanged.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// Another program changed `key` after the snapshot was read.
    #[error("{path}: {key} was changed by another program")]
    Conflict { path: PathBuf, key: String },
    /// The files fail to load, or the edit would add a problem; these are the problems.
    #[error("{}", .0.first().map(ToString::to_string).unwrap_or_default())]
    Invalid(Vec<Error>),
    #[error("cannot write {path}: {reason}")]
    Refused { path: PathBuf, reason: Refusal },
    /// Another program kept saving the file while it was being replaced.
    #[error("{path} kept changing while it was being saved")]
    Busy { path: PathBuf },
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    /// The file was replaced, but syncing its directory or reading it back failed or read
    /// other bytes, so the edit may not be on disk. Re-read the file before trusting it.
    #[error("{path} was replaced but could not be confirmed: {source}")]
    Unconfirmed { path: PathBuf, source: io::Error },
}

/// The file whose value of `key` the loader uses: `main` if it sets the key, otherwise the last
/// include that does, recursively. A key no file sets belongs to `main`.
#[must_use]
pub fn winner(main: &Path, key: &str) -> PathBuf {
    defining(main, key, &mut Vec::new()).unwrap_or_else(|| main.to_owned())
}

fn defining(path: &Path, key: &str, active: &mut Vec<PathBuf>) -> Option<PathBuf> {
    let table = parse(&fs::read(path).ok()?)?;
    if lookup(&table, key).is_some() {
        return Some(path.to_owned());
    }
    let identity = fs::canonicalize(path).ok()?;
    if active.contains(&identity) || active.len() == MAX_INCLUDE_DEPTH {
        return None;
    }
    active.push(identity);
    let found = includes(&table)
        .rev()
        .find_map(|entry| defining(&path.with_file_name(entry), key, active));
    active.pop();
    found
}

fn includes(table: &Table) -> impl DoubleEndedIterator<Item = &str> {
    table
        .get("include")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn parse(bytes: &[u8]) -> Option<Table> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn lookup<'a>(table: &'a Table, key: &str) -> Option<&'a Value> {
    match key.split_once('.') {
        Some((parent, name)) => table.get(parent)?.as_table()?.get(name),
        None => table.get(key),
    }
}

/// Set `key` to `value`, or remove it with `None`, in the file `snapshot` was read from, keeping
/// its comments and order. Returns the file as written.
///
/// `main` is the primary file. The merged settings must load, and the edit may add no problem
/// and leave none on `key`. A save made since `snapshot` is kept: the edit applies on top of it
/// when it left `key` alone and is refused otherwise, as is a save that set `key` in a file
/// that now overrides this one.
///
/// # Errors
/// Returns why nothing was written.
pub fn save(
    main: &Path,
    snapshot: &Snapshot,
    key: &str,
    value: Option<&Value>,
) -> Result<Snapshot, WriteError> {
    let failed = |source| WriteError::Io {
        path: snapshot.path.clone(),
        source,
    };
    for _ in 0..=RESTARTS {
        let current = Snapshot::read(&snapshot.path).map_err(failed)?;
        let before = problems(main, &current, &current.bytes);
        if before.iter().any(suspends) {
            return Err(WriteError::Invalid(before));
        }
        // A key another program set since in a file that overrides this one would shadow the
        // edit, so it counts as a change to the key.
        let defining = winner(main, key);
        if !same_file(&defining, &current) {
            return Err(WriteError::Conflict {
                path: defining,
                key: key.to_owned(),
            });
        }
        if current.bytes != snapshot.bytes
            && parse(&current.bytes)
                .as_ref()
                .and_then(|table| lookup(table, key))
                != parse(&snapshot.bytes)
                    .as_ref()
                    .and_then(|table| lookup(table, key))
        {
            return Err(WriteError::Conflict {
                path: snapshot.path.clone(),
                key: key.to_owned(),
            });
        }
        let text = edited(&current.bytes, key, value).map_err(failed)?;
        let after = problems(main, &current, text.as_bytes());
        let known = |error: &Error| before.iter().any(|old| same(old, error));
        if !after
            .iter()
            .all(|error| known(error) && !on_key(error, key))
        {
            return Err(WriteError::Invalid(after));
        }
        if let Replaced::Done(written) = write::replace(&current, text.as_bytes())? {
            return Ok(written);
        }
    }
    Err(WriteError::Busy {
        path: snapshot.path.clone(),
    })
}

/// Problems of the merged settings with `text` in place of `current`'s file.
fn problems(main: &Path, current: &Snapshot, text: &[u8]) -> Vec<Error> {
    let mut read = |path: &Path| {
        if same_file(path, current) {
            String::from_utf8(text.to_vec())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        } else {
            fs::read_to_string(path)
        }
    };
    load::check_with(main, &mut |_| {}, &mut read)
        .err()
        .unwrap_or_default()
}

/// Whether `path` names the file `current` was read from.
fn same_file(path: &Path, current: &Snapshot) -> bool {
    path == current.path || fs::canonicalize(path).is_ok_and(|real| real == current.real_path)
}

/// A problem that leaves the merged settings unknown, so no edit can be validated.
fn suspends(error: &Error) -> bool {
    match error {
        Error::Read { .. } => true,
        Error::Invalid { message, .. } => matches!(
            message,
            Problem::Syntax(_)
                | Problem::Read { .. }
                | Problem::IncludeCycle { .. }
                | Problem::IncludeDepth
        ),
    }
}

fn same(old: &Error, new: &Error) -> bool {
    matches!(
        (old, new),
        (Error::Invalid { path, message, .. }, Error::Invalid { path: new_path, message: new_message, .. })
            if path == new_path && message == new_message
    )
}

fn on_key(error: &Error, key: &str) -> bool {
    matches!(error, Error::Invalid { message, .. } if message.key() == Some(key))
}

fn edited(bytes: &[u8], key: &str, value: Option<&Value>) -> io::Result<String> {
    let invalid = |error: &dyn std::fmt::Display| {
        io::Error::new(io::ErrorKind::InvalidData, error.to_string())
    };
    let text = std::str::from_utf8(bytes).map_err(|error| invalid(&error))?;
    let mut document: DocumentMut = text.parse().map_err(|error| invalid(&error))?;
    let root = document.as_table_mut();
    let (parent, name) = key
        .split_once('.')
        .map_or((None, key), |(parent, name)| (Some(parent), name));
    if let Some(value) = value {
        let table = match parent {
            Some(parent) => root
                .entry(parent)
                .or_insert_with(toml_edit::table)
                .as_table_like_mut()
                .ok_or_else(|| invalid(&format_args!("{parent} is not a table")))?,
            None => root,
        };
        let mut new = to_edit(value);
        // Assign in place: the key keeps the comments above it, the value its spacing and
        // trailing comment.
        if let Some(item) = table.get_mut(name) {
            if let Some(old) = item.as_value() {
                *new.decor_mut() = old.decor().clone();
            }
            *item = Item::Value(new);
        } else {
            table.insert(name, Item::Value(new));
        }
    } else if let Some(table) = match parent {
        Some(parent) => root.get_mut(parent).and_then(Item::as_table_like_mut),
        None => Some(root as &mut dyn TableLike),
    } {
        table.remove(name);
    }
    Ok(document.to_string())
}

fn to_edit(value: &Value) -> toml_edit::Value {
    match value {
        Value::String(text) => text.as_str().into(),
        Value::Integer(number) => (*number).into(),
        Value::Float(number) => (*number).into(),
        Value::Boolean(flag) => (*flag).into(),
        Value::Datetime(datetime) => (*datetime).into(),
        Value::Array(items) => items
            .iter()
            .map(to_edit)
            .collect::<toml_edit::Array>()
            .into(),
        Value::Table(table) => table
            .iter()
            .map(|(key, value)| (key.as_str(), to_edit(value)))
            .collect::<toml_edit::InlineTable>()
            .into(),
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
