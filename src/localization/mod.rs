mod dutch;
mod english;
mod french;
mod german;
mod japanese;
mod korean;
mod portuguese_brazil;
mod russian;
mod spanish;
mod traditional_chinese;

use windows::core::PWSTR;
use windows::Win32::Globalization::{
    GetUserDefaultLocaleName, GetUserDefaultUILanguage, GetUserPreferredUILanguages,
    LCIDToLocaleName, LOCALE_ALLOW_NEUTRAL_NAMES, MAX_LOCALE_NAME, MUI_LANGUAGE_NAME,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanguageId {
    English,
    Dutch,
    Spanish,
    French,
    German,
    Japanese,
    Korean,
    TraditionalChinese,
    Russian,
    PortugueseBrazil,
}

impl LanguageId {
    pub const ALL: [LanguageId; 10] = [
        LanguageId::English,
        LanguageId::Dutch,
        LanguageId::Spanish,
        LanguageId::French,
        LanguageId::German,
        LanguageId::Japanese,
        LanguageId::Korean,
        LanguageId::TraditionalChinese,
        LanguageId::Russian,
        LanguageId::PortugueseBrazil,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Dutch => "nl",
            Self::Spanish => "es",
            Self::French => "fr",
            Self::German => "de",
            Self::Japanese => "ja",
            Self::Korean => "ko",
            Self::TraditionalChinese => "zh-TW",
            Self::Russian => "ru",
            Self::PortugueseBrazil => "pt-BR",
        }
    }

    pub fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Dutch => "Nederlands",
            Self::Spanish => "Español",
            Self::French => "Français",
            Self::German => "Deutsch",
            Self::Japanese => "日本語",
            Self::Korean => "한국어",
            Self::TraditionalChinese => "繁體中文",
            Self::Russian => "Русский",
            Self::PortugueseBrazil => "Português (Brasil)",
        }
    }

    pub fn strings(self) -> Strings {
        match self {
            Self::English => english::STRINGS,
            Self::Dutch => dutch::STRINGS,
            Self::Spanish => spanish::STRINGS,
            Self::French => french::STRINGS,
            Self::German => german::STRINGS,
            Self::Japanese => japanese::STRINGS,
            Self::Korean => korean::STRINGS,
            Self::TraditionalChinese => traditional_chinese::STRINGS,
            Self::Russian => russian::STRINGS,
            Self::PortugueseBrazil => portuguese_brazil::STRINGS,
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        let normalized = code.trim().replace('_', "-").to_ascii_lowercase();
        if normalized.is_empty() || normalized == "system" {
            return None;
        }

        let prefix = normalized.split('-').next().unwrap_or_default();
        match prefix {
            "en" => Some(Self::English),
            "nl" => Some(Self::Dutch),
            "es" => Some(Self::Spanish),
            "fr" => Some(Self::French),
            "de" => Some(Self::German),
            "ja" => Some(Self::Japanese),
            "ko" => Some(Self::Korean),
            "zh" => {
                if normalized.contains("tw")
                    || normalized.contains("hk")
                    || normalized.contains("hant")
                {
                    Some(Self::TraditionalChinese)
                } else {
                    None
                }
            }
            "ru" => Some(Self::Russian),
            "pt" => Some(Self::PortugueseBrazil),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Strings {
    pub window_title: &'static str,
    pub refresh: &'static str,
    pub update_frequency: &'static str,
    pub one_minute: &'static str,
    pub five_minutes: &'static str,
    pub fifteen_minutes: &'static str,
    pub one_hour: &'static str,
    pub models: &'static str,
    pub claude_code_model: &'static str,
    pub codex_model: &'static str,
    pub antigravity_model: &'static str,
    pub settings: &'static str,
    pub start_with_windows: &'static str,
    pub reset_position: &'static str,
    pub language: &'static str,
    pub system_default: &'static str,
    pub monitor: &'static str,
    pub about: &'static str,
    pub about_message: &'static str,
    pub session_limit_line: &'static str,
    pub weekly_limit_line: &'static str,
    pub day_word_singular: &'static str,
    pub day_word_plural: &'static str,
    pub exit: &'static str,
    pub show_widget: &'static str,
    pub session_window: &'static str,
    pub weekly_window: &'static str,
    pub now: &'static str,
    pub day_suffix: &'static str,
    pub hour_suffix: &'static str,
    pub minute_suffix: &'static str,
    pub second_suffix: &'static str,
    pub token_expired_title: &'static str,
    pub token_expired_body: &'static str,
    pub codex_token_expired_title: &'static str,
    pub codex_token_expired_body: &'static str,
    pub antigravity_token_expired_title: &'static str,
    pub antigravity_token_expired_body: &'static str,
    pub no_credentials_title: &'static str,
    pub no_credentials_body: &'static str,
    pub codex_no_credentials_title: &'static str,
    pub codex_no_credentials_body: &'static str,
    pub antigravity_no_credentials_title: &'static str,
    pub antigravity_no_credentials_body: &'static str,
    pub codex_window_title: &'static str,
    pub antigravity_window_title: &'static str,
}

pub fn resolve_language(language_override: Option<LanguageId>) -> LanguageId {
    language_override.unwrap_or_else(detect_system_language)
}

pub fn detect_system_language() -> LanguageId {
    preferred_ui_languages()
        .into_iter()
        .find_map(|locale| LanguageId::from_code(&locale))
        .or_else(default_ui_locale)
        .or_else(default_locale_name)
        .unwrap_or(LanguageId::English)
}

fn preferred_ui_languages() -> Vec<String> {
    unsafe {
        let mut num_languages = 0u32;
        let mut buffer_len = 0u32;
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut num_languages,
            PWSTR::null(),
            &mut buffer_len,
        )
        .is_err()
            || buffer_len == 0
        {
            return Vec::new();
        }

        let mut buffer = vec![0u16; buffer_len as usize];
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut num_languages,
            PWSTR(buffer.as_mut_ptr()),
            &mut buffer_len,
        )
        .is_err()
        {
            return Vec::new();
        }

        buffer
            .split(|unit| *unit == 0)
            .filter(|part| !part.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }
}

fn default_ui_locale() -> Option<LanguageId> {
    unsafe {
        let lang_id = GetUserDefaultUILanguage();
        let mut buffer = [0u16; MAX_LOCALE_NAME as usize];
        let len = LCIDToLocaleName(
            lang_id as u32,
            Some(&mut buffer),
            LOCALE_ALLOW_NEUTRAL_NAMES,
        );
        if len <= 1 {
            return None;
        }
        let locale = String::from_utf16_lossy(&buffer[..(len as usize - 1)]);
        LanguageId::from_code(&locale)
    }
}

fn default_locale_name() -> Option<LanguageId> {
    unsafe {
        let mut buffer = [0u16; MAX_LOCALE_NAME as usize];
        let len = GetUserDefaultLocaleName(&mut buffer);
        if len <= 1 {
            return None;
        }
        let locale = String::from_utf16_lossy(&buffer[..(len as usize - 1)]);
        LanguageId::from_code(&locale)
    }
}
