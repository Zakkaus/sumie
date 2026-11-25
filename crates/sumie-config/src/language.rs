use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Interface language setting. The binary resolves `Auto` from the locale environment.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
pub enum Language {
    #[default]
    #[serde(rename = "auto")]
    #[schemars(title = "Automatic")]
    Auto,
    #[serde(rename = "en")]
    #[schemars(title = "English")]
    En,
    #[serde(rename = "zh-TW")]
    #[schemars(title = "Traditional Chinese")]
    ZhTw,
    #[serde(rename = "zh-CN")]
    #[schemars(title = "Simplified Chinese")]
    ZhCn,
    #[serde(rename = "ja")]
    #[schemars(title = "Japanese")]
    Ja,
}

#[cfg(test)]
mod tests {
    use crate::{Config, Language};

    #[test]
    fn language_parses_all_tags() {
        for (tag, language) in [
            ("auto", Language::Auto),
            ("en", Language::En),
            ("zh-TW", Language::ZhTw),
            ("zh-CN", Language::ZhCn),
            ("ja", Language::Ja),
        ] {
            let config: Config = toml::from_str(&format!("language = '{tag}'")).unwrap();
            assert_eq!(config.language(), language, "{tag}");
        }
        assert_eq!(Config::default().language(), Language::Auto);
        // Tags are matched exactly; locale spellings belong to the resolver, not the file.
        for tag in ["zh_TW", "zh-tw", "zh", "fr", "en-US"] {
            let error = toml::from_str::<Config>(&format!("language = '{tag}'")).unwrap_err();
            assert!(
                error.message().contains("unknown variant"),
                "{tag}: {error}"
            );
        }
    }
}
