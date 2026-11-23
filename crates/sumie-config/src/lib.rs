#![forbid(unsafe_code)]

//! Owns settings definitions, loading, validation, hot reload, TOML write-back,
//! and JSON Schema generation. Does not manage windows or render.

use std::{
    borrow::Cow,
    env, fs, io,
    path::{Path, PathBuf},
};

use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;
use toml::Spanned;

const POINT_TO_PX: f32 = 96.0 / 72.0;
const MIN_WEIGHT: u16 = 100;
const MAX_WEIGHT: u16 = 900;
const MAX_INCLUDE_DEPTH: usize = 64;

#[derive(Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Document {
    /// Share settings across machines. Paths are relative to the file that lists them.
    /// Later includes override earlier ones; this file overrides all of them per key.
    #[schemars(with = "Vec<PathBuf>")]
    include: Vec<Spanned<PathBuf>>,
    font: Font,
}

/// Font selection and rasterization settings.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Font {
    /// Choose a different installed font family for the terminal grid.
    #[schemars(with = "String", default = "default_family")]
    family: Option<String>,
    /// Adjust text size for readability, in points at 96 DPI.
    #[serde(deserialize_with = "deserialize_size")]
    #[schemars(with = "f32", default = "default_size", transform = positive_number)]
    size: Option<f32>,
    /// Set a CSS weight for primary and fallback fonts. Omit to use platform matching.
    #[serde(deserialize_with = "deserialize_weight")]
    #[schemars(with = "u16", range(min = MIN_WEIGHT, max = MAX_WEIGHT))]
    weight: Option<u16>,
    /// Tune glyph grid fitting for legibility on low-DPI displays: off, light, or normal.
    #[schemars(with = "Hinting", default = "Hinting::default")]
    hinting: Option<Hinting>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Hinting {
    Off,
    #[default]
    Light,
    Normal,
}

impl Font {
    #[must_use]
    pub fn into_family(self) -> Cow<'static, str> {
        self.family
            .map_or_else(|| default_family().into(), Cow::Owned)
    }

    #[must_use]
    pub fn logical_px(&self) -> f32 {
        self.size.unwrap_or_else(default_size) * POINT_TO_PX
    }

    #[must_use]
    pub fn weight(&self) -> Option<u16> {
        self.weight
    }

    #[must_use]
    pub fn hinting(&self) -> Hinting {
        self.hinting.unwrap_or_default()
    }

    fn overlay(&mut self, file: Self) {
        self.family = file.family.or(self.family.take());
        self.size = file.size.or(self.size);
        self.weight = file.weight.or(self.weight);
        self.hinting = file.hinting.or(self.hinting);
    }
}

const fn default_family() -> &'static str {
    "JetBrains Mono"
}

const fn default_size() -> f32 {
    15.0
}

fn positive_number(schema: &mut Schema) {
    schema.insert("exclusiveMinimum".into(), 0.into());
}

fn deserialize_size<'de, D: Deserializer<'de>>(input: D) -> Result<Option<f32>, D::Error> {
    let size = f32::deserialize(input)?;
    if size > 0.0 && (size * POINT_TO_PX).is_finite() {
        Ok(Some(size))
    } else {
        Err(de::Error::custom(
            "font size must be a positive finite number of points",
        ))
    }
}

fn deserialize_weight<'de, D: Deserializer<'de>>(input: D) -> Result<Option<u16>, D::Error> {
    let weight = u16::deserialize(input)?;
    if (MIN_WEIGHT..=MAX_WEIGHT).contains(&weight) {
        Ok(Some(weight))
    } else {
        Err(de::Error::custom(format_args!(
            "font weight must be between {MIN_WEIGHT} and {MAX_WEIGHT}"
        )))
    }
}

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

/// Load fonts from `$XDG_CONFIG_HOME/sumie/sumie.toml` or `~/.config/sumie/sumie.toml`.
/// An absent main file uses defaults.
///
/// # Errors
/// Returns the path of an unreadable file or the location of invalid settings.
pub fn load_user_font() -> Result<Font, Error> {
    fn absolute_var(name: &str) -> Option<PathBuf> {
        env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    }

    absolute_var("XDG_CONFIG_HOME")
        .or_else(|| absolute_var("HOME").map(|home| home.join(".config")))
        .map_or_else(
            || Ok(Font::default()),
            |dir| load(&dir.join("sumie/sumie.toml")),
        )
}

fn load(path: &Path) -> Result<Font, Error> {
    match fs::read_to_string(path) {
        Ok(text) => load_document(path, &text, &mut Vec::new()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Font::default()),
        Err(source) => Err(Error::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

fn load_document(path: &Path, text: &str, active: &mut Vec<PathBuf>) -> Result<Font, Error> {
    let Document { include, font } = toml::from_str(text).map_err(|error: toml::de::Error| {
        invalid(
            path,
            text,
            error.span().map_or(0, |span| span.start),
            error.message().into(),
        )
    })?;
    if include.is_empty() {
        return Ok(font);
    }

    if active.is_empty() {
        let identity = fs::canonicalize(path).map_err(|source| Error::Read {
            path: path.to_owned(),
            source,
        })?;
        active.push(identity);
    }
    let mut merged = Font::default();
    for entry in include {
        let included = path.with_file_name(entry.get_ref());
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
        merged.overlay(load_document(&included, &source, active)?);
        active.pop();
    }
    merged.overlay(font);
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

fn schema() -> Schema {
    SchemaSettings::default()
        .with(|settings| settings.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<Document>()
}

/// Generate the JSON Schema for the main file and its includes.
///
/// # Errors
/// Returns a JSON serialization error if the generated schema cannot be encoded.
pub fn schema_json() -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&schema())
}

/// Generate a parseable example with descriptions and defaults from the schema.
#[must_use]
pub fn defaults_toml() -> String {
    let schema = schema();
    let properties = &schema.as_value()["properties"];
    let mut text = String::from("#:schema ./sumie.schema.json\n\n");
    write_default(&mut text, "include", &properties["include"]);
    text.push_str("\n[font]\n");
    if let Some(fields) = properties["font"]["properties"].as_object() {
        for (key, value) in fields {
            write_default(&mut text, key, value);
        }
    }
    text
}

fn write_default(text: &mut String, key: &str, schema: &Value) {
    use std::fmt::Write as _;

    if let Some(description) = schema["description"].as_str() {
        for line in description.lines() {
            writeln!(text, "# {line}").expect("writing to a String is infallible");
        }
    }
    if let Some(value) = schema.get("default").filter(|value| !value.is_null()) {
        writeln!(text, "{key} = {value}").expect("writing to a String is infallible");
    } else {
        writeln!(text, "# {key} (unset)").expect("writing to a String is infallible");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn load_font_configuration() {
        let dir = env::temp_dir().join(format!("sumie-config-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let scratch = Scratch(dir);
        let main = scratch.0.join("sumie.toml");
        fs::write(
            scratch.0.join("shared.toml"),
            "[font]\nfamily = 'Iosevka'\nsize = 12\nweight = 500\nhinting = 'off'\n",
        )
        .unwrap();

        let defaults = defaults_toml();
        let default_font = ("JetBrains Mono", 20.0_f32, None, Hinting::Light);
        let cases = [
            (
                "[font]\nfamily = 'Monospace'\nsize = 18\nweight = 600\nhinting = 'normal'\n",
                Ok(("Monospace", 24.0, Some(600), Hinting::Normal)),
            ),
            ("[font]\nunknown = true\n", Err((2, "unknown field"))),
            (
                "include = ['shared.toml']\n[font]\nsize = 15\nweight = 700\n",
                Ok(("Iosevka", 20.0, Some(700), Hinting::Off)),
            ),
            ("[font]\nhinting = 'strong'\n", Err((2, "unknown variant"))),
            (&defaults, Ok(default_font)),
        ];
        for (input, expected) in cases {
            fs::write(&main, input).unwrap();
            match expected {
                Ok((family, px, weight, hinting)) => {
                    let font = load(&main).unwrap();
                    assert_eq!(font.logical_px().to_bits(), px.to_bits());
                    assert_eq!(font.weight(), weight);
                    assert_eq!(font.hinting(), hinting);
                    assert_eq!(font.into_family(), family);
                }
                Err((expected_line, reason)) => match load(&main).unwrap_err() {
                    Error::Invalid {
                        path,
                        line,
                        column,
                        message,
                    } => {
                        assert_eq!(path, main);
                        assert_eq!(line, expected_line);
                        assert!((1..=12).contains(&column));
                        assert!(message.contains(reason), "{message}");
                    }
                    error @ Error::Read { .. } => panic!("{error}"),
                },
            }
        }
    }
}
