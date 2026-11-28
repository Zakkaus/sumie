//! Typed settings problems. Their English text is the source the translations follow.

use std::{fmt, path::PathBuf};

use serde::de::DeserializeOwned;
use toml::{
    Spanned,
    de::{DeTable, DeValue, ValueDeserializer},
};

use crate::{colors, load::MAX_INCLUDE_DEPTH, settings, window};

/// What is wrong with a settings file. [`crate::Error::Invalid`] locates it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The number at `key` lies outside `min..=max`.
    Range {
        key: &'static str,
        min: String,
        max: String,
    },
    /// `font.size` is not a positive finite number of points.
    FontSize,
    /// The value at `key` is not a `#rrggbb` color.
    Color {
        key: String,
        value: String,
    },
    /// The shortcut cannot be parsed as a bindable chord.
    Binding {
        key: String,
        value: String,
    },
    /// `key` is not a settings key.
    UnknownKey {
        key: String,
    },
    IncludeCycle {
        path: PathBuf,
    },
    IncludeDepth,
    /// An include could not be read.
    Read {
        path: PathBuf,
        detail: String,
    },
    /// A TOML syntax error or a value of the wrong type, in the parser's English.
    Syntax(String),
}

impl Problem {
    pub(crate) fn range(key: &'static str, min: impl fmt::Debug, max: impl fmt::Debug) -> Self {
        Self::Range {
            key,
            min: format!("{min:?}"),
            max: format!("{max:?}"),
        }
    }

    /// Dotted settings key the problem belongs to, if any.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        match self {
            Self::Range { key, .. } => Some(key),
            Self::FontSize => Some("font.size"),
            Self::Color { key, .. } | Self::Binding { key, .. } | Self::UnknownKey { key } => {
                Some(key)
            }
            _ => None,
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Range { key, min, max } => {
                let noun = match *key {
                    "minimum-contrast" => "minimum contrast",
                    "font.weight" => "font weight",
                    "window.background-opacity" => "background opacity",
                    other => other,
                };
                write!(formatter, "{noun} must be between {min} and {max}")
            }
            Self::FontSize => {
                formatter.write_str("font size must be a positive finite number of points")
            }
            Self::Color { value, .. } => write!(
                formatter,
                "invalid value: string {value:?}, expected a color in #rrggbb notation"
            ),
            Self::Binding { value, .. } => write!(formatter, "invalid shortcut chord {value:?}"),
            Self::UnknownKey { key } => unknown_key(formatter, key),
            Self::IncludeCycle { path } => {
                write!(formatter, "include cycle through {}", path.display())
            }
            Self::IncludeDepth => write!(
                formatter,
                "includes are nested more than {MAX_INCLUDE_DEPTH} files deep"
            ),
            Self::Read { path, detail, .. } => write!(formatter, "{}: {detail}", path.display()),
            Self::Syntax(message) => formatter.write_str(message),
        }
    }
}

/// Serde's wording for `deny_unknown_fields`, so the banner reads as it did before typing.
fn unknown_key(formatter: &mut fmt::Formatter<'_>, key: &str) -> fmt::Result {
    let (table, name) = key.rsplit_once('.').unwrap_or(("", key));
    let fields = settings::fields(table).unwrap_or_default();
    write!(formatter, "unknown field `{name}`, ")?;
    match fields.as_slice() {
        [] => formatter.write_str("there are no fields"),
        [only] => write!(formatter, "expected `{only}`"),
        [first, second] => write!(formatter, "expected `{first}` or `{second}`"),
        all => {
            formatter.write_str("expected one of ")?;
            for (index, field) in all.iter().enumerate() {
                let separator = if index == 0 { "" } else { ", " };
                write!(formatter, "{separator}`{field}`")?;
            }
            Ok(())
        }
    }
}

/// Every unknown key, out-of-range number and malformed color, ordered by byte offset.
/// Type errors are left to serde, which reports only the first.
pub(crate) fn audit(document: &DeTable<'_>) -> Vec<(usize, Problem)> {
    let mut problems = Vec::new();
    walk("", document, &mut problems);
    problems.sort_by_key(|(offset, _)| *offset);
    problems
}

fn walk(table: &str, entries: &DeTable<'_>, problems: &mut Vec<(usize, Problem)>) {
    let names = settings::fields(table).unwrap_or_default();
    for (key, value) in entries {
        let path = if table.is_empty() {
            key.get_ref().to_string()
        } else {
            format!("{table}.{}", key.get_ref())
        };
        if !names.contains(&key.get_ref().as_ref()) {
            problems.push((key.span().start, Problem::UnknownKey { key: path }));
        } else if let (true, DeValue::Table(inner)) = (table.is_empty(), value.get_ref())
            && settings::fields(&path).is_some()
        {
            walk(&path, inner, problems);
        } else {
            check(&path, value, problems);
        }
    }
}

fn check(path: &str, value: &Spanned<DeValue<'_>>, problems: &mut Vec<(usize, Problem)>) {
    let problem = match path {
        "minimum-contrast" => parsed(value).and_then(|ratio| crate::contrast(ratio).err()),
        "font.size" => parsed(value).and_then(|size| crate::font_size(size).err()),
        "font.weight" => parsed(value).and_then(|weight| crate::font_weight(weight).err()),
        "window.background-opacity" => {
            parsed(value).and_then(|opacity| window::opacity(opacity).err())
        }
        "colors.palette" => {
            for color in value.get_ref().as_array().into_iter().flatten() {
                check_color(path, color, problems);
            }
            return;
        }
        _ if path.starts_with("colors.") => return check_color(path, value, problems),
        _ if path.starts_with("keybinds.") => value.get_ref().as_str().and_then(|text| {
            sumie_core::Chord::parse(text)
                .is_none()
                .then(|| Problem::Binding {
                    key: path.to_owned(),
                    value: text.to_owned(),
                })
        }),
        _ => None,
    };
    problems.extend(problem.map(|problem| (value.span().start, problem)));
}

fn check_color(path: &str, value: &Spanned<DeValue<'_>>, problems: &mut Vec<(usize, Problem)>) {
    if let Some(text) = value.get_ref().as_str()
        && colors::parse(text).is_none()
    {
        problems.push((
            value.span().start,
            Problem::Color {
                key: path.to_owned(),
                value: text.to_owned(),
            },
        ));
    }
}

/// The value as serde would read it, or `None` for a type error serde reports itself.
fn parsed<T: DeserializeOwned>(value: &Spanned<DeValue<'_>>) -> Option<T> {
    T::deserialize(ValueDeserializer::from(value.clone())).ok()
}

#[cfg(test)]
mod tests;
