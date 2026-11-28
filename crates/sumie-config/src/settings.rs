//! Per-key metadata read from the JSON Schema, so serde, Taplo and the settings editor agree.

use std::{fmt::Write as _, sync::LazyLock};

use schemars::{Schema, generate::SchemaSettings};
use serde_json::Value;
use sumie_core::Binding;

use crate::Config;

const CATEGORY: &str = "x-sumie-category";

/// Settings editor tab that lists a key, as the design book's settings section orders them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Appearance,
    Font,
    Colors,
    Behavior,
    Keybinds,
    Links,
    InputMethod,
}

impl Category {
    /// Tabs in display order.
    pub const ALL: [Self; 7] = [
        Self::Appearance,
        Self::Font,
        Self::Colors,
        Self::Behavior,
        Self::Keybinds,
        Self::Links,
        Self::InputMethod,
    ];

    /// Stable identifier used in the schema and in translation keys.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Font => "font",
            Self::Colors => "colors",
            Self::Behavior => "behavior",
            Self::Keybinds => "keybinds",
            Self::Links => "links",
            Self::InputMethod => "input-method",
        }
    }
}

/// One allowed value of an enumerated key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// Spelling in the settings file.
    pub value: String,
    /// English label.
    pub label: String,
}

/// Values a key accepts. The loader remains the authority on validity.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Bool,
    Integer {
        min: Option<i64>,
        max: Option<i64>,
    },
    /// A floating-point setting. Bounds come from the schema; the loader validates writes.
    Number {
        min: Option<f64>,
        max: Option<f64>,
        exclusive_min: bool,
    },
    Text,
    /// A `#rrggbb` color.
    Color,
    /// Sixteen `#rrggbb` colors.
    Palette,
    /// File paths relative to the file that lists them.
    Paths,
    Binding(&'static Binding),
    Choice(Vec<Choice>),
}

/// Metadata for one settings key.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingInfo {
    /// Dotted TOML key, for example `font.size`.
    pub path: String,
    pub kind: Kind,
    /// Effective value when no file sets the key; `None` when unset means platform behavior.
    pub default: Option<Value>,
    /// Another key supplies the runtime default when this setting is unset.
    pub default_from: Option<String>,
    /// English label.
    pub label: String,
    /// English description.
    pub description: String,
    pub category: Category,
}

static SCHEMA: LazyLock<Schema> = LazyLock::new(|| {
    SchemaSettings::default()
        .with(|settings| settings.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<Config>()
});

/// Field names of `table` (`""` for the top level) in declaration order, or `None` if it is
/// not a table.
pub(crate) fn fields(table: &str) -> Option<Vec<&'static str>> {
    let root = &SCHEMA.as_value()["properties"];
    let properties = if table.is_empty() {
        root
    } else {
        &root[table]["properties"]
    };
    Some(properties.as_object()?.keys().map(String::as_str).collect())
}

/// Generate the JSON Schema for the main file and its includes.
///
/// # Errors
/// Returns a JSON serialization error if the generated schema cannot be encoded.
pub fn schema_json() -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&*SCHEMA)
}

/// Every settings key in declaration order, top-level keys first.
///
/// # Panics
/// Panics if a key lacks a title, description or category, which is a bug the tests catch.
#[must_use]
pub fn settings() -> Vec<SettingInfo> {
    let (tables, leaves) = split(&SCHEMA.as_value()["properties"]);
    let mut settings: Vec<_> = leaves
        .map(|(key, leaf)| info(key.clone(), leaf, &Value::Null))
        .collect();
    for (table, schema) in tables {
        let (_, fields) = split(&schema["properties"]);
        settings.extend(fields.map(|(key, leaf)| info(format!("{table}.{key}"), leaf, schema)));
    }
    settings
}

type Entries<'a> = Box<dyn Iterator<Item = (&'a String, &'a Value)> + 'a>;

fn split(properties: &Value) -> (Entries<'_>, Entries<'_>) {
    let entries = properties.as_object().into_iter().flatten();
    let is_table = |(_, schema): &(&String, &Value)| schema.get("properties").is_some();
    (
        Box::new(entries.clone().filter(is_table)),
        Box::new(entries.filter(move |entry| !is_table(entry))),
    )
}

fn info(path: String, leaf: &Value, table: &Value) -> SettingInfo {
    let text = |name: &str| {
        leaf[name]
            .as_str()
            .unwrap_or_else(|| panic!("{path} has no {name}"))
            .to_owned()
    };
    let category = leaf
        .get(CATEGORY)
        .or_else(|| table.get(CATEGORY))
        .and_then(Value::as_str)
        .and_then(|id| {
            Category::ALL
                .into_iter()
                .find(|category| category.id() == id)
        })
        .unwrap_or_else(|| panic!("{path} has no category"));
    let kind = kind(&path, leaf);
    let default = match kind {
        Kind::Binding(binding) => Some(binding.default_on(crate::keybinds::platform()).into()),
        _ => leaf
            .get("default")
            .filter(|value| !value.is_null())
            .cloned(),
    };
    SettingInfo {
        kind,
        default,
        default_from: leaf["x-sumie-default-from"].as_str().map(str::to_owned),
        label: text("title"),
        description: text("description"),
        category,
        path,
    }
}

fn kind(path: &str, leaf: &Value) -> Kind {
    if let Some(binding) = path
        .strip_prefix("keybinds.")
        .and_then(crate::keybinds::binding)
    {
        return Kind::Binding(binding);
    }
    if let Some(choices) = leaf["oneOf"].as_array() {
        return Kind::Choice(
            choices
                .iter()
                .map(|choice| Choice {
                    value: choice["const"]
                        .as_str()
                        .expect("choices are strings")
                        .into(),
                    label: choice["title"]
                        .as_str()
                        .unwrap_or_else(|| panic!("a {path} choice has no title"))
                        .into(),
                })
                .collect(),
        );
    }
    let bound = |name| leaf[name].as_f64();
    match leaf["type"].as_str() {
        Some("boolean") => Kind::Bool,
        Some("integer") => Kind::Integer {
            min: leaf["minimum"].as_i64(),
            max: leaf["maximum"].as_i64(),
        },
        Some("number") => Kind::Number {
            min: bound("minimum").or_else(|| bound("exclusiveMinimum")),
            max: bound("maximum"),
            exclusive_min: leaf.get("exclusiveMinimum").is_some(),
        },
        Some("string") if leaf.get("pattern").is_some() => Kind::Color,
        Some("string") => Kind::Text,
        Some("array") if leaf["items"].get("pattern").is_some() => Kind::Palette,
        Some("array") => Kind::Paths,
        other => panic!("{path} has an unsupported type {other:?}"),
    }
}

/// Generate a parseable example with descriptions and defaults from the schema.
#[must_use]
pub fn defaults_toml() -> String {
    let (tables, leaves) = split(&SCHEMA.as_value()["properties"]);
    let mut text = String::from("#:schema ./sumie.schema.json\n\n");
    for (key, leaf) in leaves {
        write_default(&mut text, key, leaf);
    }
    for (table, schema) in tables {
        writeln!(text, "\n[{table}]").expect("writing to a String is infallible");
        let (_, fields) = split(&schema["properties"]);
        for (key, leaf) in fields {
            write_default(&mut text, key, leaf);
        }
    }
    text
}

fn write_default(text: &mut String, key: &str, schema: &Value) {
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

    #[test]
    fn every_leaf_has_title_and_category() {
        let settings = settings();
        let paths: Vec<_> = settings.iter().map(|info| info.path.as_str()).collect();
        assert_eq!(paths.len(), 24 + sumie_core::BINDINGS.len(), "{paths:?}");
        assert_eq!(
            paths[..9],
            [
                "include",
                "language",
                "minimum-contrast",
                "scrollback-limit",
                "settings-vim-keys",
                "font.family",
                "font.size",
                "font.weight",
                "font.hinting",
            ]
        );
        for info in &settings {
            assert!(
                !info.label.is_empty() && !info.description.is_empty(),
                "{info:?}"
            );
            if let Kind::Choice(choices) = &info.kind {
                assert!(
                    choices.iter().all(|choice| !choice.label.is_empty()),
                    "{info:?}"
                );
            }
        }
        let count = |category| {
            settings
                .iter()
                .filter(|info| info.category == category)
                .count()
        };
        let counts = Category::ALL.map(count);
        assert_eq!(counts, [6, 4, 7, 7, sumie_core::BINDINGS.len(), 0, 0]);
    }

    #[test]
    fn settings_describe_kinds_and_defaults() {
        let settings = settings();
        let find = |path| settings.iter().find(|info| info.path == path).unwrap();
        assert_eq!(
            find("font.weight").kind,
            Kind::Integer {
                min: Some(100),
                max: Some(900)
            }
        );
        assert_eq!(find("font.weight").default, None);
        assert_eq!(find("colors.palette").kind, Kind::Palette);
        assert_eq!(find("colors.cursor").category, Category::Colors);
        assert_eq!(find("minimum-contrast").category, Category::Colors);
        assert_eq!(find("cursor.blink").category, Category::Appearance);
        assert_eq!(find("window.padding-x").category, Category::Appearance);
        assert_eq!(find("scroll.smooth").category, Category::Behavior);
        assert_eq!(find("language").category, Category::Behavior);
        let Kind::Choice(languages) = &find("language").kind else {
            panic!("language is not a choice");
        };
        let values: Vec<_> = languages
            .iter()
            .map(|choice| choice.value.as_str())
            .collect();
        assert_eq!(values, ["auto", "en", "zh-TW", "zh-CN", "ja"]);
        assert_eq!(find("language").default, Some("auto".into()));
    }
}
