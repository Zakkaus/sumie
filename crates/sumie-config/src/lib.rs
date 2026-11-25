#![forbid(unsafe_code)]

//! Owns settings definitions, loading, validation, hot reload, TOML write-back,
//! and JSON Schema generation. Does not manage windows or render.

mod changes;
mod colors;
mod cursor;
mod images;
mod load;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(target_os = "macos", path = "watch_macos.rs")]
mod watch;
mod window;

pub use changes::Changes;
pub use colors::Colors;
pub use cursor::{Cursor, CursorStyle};
pub use images::Images;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use load::load_visited;
pub use load::{Error, load, user_config_path};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use watch::Watch;
#[cfg(target_os = "macos")]
pub use watch::WatchEvent;
pub use window::Window;

use std::{borrow::Cow, path::PathBuf};

use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;
use toml::Spanned;

mod scroll;
pub use scroll::Scroll;
mod shell;
pub use shell::Shell;

/// Ratio of logical pixels at 96 DPI to typographic points.
pub const POINT_TO_PX: f32 = 96.0 / 72.0;
const MIN_WEIGHT: u16 = 100;
const MAX_WEIGHT: u16 = 900;

/// Settings for the terminal and its renderer.
#[derive(Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Share settings across machines. Paths are relative to the file that lists them.
    /// Later includes override earlier ones; this file overrides all of them per key.
    #[schemars(with = "Vec<PathBuf>")]
    #[serde(skip_serializing)]
    include: Vec<Spanned<PathBuf>>,
    /// Font selection and rasterization settings.
    pub font: Font,
    /// Default terminal cursor appearance.
    pub cursor: Cursor,
    /// Default terminal colors and selection overrides.
    pub colors: Colors,
    /// Kitty image placement settings.
    pub images: Images,
    /// Interactive login shell settings.
    pub shell: Shell,
    /// Pane padding and window background settings.
    pub window: Window,
    /// History scrolling behavior.
    pub scroll: Scroll,
    /// Minimum WCAG contrast ratio for text. Set to 1.0 to disable correction.
    #[serde(deserialize_with = "deserialize_contrast")]
    #[schemars(
        with = "f64",
        range(min = 1.0, max = 21.0),
        default = "default_contrast"
    )]
    minimum_contrast: Option<f64>,
    /// Approximate scrollback limit in bytes. Lowering it can discard history immediately.
    /// Set to 0 to clear and disable scrollback.
    #[schemars(with = "usize", default = "default_scrollback_limit")]
    scrollback_limit: Option<usize>,
}

impl Config {
    /// Encode resolved settings for another sumie process. Unset values stay unset.
    ///
    /// # Panics
    /// Panics only if a settings type stops being serializable, which is a bug.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("settings serialize to JSON")
    }

    /// Decode [`Config::to_json`] output with the same validation as a settings file.
    ///
    /// # Errors
    /// Returns the JSON or validation error.
    pub fn from_json(json: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(json)
    }

    #[must_use]
    pub fn minimum_contrast(&self) -> f32 {
        #[expect(clippy::cast_possible_truncation, reason = "validated ratio in 1..=21")]
        {
            self.minimum_contrast.unwrap_or_else(default_contrast) as f32
        }
    }

    #[must_use]
    pub fn scrollback_limit(&self) -> usize {
        self.scrollback_limit
            .unwrap_or_else(default_scrollback_limit)
    }

    fn overlay(&mut self, file: Self) {
        self.font.overlay(file.font);
        self.colors.overlay(&file.colors);
        self.cursor.overlay(file.cursor);
        self.images.overlay(&file.images);
        self.shell.overlay(&file.shell);
        self.window.overlay(&file.window);
        self.scroll.overlay(&file.scroll);
        self.minimum_contrast = file.minimum_contrast.or(self.minimum_contrast);
        self.scrollback_limit = file.scrollback_limit.or(self.scrollback_limit);
    }
}

const fn default_contrast() -> f64 {
    1.1
}

const fn default_scrollback_limit() -> usize {
    50_000_000
}

fn deserialize_contrast<'de, D: Deserializer<'de>>(input: D) -> Result<Option<f64>, D::Error> {
    let Some(ratio) = Option::<f64>::deserialize(input)? else {
        return Ok(None);
    };
    if (1.0..=21.0).contains(&ratio) {
        Ok(Some(ratio))
    } else {
        Err(de::Error::custom(
            "minimum contrast must be between 1.0 and 21.0",
        ))
    }
}

/// Font selection and rasterization settings.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
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
    pub fn family(&self) -> &str {
        self.family.as_deref().unwrap_or_else(|| default_family())
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
    let Some(size) = Option::<f32>::deserialize(input)? else {
        return Ok(None);
    };
    if size > 0.0 && (size * POINT_TO_PX).is_finite() {
        Ok(Some(size))
    } else {
        Err(de::Error::custom(
            "font size must be a positive finite number of points",
        ))
    }
}

fn deserialize_weight<'de, D: Deserializer<'de>>(input: D) -> Result<Option<u16>, D::Error> {
    let Some(weight) = Option::<u16>::deserialize(input)? else {
        return Ok(None);
    };
    if (MIN_WEIGHT..=MAX_WEIGHT).contains(&weight) {
        Ok(Some(weight))
    } else {
        Err(de::Error::custom(format_args!(
            "font weight must be between {MIN_WEIGHT} and {MAX_WEIGHT}"
        )))
    }
}

fn schema() -> Schema {
    SchemaSettings::default()
        .with(|settings| settings.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<Config>()
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
    write_default(
        &mut text,
        "minimum-contrast",
        &properties["minimum-contrast"],
    );
    write_default(
        &mut text,
        "scrollback-limit",
        &properties["scrollback-limit"],
    );
    for table in [
        "font", "colors", "cursor", "shell", "images", "window", "scroll",
    ] {
        use std::fmt::Write as _;
        writeln!(text, "\n[{table}]").expect("writing to a String is infallible");
        if let Some(fields) = properties[table]["properties"].as_object() {
            for (key, value) in fields {
                write_default(&mut text, key, value);
            }
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
mod tests;
