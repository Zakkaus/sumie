#![forbid(unsafe_code)]

//! Select and look up interface text. Never reads settings files or renders text.

use std::{collections::BTreeMap, sync::LazyLock};

use sumie_config::{Choice, Kind, Language, SettingInfo};

type Catalog = BTreeMap<String, String>;
static EN: LazyLock<Catalog> = LazyLock::new(|| catalog(include_str!("../locales/en.toml")));
static ZH_TW: LazyLock<Catalog> = LazyLock::new(|| catalog(include_str!("../locales/zh-TW.toml")));
static ZH_CN: LazyLock<Catalog> = LazyLock::new(|| catalog(include_str!("../locales/zh-CN.toml")));
static JA: LazyLock<Catalog> = LazyLock::new(|| catalog(include_str!("../locales/ja.toml")));

fn catalog(source: &str) -> Catalog {
    toml::from_str(source).expect("embedded locale catalog must be valid TOML")
}

/// Locales supported by the interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    En,
    ZhTw,
    ZhCn,
    Ja,
}

impl Locale {
    /// Resolve `Auto` once from the first nonempty locale variable.
    #[must_use]
    pub fn from_language(language: Language) -> Self {
        Self::resolve(language, |name| std::env::var(name).ok())
    }

    fn resolve(language: Language, mut env: impl FnMut(&str) -> Option<String>) -> Self {
        match language {
            Language::En => Self::En,
            Language::ZhTw => Self::ZhTw,
            Language::ZhCn => Self::ZhCn,
            Language::Ja => Self::Ja,
            Language::Auto => ["LC_ALL", "LC_MESSAGES", "LANG"]
                .into_iter()
                .find_map(|name| env(name).filter(|locale| !locale.trim().is_empty()))
                .map_or(Self::En, |locale| Self::from_tag(&locale)),
        }
    }

    fn from_tag(tag: &str) -> Self {
        let mut parts = tag
            .trim()
            .split(['.', '@'])
            .next()
            .unwrap_or_default()
            .split(['-', '_']);
        match parts
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "ja" => Self::Ja,
            "zh" if parts.any(|part| {
                ["TW", "HK", "MO", "Hant"]
                    .iter()
                    .any(|suffix| part.eq_ignore_ascii_case(suffix))
            }) =>
            {
                Self::ZhTw
            }
            "zh" => Self::ZhCn,
            _ => Self::En,
        }
    }

    fn catalog(self) -> &'static Catalog {
        match self {
            Self::En => &EN,
            Self::ZhTw => &ZH_TW,
            Self::ZhCn => &ZH_CN,
            Self::Ja => &JA,
        }
    }

    /// Look up a message, using English when the selected catalog lacks the key.
    /// `None` means the key does not exist in either catalog.
    #[must_use]
    pub fn get(self, key: &str) -> Option<&'static str> {
        self.catalog()
            .get(key)
            .or_else(|| EN.get(key))
            .map(String::as_str)
    }

    /// A setting's label, or its English schema title when the catalog has no translation.
    #[must_use]
    pub fn setting_label(self, info: &SettingInfo) -> &str {
        self.get(&label_key(info)).unwrap_or(&info.label)
    }

    /// A setting's help, or its English schema description.
    #[must_use]
    pub fn setting_help(self, info: &SettingInfo) -> &str {
        self.get(&help_key(info)).unwrap_or(&info.description)
    }

    /// A choice's label, or its English schema title.
    #[must_use]
    pub fn choice_label<'a>(self, info: &SettingInfo, choice: &'a Choice) -> &'a str {
        self.get(&choice_key(info, choice)).unwrap_or(&choice.label)
    }

    /// Look up a required message. The key is never used as display text.
    ///
    /// # Panics
    /// Panics if the message is missing from the source catalog and the selected catalog.
    #[must_use]
    pub fn text(self, key: &str) -> &'static str {
        self.get(key)
            .unwrap_or_else(|| panic!("missing source message: {key}"))
    }
}

fn label_key(info: &SettingInfo) -> String {
    format!("setting.{}.label", info.path)
}

fn help_key(info: &SettingInfo) -> String {
    if matches!(info.kind, Kind::Binding(_)) {
        "setting.keybinds.help".into()
    } else {
        format!("setting.{}.help", info.path)
    }
}

fn choice_key(info: &SettingInfo, choice: &Choice) -> String {
    format!("choice.{}.{}", info.path, choice.value)
}

#[cfg(test)]
mod tests;
