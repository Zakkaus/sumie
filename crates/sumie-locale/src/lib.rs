#![forbid(unsafe_code)]

//! Select and look up interface text. Never reads settings files or renders text.

use std::{collections::BTreeMap, sync::LazyLock};

use sumie_config::Language;

type Catalog = BTreeMap<String, String>;
static EN: LazyLock<Catalog> = LazyLock::new(|| catalog(include_str!("../locales/en.toml")));
static ZH_TW: LazyLock<Catalog> = LazyLock::new(|| catalog(include_str!("../locales/zh-TW.toml")));

fn catalog(source: &str) -> Catalog {
    toml::from_str(source).expect("embedded locale catalog must be valid TOML")
}

/// Locales supported by the interface, including those still using English text.
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

    fn catalog(self) -> Option<&'static Catalog> {
        match self {
            Self::En => Some(&EN),
            Self::ZhTw => Some(&ZH_TW),
            Self::ZhCn | Self::Ja => None,
        }
    }

    /// Look up a message, using English when the selected catalog lacks the key.
    /// `None` means the key does not exist in either catalog.
    #[must_use]
    pub fn get(self, key: &str) -> Option<&'static str> {
        self.catalog()
            .and_then(|catalog| catalog.get(key))
            .or_else(|| EN.get(key))
            .map(String::as_str)
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

#[cfg(test)]
mod tests;
