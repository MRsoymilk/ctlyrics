use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    ZhCn,
}

impl Locale {
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
        }
    }

    pub fn from_code(value: &str) -> Option<Self> {
        let normalized = value.trim().replace('_', "-").to_ascii_lowercase();
        if normalized == "en" || normalized.starts_with("en-") {
            Some(Self::En)
        } else if normalized == "zh" || normalized.starts_with("zh-") {
            Some(Self::ZhCn)
        } else {
            None
        }
    }
}

static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
static ZH_CN: OnceLock<HashMap<String, String>> = OnceLock::new();

fn catalog(locale: Locale) -> &'static HashMap<String, String> {
    match locale {
        Locale::En => EN.get_or_init(|| parse_catalog(include_str!("../locales/en.json"))),
        Locale::ZhCn => ZH_CN.get_or_init(|| parse_catalog(include_str!("../locales/zh-CN.json"))),
    }
}

fn parse_catalog(content: &str) -> HashMap<String, String> {
    serde_json::from_str(content).expect("invalid embedded locale resource")
}

pub fn tr(locale: Locale, key: &'static str) -> &'static str {
    catalog(locale)
        .get(key)
        .or_else(|| catalog(Locale::En).get(key))
        .map(String::as_str)
        .unwrap_or(key)
}

pub fn format(locale: Locale, key: &'static str, values: &[(&str, &str)]) -> String {
    let mut message = tr(locale, key).to_string();
    for (name, value) in values {
        message = message.replace(&format!("{{{}}}", name), value);
    }
    message
}

pub fn detect_system_locale() -> Locale {
    std::env::var("LC_ALL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("LC_MESSAGES")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .or_else(|| std::env::var("LANG").ok())
        .as_deref()
        .and_then(Locale::from_code)
        .unwrap_or(Locale::En)
}

pub fn detect_browser_locale(accept_language: Option<&str>) -> Locale {
    accept_language
        .into_iter()
        .flat_map(|header| header.split(','))
        .filter_map(|entry| Locale::from_code(entry.split(';').next().unwrap_or("")))
        .next()
        .unwrap_or(Locale::En)
}

pub fn preferred_locale() -> Locale {
    fs::read_to_string(language_path())
        .ok()
        .as_deref()
        .and_then(Locale::from_code)
        .unwrap_or_else(detect_system_locale)
}

pub fn set_preference(value: &str) -> std::io::Result<Locale> {
    if value.eq_ignore_ascii_case("auto") {
        match fs::remove_file(language_path()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        return Ok(detect_system_locale());
    }

    let locale = Locale::from_code(value).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsupported language preference",
        )
    })?;
    fs::create_dir_all("config")?;
    fs::write(language_path(), locale.code())?;
    Ok(locale)
}

fn language_path() -> &'static Path {
    Path::new("config/language")
}

#[cfg(test)]
mod tests {
    use super::{Locale, catalog, detect_browser_locale};

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
        let mut english: Vec<_> = catalog(Locale::En).keys().collect();
        let mut chinese: Vec<_> = catalog(Locale::ZhCn).keys().collect();
        english.sort();
        chinese.sort();
        assert_eq!(english, chinese);
    }
}
