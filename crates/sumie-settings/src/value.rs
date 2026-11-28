use std::borrow::Cow;

use sumie_config::{Kind, SettingInfo, Value, format_list, parse_list};
use sumie_core::{Chord, Platform};
use sumie_locale::Locale;

pub(crate) struct OptionInfo {
    pub(crate) info: SettingInfo,
    pub(crate) default: Option<Value>,
}

impl OptionInfo {
    pub(crate) fn new(info: SettingInfo) -> Self {
        let default = info
            .default
            .as_ref()
            .map(|value| Value::try_from(value).expect("settings defaults are TOML values"))
            .or_else(|| matches!(info.kind, Kind::Paths).then(|| Value::Array(Vec::new())));
        Self { info, default }
    }

    pub(crate) fn label(&self, locale: Locale) -> &str {
        locale.setting_label(&self.info)
    }

    pub(crate) fn help(&self, locale: Locale) -> &str {
        locale.setting_help(&self.info)
    }

    pub(crate) fn display<'a>(&'a self, locale: Locale, value: Option<&'a Value>) -> Cow<'a, str> {
        match (&self.info.kind, value) {
            (Kind::Choice(choices), Some(Value::String(value))) => choices
                .iter()
                .find(|choice| choice.value == *value)
                .map_or_else(
                    || locale.text("ui.invalid-value"),
                    |choice| locale.choice_label(&self.info, choice),
                )
                .into(),
            (Kind::Bool, Some(Value::Boolean(value))) => {
                locale.text(if *value { "ui.on" } else { "ui.off" }).into()
            }
            (Kind::Text | Kind::Color, Some(Value::String(text))) => text.as_str().into(),
            (Kind::Binding(_), Some(Value::String(text))) => Chord::parse(text).map_or_else(
                || locale.text("ui.invalid-value").into(),
                |chord| chord.display(Platform::current()).into(),
            ),
            (Kind::Integer { .. } | Kind::Number { .. }, Some(Value::Integer(value))) => {
                value.to_string().into()
            }
            (Kind::Number { .. }, Some(Value::Float(value))) => value.to_string().into(),
            (Kind::Paths, Some(Value::Array(items))) if items.is_empty() => {
                locale.text("ui.empty-list").into()
            }
            (Kind::Palette | Kind::Paths, Some(value @ Value::Array(_))) => {
                format_list(value).into()
            }
            (_, None) => locale.text("ui.automatic").into(),
            _ => locale.text("ui.invalid-value").into(),
        }
    }

    pub(crate) fn allowed(&self, locale: Locale) -> Cow<'_, str> {
        match &self.info.kind {
            Kind::Bool => format!("{} / {}", locale.text("ui.off"), locale.text("ui.on")).into(),
            Kind::Choice(choices) => choices
                .iter()
                .map(|choice| locale.choice_label(&self.info, choice))
                .collect::<Vec<_>>()
                .join(" / ")
                .into(),
            Kind::Text => locale.text("kind.text").into(),
            Kind::Color => locale.text("kind.color").into(),
            Kind::Palette => locale.text("kind.palette").into(),
            Kind::Paths => locale.text("kind.paths").into(),
            Kind::Binding(_) => locale.text("kind.binding").into(),
            Kind::Integer { min, max } => bounds(locale.text("kind.integer"), *min, *max, false),
            Kind::Number {
                min,
                max,
                exclusive_min,
            } => bounds(locale.text("kind.number"), *min, *max, *exclusive_min),
        }
    }

    pub(crate) fn edit_text(&self, value: Option<&Value>) -> Option<String> {
        match self.info.kind {
            Kind::Bool | Kind::Choice(_) | Kind::Binding(_) => None,
            Kind::Paths | Kind::Palette => Some(value.map_or_else(|| "[]".into(), format_list)),
            _ => Some(match value {
                Some(Value::Integer(value)) => value.to_string(),
                Some(Value::Float(value)) => value.to_string(),
                Some(Value::String(text)) => text.clone(),
                _ => String::new(),
            }),
        }
    }

    pub(crate) fn parse(&self, input: &str) -> Option<Value> {
        match self.info.kind {
            Kind::Integer { min, max } => input
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|n| within(*n, min, max, false))
                .map(Value::Integer),
            Kind::Number {
                min,
                max,
                exclusive_min,
            } => input
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && within(*n, min, max, exclusive_min))
                .map(Value::Float),
            Kind::Text | Kind::Color => Some(Value::String(input.to_owned())),
            Kind::Paths | Kind::Palette => parse_list(input),
            Kind::Bool | Kind::Choice(_) | Kind::Binding(_) => None,
        }
    }

    pub(crate) fn step(&self, value: Option<&Value>, right: bool) -> Option<Value> {
        let step = if right { 1_i32 } else { -1_i32 };
        match &self.info.kind {
            Kind::Bool => Some(Value::Boolean(
                !value.and_then(Value::as_bool).unwrap_or(!right),
            )),
            Kind::Choice(choices) => {
                let current = value
                    .and_then(Value::as_str)
                    .and_then(|tag| choices.iter().position(|choice| choice.value == tag));
                let len = choices.len();
                let index = if right {
                    current.map_or(0, |i| (i + 1) % len)
                } else {
                    current.map_or(len - 1, |i| (i + len - 1) % len)
                };
                Some(Value::String(choices[index].value.clone()))
            }
            Kind::Integer { min, max } => {
                let start = if right {
                    min.unwrap_or(0)
                } else {
                    max.unwrap_or(0)
                };
                let next = value
                    .and_then(Value::as_integer)
                    .map_or(start, |n| n.saturating_add(i64::from(step)));
                Some(Value::Integer(
                    next.clamp(min.unwrap_or(i64::MIN), max.unwrap_or(i64::MAX)),
                ))
            }
            Kind::Number {
                min,
                max,
                exclusive_min,
            } => {
                let lower = min.map_or(f64::MIN, |n| if *exclusive_min { n + 0.1 } else { n });
                let upper = max.unwrap_or(f64::MAX);
                let current = value.and_then(as_number).unwrap_or(0.0);
                let next = ((current * 10.0).round() + f64::from(step)) / 10.0;
                Some(Value::Float(next.clamp(lower, upper)))
            }
            _ => None,
        }
    }
}

fn within<T: Copy + PartialOrd>(
    value: T,
    min: Option<T>,
    max: Option<T>,
    exclusive_min: bool,
) -> bool {
    min.is_none_or(|min| {
        if exclusive_min {
            value > min
        } else {
            value >= min
        }
    }) && max.is_none_or(|max| value <= max)
}

pub(crate) fn same(left: Option<&Value>, right: Option<&Value>) -> bool {
    left == right
        || matches!((left, right),
            (Some(a @ Value::Float(_)), Some(b @ Value::Integer(_)))
            | (Some(a @ Value::Integer(_)), Some(b @ Value::Float(_)))
            if as_number(a).zip(as_number(b)).is_some_and(|(a, b)| a.total_cmp(&b).is_eq()))
}

fn as_number(value: &Value) -> Option<f64> {
    #[expect(
        clippy::cast_precision_loss,
        reason = "TOML integer converted only for floating settings or comparison"
    )]
    value
        .as_float()
        .or_else(|| value.as_integer().map(|value| value as f64))
}

fn bounds<T: std::fmt::Display>(
    label: &'static str,
    min: Option<T>,
    max: Option<T>,
    exclusive: bool,
) -> Cow<'static, str> {
    match (min, max) {
        (Some(min), Some(max)) if !exclusive => format!("{label} · {min}–{max}").into(),
        (Some(min), _) => format!("{label} · {} {min}", if exclusive { ">" } else { "≥" }).into(),
        (None, Some(max)) => format!("{label} · ≤ {max}").into(),
        (None, None) => label.into(),
    }
}
