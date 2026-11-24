#![forbid(unsafe_code)]

//! Owns settings definitions, loading, validation, hot reload, TOML write-back,
//! and JSON Schema generation. Does not manage windows or render.

mod changes;
mod cursor;
mod images;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(target_os = "macos", path = "watch_macos.rs")]
mod watch;

pub use changes::Changes;
pub use cursor::{Cursor, CursorStyle};
pub use images::Images;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use watch::Watch;
#[cfg(target_os = "macos")]
pub use watch::WatchEvent;

use std::{
    borrow::Cow,
    env, fs, io,
    path::{Path, PathBuf},
};

use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;
use toml::Spanned;

mod shell;
pub use shell::Shell;

/// Ratio of logical pixels at 96 DPI to typographic points.
pub const POINT_TO_PX: f32 = 96.0 / 72.0;
const MIN_WEIGHT: u16 = 100;
const MAX_WEIGHT: u16 = 900;
const MAX_INCLUDE_DEPTH: usize = 64;

/// Settings for the terminal and its renderer.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Share settings across machines. Paths are relative to the file that lists them.
    /// Later includes override earlier ones; this file overrides all of them per key.
    #[schemars(with = "Vec<PathBuf>")]
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
    /// Padding around each terminal pane.
    pub window: Window,
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
        self.minimum_contrast = file.minimum_contrast.or(self.minimum_contrast);
        self.scrollback_limit = file.scrollback_limit.or(self.scrollback_limit);
    }
}

/// Padding in logical pixels on each side of a pane.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Window {
    /// Left and right padding. Set to 0 to remove it.
    #[schemars(with = "u16", default = "default_padding_x")]
    padding_x: Option<u16>,
    /// Top and bottom padding. Set to 0 to remove it.
    #[schemars(with = "u16", default = "default_padding_y")]
    padding_y: Option<u16>,
}

impl Window {
    #[must_use]
    pub fn padding(&self) -> [u16; 2] {
        [
            self.padding_x.unwrap_or_else(default_padding_x),
            self.padding_y.unwrap_or_else(default_padding_y),
        ]
    }

    fn overlay(&mut self, file: &Self) {
        self.padding_x = file.padding_x.or(self.padding_x);
        self.padding_y = file.padding_y.or(self.padding_y);
    }
}

const fn default_padding_x() -> u16 {
    8
}

const fn default_padding_y() -> u16 {
    6
}

const fn default_contrast() -> f64 {
    1.1
}

const fn default_scrollback_limit() -> usize {
    50_000_000
}

fn deserialize_contrast<'de, D: Deserializer<'de>>(input: D) -> Result<Option<f64>, D::Error> {
    let ratio = f64::deserialize(input)?;
    if (1.0..=21.0).contains(&ratio) {
        Ok(Some(ratio))
    } else {
        Err(de::Error::custom(
            "minimum contrast must be between 1.0 and 21.0",
        ))
    }
}

/// A six-digit sRGB color in `#rrggbb` notation.
#[derive(Clone, Copy, Debug, JsonSchema)]
#[schemars(transparent)]
struct Color(#[schemars(with = "String", regex(pattern = r"^#[0-9a-fA-F]{6}$"))] [u8; 3]);

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        struct HexColor;

        impl de::Visitor<'_> for HexColor {
            type Value = Color;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a color in #rrggbb notation")
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Color, E> {
                let rgb = text
                    .strip_prefix('#')
                    .filter(|hex| hex.len() == 6 && !hex.starts_with('+'))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .ok_or_else(|| E::invalid_value(de::Unexpected::Str(text), &self))?
                    .to_be_bytes();
                Ok(Color([rgb[1], rgb[2], rgb[3]]))
            }
        }

        input.deserialize_str(HexColor)
    }
}

impl Serialize for Color {
    fn serialize<S: serde::Serializer>(&self, output: S) -> Result<S::Ok, S::Error> {
        let [r, g, b] = self.0;
        output.collect_str(&format_args!("#{r:02x}{g:02x}{b:02x}"))
    }
}

/// Terminal colors in `#rrggbb` notation.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Colors {
    /// Default text color.
    #[schemars(with = "Color", default = "default_foreground")]
    foreground: Option<Color>,
    /// Default terminal background color.
    #[schemars(with = "Color", default = "default_background")]
    background: Option<Color>,
    /// Cursor color. Omit to use the foreground color.
    #[schemars(with = "Color", default = "default_foreground")]
    cursor: Option<Color>,
    /// Selected text color.
    #[schemars(with = "Color", default = "default_foreground")]
    selection_foreground: Option<Color>,
    /// Selection background color.
    #[schemars(with = "Color", default = "default_selection_background")]
    selection_background: Option<Color>,
    /// ANSI colors 0 through 15, in index order.
    #[schemars(with = "Vec<Color>", length(equal = 16), default = "default_palette")]
    palette: Option<[Color; 16]>,
}

impl Colors {
    #[must_use]
    pub fn foreground(&self) -> [u8; 3] {
        self.foreground.unwrap_or_else(default_foreground).0
    }

    #[must_use]
    pub fn background(&self) -> [u8; 3] {
        self.background.unwrap_or_else(default_background).0
    }

    #[must_use]
    pub fn cursor(&self) -> Option<[u8; 3]> {
        Some(
            self.cursor
                .map_or_else(|| self.foreground(), |color| color.0),
        )
    }

    #[must_use]
    pub fn selection_foreground(&self) -> Option<[u8; 3]> {
        Some(
            self.selection_foreground
                .unwrap_or_else(default_foreground)
                .0,
        )
    }

    #[must_use]
    pub fn selection_background(&self) -> Option<[u8; 3]> {
        Some(
            self.selection_background
                .unwrap_or_else(default_selection_background)
                .0,
        )
    }

    #[must_use]
    pub fn palette(&self) -> [[u8; 3]; 16] {
        self.palette
            .unwrap_or_else(default_palette)
            .map(|color| color.0)
    }

    fn overlay(&mut self, file: &Self) {
        self.foreground = file.foreground.or(self.foreground);
        self.background = file.background.or(self.background);
        self.cursor = file.cursor.or(self.cursor);
        self.selection_foreground = file.selection_foreground.or(self.selection_foreground);
        self.selection_background = file.selection_background.or(self.selection_background);
        self.palette = file.palette.or(self.palette);
    }
}

const fn default_foreground() -> Color {
    Color([0xff; 3])
}

const fn default_background() -> Color {
    Color([0x12, 0x27, 0x38])
}

const fn default_selection_background() -> Color {
    Color([0x00, 0x50, 0xa4])
}

const fn default_palette() -> [Color; 16] {
    [
        Color([0x00, 0x00, 0x00]),
        Color([0xff, 0x62, 0x8c]),
        Color([0x3a, 0xd9, 0x00]),
        Color([0xff, 0xc6, 0x00]),
        Color([0x00, 0x88, 0xff]),
        Color([0xfb, 0x94, 0xff]),
        Color([0x80, 0xfc, 0xff]),
        Color([0xff; 3]),
        Color([0x00, 0x50, 0xa4]),
        Color([0xff, 0x62, 0x8c]),
        Color([0x3a, 0xd9, 0x00]),
        Color([0xff, 0xc6, 0x00]),
        Color([0x00, 0x88, 0xff]),
        Color([0xfb, 0x94, 0xff]),
        Color([0x80, 0xfc, 0xff]),
        Color([0xff; 3]),
    ]
}

/// Font selection and rasterization settings.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
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

fn load_visited(path: &Path, visit: &mut impl FnMut(&Path)) -> Result<Config, Error> {
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
    for table in ["font", "colors", "cursor", "shell", "images", "window"] {
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
