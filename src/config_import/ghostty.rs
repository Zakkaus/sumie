//! Read the lines of a Ghostty config. A line becomes a sumie setting or nothing; the value is
//! checked again when it is written.

use sumie_config::Value;

use super::keybinds;

pub(super) enum Target {
    /// A dotted settings key such as `font.size`.
    Key(String),
    /// One of the sixteen `colors.palette` entries.
    Palette(usize),
}

pub(super) struct Setting {
    pub line: usize,
    pub key: String,
    pub value: Option<(Target, Value)>,
}

pub(super) fn parse(text: &str) -> Vec<Setting> {
    text.lines()
        .enumerate()
        .filter_map(|(index, raw)| {
            let raw = raw.trim();
            if raw.is_empty() || raw.starts_with('#') {
                return None;
            }
            let (key, value) = raw.split_once('=').unwrap_or((raw, ""));
            let (key, value) = (key.trim(), value.trim());
            let value = value
                .strip_prefix('"')
                .and_then(|body| body.strip_suffix('"'))
                .unwrap_or(value);
            Some(Setting {
                line: index + 1,
                key: key.to_owned(),
                value: translate(key, value),
            })
        })
        .collect()
}

fn translate(key: &str, value: &str) -> Option<(Target, Value)> {
    let target = match key {
        "font-family" if !value.is_empty() => "font.family",
        "font-size" => "font.size",
        "background" => "colors.background",
        "foreground" => "colors.foreground",
        "cursor-color" => "colors.cursor",
        "selection-background" => "colors.selection-background",
        "selection-foreground" => "colors.selection-foreground",
        "palette" => {
            let (index, color) = value.split_once('=')?;
            let index = index.trim().parse().ok().filter(|&index| index < 16)?;
            return Some((Target::Palette(index), color_value(color.trim())?));
        }
        "keybind" => return keybinds::translate(value),
        _ => return None,
    };
    let value = match target {
        "font.family" => Value::String(value.to_owned()),
        "font.size" => Value::Float(value.parse().ok()?),
        _ => color_value(value)?,
    };
    Some((Target::Key(target.to_owned()), value))
}

/// Only `#rrggbb` and `rrggbb`; sumie writes the first form.
fn color_value(text: &str) -> Option<Value> {
    let digits = text.strip_prefix('#').unwrap_or(text);
    (digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| Value::String(format!("#{}", digits.to_ascii_lowercase())))
}
