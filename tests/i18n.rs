use ctlyrics::i18n::{Locale, detect_browser_locale};
use std::collections::BTreeSet;

#[test]
fn parses_supported_locale_codes() {
    assert_eq!(Locale::from_code("en_US.UTF-8"), Some(Locale::En));
    assert_eq!(Locale::from_code("zh_CN.UTF-8"), Some(Locale::ZhCn));
}

#[test]
fn detects_browser_language() {
    assert_eq!(
        detect_browser_locale(Some("zh-CN,zh;q=0.9,en;q=0.8")),
        Locale::ZhCn
    );
}

#[test]
fn locale_resources_have_matching_keys() {
    let english: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(include_str!("../locales/en.json")).unwrap();
    let chinese: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(include_str!("../locales/zh-CN.json")).unwrap();
    let english_keys = english.keys().collect::<BTreeSet<_>>();
    let chinese_keys = chinese.keys().collect::<BTreeSet<_>>();
    assert_eq!(english_keys, chinese_keys);
}
