use super::{EN, Language, Locale};
use sumie_config::{Kind, settings};

#[test]
fn reports_missing_keys_by_locale_and_rejects_unknown_keys() {
    for (tag, locale) in [
        ("en", Locale::En),
        ("zh-TW", Locale::ZhTw),
        ("zh-CN", Locale::ZhCn),
        ("ja", Locale::Ja),
    ] {
        let catalog = locale.catalog();
        let missing: Vec<_> = EN
            .keys()
            .filter(|key| !catalog.is_some_and(|catalog| catalog.contains_key(*key)))
            .collect();
        println!("{tag}: missing {}: {missing:?}", missing.len());
        if let Some(catalog) = catalog {
            let unknown: Vec<_> = catalog
                .keys()
                .filter(|key| !EN.contains_key(*key))
                .collect();
            assert!(unknown.is_empty(), "{tag}: unknown keys: {unknown:?}");
            assert!(
                catalog.values().all(|text| !text.is_empty()),
                "{tag}: empty message"
            );
        }
    }
}

#[test]
fn missing_translation_uses_english() {
    assert_eq!(Locale::ZhCn.get("ui.default"), Some("Default"));
    assert_eq!(Locale::Ja.get("ui.default"), Some("Default"));
    assert_eq!(Locale::ZhTw.get("ui.default"), Some("預設值"));
    assert_eq!(Locale::ZhTw.get("undefined"), None);
}

#[test]
fn english_settings_text_matches_schema() {
    for info in settings() {
        assert_eq!(
            Locale::En.text(&format!("setting.{}.label", info.path)),
            info.label
        );
        assert_eq!(
            Locale::En.text(&format!("setting.{}.help", info.path)),
            info.description
        );
        if let Kind::Choice(choices) = info.kind {
            for choice in choices {
                assert_eq!(
                    Locale::En.text(&format!("choice.{}.{}", info.path, choice.value)),
                    choice.label
                );
            }
        }
    }
}

#[test]
fn language_overrides_environment_and_auto_uses_locale_precedence() {
    let environment = |name: &str| match name {
        "LC_ALL" => Some(String::new()),
        "LC_MESSAGES" => Some("zh_HK.UTF-8".to_owned()),
        "LANG" => Some("ja_JP.UTF-8".to_owned()),
        _ => None,
    };
    assert_eq!(
        Locale::resolve(Language::En, |_| panic!(
            "explicit language read environment"
        )),
        Locale::En
    );
    assert_eq!(Locale::resolve(Language::Auto, environment), Locale::ZhTw);
    assert_eq!(
        Locale::resolve(Language::Auto, |name| (name == "LANG")
            .then(|| "ja_JP.UTF-8".into())),
        Locale::Ja
    );
    assert_eq!(
        Locale::resolve(Language::Auto, |name| match name {
            "LC_ALL" => Some("C.UTF-8".into()),
            "LC_MESSAGES" => Some("zh_TW".into()),
            _ => None,
        }),
        Locale::En
    );
    assert_eq!(Locale::from_tag("zh-Hant"), Locale::ZhTw);
    assert_eq!(Locale::from_tag("zh_CN.GB18030"), Locale::ZhCn);
}
