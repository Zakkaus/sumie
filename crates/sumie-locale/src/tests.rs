use std::collections::{BTreeMap, BTreeSet};

use super::{EN, Language, Locale, ZH_TW, choice_key, help_key, label_key};
use sumie_config::{Kind, settings};

#[test]
fn translations_match_source_keys_and_placeholders() {
    let mut english: BTreeMap<String, String> = EN.clone();
    for info in settings() {
        english.insert(label_key(&info), info.label.clone());
        english.insert(help_key(&info), info.description.clone());
        if let Kind::Choice(choices) = &info.kind {
            for choice in choices {
                english.insert(choice_key(&info, choice), choice.label.clone());
            }
        }
    }
    let expected: BTreeSet<_> = ZH_TW.keys().collect();
    assert_eq!(english.keys().collect::<BTreeSet<_>>(), expected);
    for locale in [Locale::ZhTw, Locale::ZhCn, Locale::Ja] {
        let catalog = locale.catalog();
        assert_eq!(
            catalog.keys().collect::<BTreeSet<_>>(),
            expected,
            "{locale:?}"
        );
        for (key, source) in &english {
            let text = &catalog[key];
            assert!(!text.is_empty(), "{locale:?}: {key} is empty");
            for placeholder in source
                .split('{')
                .skip(1)
                .filter_map(|part| part.split_once('}').map(|(name, _)| name))
            {
                assert!(
                    text.contains(&format!("{{{placeholder}}}")),
                    "{locale:?}: {key} missing {{{placeholder}}}"
                );
            }
        }
    }
}

#[test]
fn localized_messages_are_selected() {
    assert_eq!(Locale::ZhCn.get("ui.default"), Some("默认值"));
    assert_eq!(Locale::Ja.get("ui.default"), Some("デフォルト"));
    assert_eq!(Locale::ZhTw.get("ui.default"), Some("預設值"));
    assert_eq!(Locale::ZhTw.get("undefined"), None);
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
