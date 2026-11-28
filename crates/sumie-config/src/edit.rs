//! Settings write-back: the file that defines a key, rebasing over foreign saves, validation.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use toml::{Table, Value};
use toml_edit::{Document, Item, TableLike};

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
    walk_sources(main, &mut Vec::new(), &mut |snapshot, table| {
        lookup(&table, key).map(|_| snapshot.path)
    })
    .unwrap_or_else(|| main.to_owned())
}

// The editor needs the same winner for display, conflict detection, and write-back, even
// when a parseable file has a locally invalid setting that the user can repair.
fn walk_sources<T>(
    path: &Path,
    active: &mut Vec<PathBuf>,
    visit: &mut impl FnMut(Snapshot, Table) -> Option<T>,
) -> Option<T> {
    if active.len() == MAX_INCLUDE_DEPTH {
        return None;
    }
    let snapshot = Snapshot::read(path).ok()?;
    if active.contains(&snapshot.real_path) {
        return None;
    }
    let table = parse(&snapshot.bytes).unwrap_or_default();
    let children: Vec<_> = includes(&table)
        .rev()
        .map(|entry| path.with_file_name(entry))
        .collect();
    active.push(snapshot.real_path.clone());
    let found = visit(snapshot, table).or_else(|| {
        children
            .iter()
            .find_map(|child| walk_sources(child, active, visit))
    });
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
            load::read_file(path)
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
    let document = Document::parse(text).map_err(|error| invalid(&error))?;
    let (parent, name) = key
        .split_once('.')
        .map_or((None, key), |(parent, name)| (Some(parent), name));
    if value.is_none()
        && let Some(text) = remove_assignment(&document, parent, name)
    {
        return Ok(text);
    }
    let mut document = document.into_mut();
    let root = document.as_table_mut();
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

// Remove the assignment but leave its comments, including those inside an array.
fn remove_assignment(
    document: &Document<&str>,
    parent: Option<&str>,
    name: &str,
) -> Option<String> {
    let (inline, table): (bool, &dyn TableLike) = match parent {
        Some(parent) => {
            let item = document.get(parent)?;
            (item.is_inline_table(), item.as_table_like()?)
        }
        None => (false, document.as_table()),
    };
    let (key, item) = table.get_key_value(name)?;
    let value = item.as_value()?;
    let text = document.raw();
    let key_start = key.span()?.start;
    let value_end = value.span()?.end;
    let mut output = String::with_capacity(text.len());
    if inline {
        let suffix_end = value
            .decor()
            .suffix()
            .and_then(toml_edit::RawString::span)
            .map_or(value_end, |span| span.end);
        let suffix = &text[suffix_end..];
        if let Some(after_comma) = suffix.strip_prefix(',') {
            output.push_str(&text[..key_start]);
            keep_array_comments(&mut output, text, value);
            output.push_str(&text[value_end..suffix_end]);
            output.push_str(after_comma);
        } else {
            // Last field: its leading trivia starts just after the previous separator.
            let prefix_start = key
                .leaf_decor()
                .prefix()
                .and_then(toml_edit::RawString::span)
                .map_or(key_start, |span| span.start);
            let prefix = &text[..prefix_start];
            output.push_str(prefix.strip_suffix(',').unwrap_or(prefix));
            output.push_str(&text[prefix_start..key_start]);
            keep_array_comments(&mut output, text, value);
            output.push_str(&text[value_end..]);
        }
        return Some(output);
    }
    let start = text[..key_start].rfind('\n').map_or(0, |index| index + 1);
    let end = text[value_end..]
        .find('\n')
        .map_or(text.len(), |index| value_end + index + 1);
    output.push_str(&text[..start]);
    keep_array_comments(&mut output, text, value);
    let trailing = text[value_end..end].trim_start_matches([' ', '\t']);
    if trailing.starts_with('#') {
        let indent = text[start..]
            .bytes()
            .take_while(|b| matches!(b, b' ' | b'\t'))
            .count();
        output.push_str(&text[start..start + indent]);
        output.push_str(trailing);
    }
    output.push_str(&text[end..]);
    Some(output)
}

fn keep_array_comments(output: &mut String, text: &str, value: &toml_edit::Value) {
    let Some(array) = value.as_array() else {
        return;
    };
    for trivia in array
        .iter()
        .flat_map(|item| [item.decor().prefix(), item.decor().suffix()])
        .flatten()
        .chain([array.trailing()])
        .filter_map(toml_edit::RawString::span)
    {
        for line in text[trivia].split_inclusive('\n') {
            if line.trim_start_matches([' ', '\t']).starts_with('#') {
                output.push_str(line);
            }
        }
    }
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
