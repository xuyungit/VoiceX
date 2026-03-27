pub const UI_LANGUAGE_SYSTEM: &str = "system";
pub const LOCALE_ZH_CN: &str = "zh-CN";
pub const LOCALE_EN_US: &str = "en-US";

pub fn normalize_ui_language(value: &str) -> String {
    let trimmed = value.trim();

    if trimmed.eq_ignore_ascii_case(UI_LANGUAGE_SYSTEM) || trimmed.is_empty() {
        return UI_LANGUAGE_SYSTEM.to_string();
    }

    if trimmed.eq_ignore_ascii_case(LOCALE_ZH_CN) || trimmed.eq_ignore_ascii_case("zh") {
        return LOCALE_ZH_CN.to_string();
    }

    if trimmed.eq_ignore_ascii_case(LOCALE_EN_US) || trimmed.eq_ignore_ascii_case("en") {
        return LOCALE_EN_US.to_string();
    }

    UI_LANGUAGE_SYSTEM.to_string()
}

pub fn resolve_ui_locale(preferred: &str) -> String {
    match normalize_ui_language(preferred).as_str() {
        LOCALE_ZH_CN => LOCALE_ZH_CN.to_string(),
        LOCALE_EN_US => LOCALE_EN_US.to_string(),
        _ => resolve_system_locale(),
    }
}

pub fn resolve_system_locale() -> String {
    let locale = sys_locale::get_locale().unwrap_or_else(|| LOCALE_EN_US.to_string());
    if is_chinese_locale(&locale) {
        LOCALE_ZH_CN.to_string()
    } else {
        LOCALE_EN_US.to_string()
    }
}

fn is_chinese_locale(locale: &str) -> bool {
    let normalized = locale.trim().to_ascii_lowercase();
    normalized.starts_with("zh")
}
