//! Lightweight localization registry for high-visibility UI strings.
//!
//! This intentionally covers UI chrome only. It does not change model prompts,
//! model output language, provider behavior, or media payload semantics.
//!
//! Known limitation: since the ratatui TUI was retired (0.9.0) the catalog
//! holds only the `/settings` header and footer labels; REPL command help
//! and key tables are English-only and live beside their commands.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDirection {
    Ltr,
    Rtl,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocaleCoverage {
    English,
    Shipped,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocaleSpec {
    pub tag: &'static str,
    pub display_name: &'static str,
    pub script: &'static str,
    pub direction: TextDirection,
    pub fallback: &'static str,
    pub coverage: LocaleCoverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    En,
    ZhHans,
    ZhHant,
    Hi,
    Es419,
}

impl Locale {
    pub fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhHans => "zh-Hans",
            Self::ZhHant => "zh-Hant",
            Self::Hi => "hi",
            Self::Es419 => "es-419",
        }
    }

    pub fn spec(self) -> LocaleSpec {
        match self {
            Self::En => LocaleSpec {
                tag: "en",
                display_name: "English",
                script: "Latin",
                direction: TextDirection::Ltr,
                fallback: "en",
                coverage: LocaleCoverage::English,
            },
            Self::ZhHans => LocaleSpec {
                tag: "zh-Hans",
                display_name: "Chinese Simplified",
                script: "Hans",
                direction: TextDirection::Ltr,
                fallback: "en",
                coverage: LocaleCoverage::Shipped,
            },
            Self::ZhHant => LocaleSpec {
                tag: "zh-Hant",
                display_name: "Chinese Traditional",
                script: "Hant",
                direction: TextDirection::Ltr,
                fallback: "zh-Hans",
                coverage: LocaleCoverage::Shipped,
            },
            Self::Hi => LocaleSpec {
                tag: "hi",
                display_name: "Hindi",
                script: "Deva",
                direction: TextDirection::Ltr,
                fallback: "en",
                coverage: LocaleCoverage::Shipped,
            },
            Self::Es419 => LocaleSpec {
                tag: "es-419",
                display_name: "Spanish (Latin America)",
                script: "Latin",
                direction: TextDirection::Ltr,
                fallback: "en",
                coverage: LocaleCoverage::Shipped,
            },
        }
    }

    pub fn shipped() -> &'static [Self] {
        &[Self::En, Self::ZhHans, Self::ZhHant, Self::Hi, Self::Es419]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageId {
    SettingsTitle,
    SettingsConfigFile,
}

#[allow(dead_code)]
pub const ALL_MESSAGE_IDS: &[MessageId] =
    &[MessageId::SettingsTitle, MessageId::SettingsConfigFile];

pub fn tr(locale: Locale, id: MessageId) -> &'static str {
    // Follow the declarative `LocaleSpec::fallback` chain (zh-Hant →
    // zh-Hans) before English, so a future locale whose spec declares a
    // non-English fallback is honored automatically; the walk is bounded
    // so a cyclic spec cannot loop.
    let mut current = locale;
    let mut candidate = translation(current, id);
    for _ in 0..Locale::shipped().len() {
        if candidate.is_some() {
            break;
        }
        match locale_from_tag(current.spec().fallback) {
            Some(next) => current = next,
            None => break,
        }
        candidate = translation(current, id);
    }
    fallback_translation(candidate, id)
}

fn locale_from_tag(tag: &str) -> Option<Locale> {
    Locale::shipped()
        .iter()
        .copied()
        .find(|locale| locale.tag() == tag)
}

#[allow(dead_code)]
pub fn missing_message_ids(locale: Locale) -> Vec<MessageId> {
    ALL_MESSAGE_IDS
        .iter()
        .copied()
        .filter(|id| translation(locale, *id).is_none())
        .collect()
}

pub fn normalize_configured_locale(input: &str) -> Option<&'static str> {
    let normalized = normalize_locale_input(input);
    if matches!(normalized.as_str(), "" | "auto" | "system") {
        return Some("auto");
    }
    parse_locale(&normalized).map(Locale::tag)
}

pub fn resolve_locale(setting: &str) -> Locale {
    resolve_locale_with_env(setting, |key| std::env::var(key).ok())
}

pub fn resolve_locale_with_env<F>(setting: &str, env: F) -> Locale
where
    F: Fn(&str) -> Option<String>,
{
    let normalized = normalize_locale_input(setting);
    if !matches!(normalized.as_str(), "" | "auto" | "system")
        && let Some(locale) = parse_locale(&normalized)
    {
        return locale;
    }
    // An unrecognized setting (typo like "chinese" or "zh:CN") must not
    // silently drop the environment locale: fall through to env detection
    // before defaulting to English.

    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(value) = env(key)
            && let Some(locale) = parse_locale(&normalize_locale_input(&value))
        {
            return locale;
        }
    }

    Locale::En
}

/// Truncate to a terminal-column budget, appending `…` when it cuts. Wide
/// characters count as the two columns they occupy, so callers that promise
/// "one row" keep that promise for CJK text too.
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text.width() <= max_width {
        return text.to_string();
    }

    // U+2026 (…) is East-Asian Ambiguous: `width()` always reports 1,
    // but CJK-configured terminals render it in two columns — exactly
    // the locales this helper targets. Reserve the wider rendering so
    // the column budget holds everywhere.
    let ellipsis_width = 2;
    if max_width <= ellipsis_width {
        return "…".to_string();
    }

    let limit = max_width - ellipsis_width;
    let mut out = String::new();
    let mut width = 0usize;
    // Whole grapheme clusters only: cutting after a base character's
    // zero-width joiner or combining mark (emoji ZWJ sequences, Indic
    // matras) leaves a broken glyph in front of the ellipsis. A cluster
    // wider than the remaining budget is dropped whole, never split.
    for cluster in text.graphemes(true) {
        let cluster_width: usize = cluster.chars().map(|ch| ch.width().unwrap_or(0)).sum();
        if width + cluster_width > limit {
            break;
        }
        out.push_str(cluster);
        width += cluster_width;
    }
    out.push('…');
    out
}

fn normalize_locale_input(input: &str) -> String {
    input
        .split('.')
        .next()
        .unwrap_or(input)
        .split('@')
        .next()
        .unwrap_or(input)
        .trim()
        .replace('_', "-")
        .to_lowercase()
}

fn parse_locale(value: &str) -> Option<Locale> {
    // Match whole `-`-separated subtags, never raw prefixes: prefix
    // matching accepted unrelated ISO codes ("hil"/"hif" → Hindi) and
    // typos ("español", "enjoy"), and `contains("-tw")` fired on any
    // subtag merely ending in "-tw". Unsupported input must fall through
    // to None and then the documented English fallback.
    let is_language = |tag: &str| value.split('-').next() == Some(tag);
    let has_subtag = |tag: &str| value.split('-').any(|part| part == tag);

    if is_language("c") || is_language("posix") || is_language("en") {
        return Some(Locale::En);
    }
    if is_language("zh") {
        if has_subtag("hant") || has_subtag("tw") || has_subtag("hk") || has_subtag("mo") {
            return Some(Locale::ZhHant);
        }
        return Some(Locale::ZhHans);
    }
    if is_language("hi") {
        return Some(Locale::Hi);
    }
    if is_language("es") {
        return Some(Locale::Es419);
    }
    None
}

fn fallback_translation(candidate: Option<&'static str>, id: MessageId) -> &'static str {
    candidate.unwrap_or_else(|| english(id))
}

fn english(id: MessageId) -> &'static str {
    match id {
        MessageId::SettingsTitle => "Settings:",
        MessageId::SettingsConfigFile => "Config file:",
    }
}

fn translation(locale: Locale, id: MessageId) -> Option<&'static str> {
    match locale {
        Locale::En => Some(english(id)),
        Locale::ZhHans => chinese_simplified(id),
        Locale::ZhHant => traditional_chinese(id),
        Locale::Hi => hindi(id),
        Locale::Es419 => spanish_latin_america(id),
    }
}

fn traditional_chinese(id: MessageId) -> Option<&'static str> {
    // No zh-Hant-specific overrides remain; fall through to zh-Hans.
    chinese_simplified(id)
}

fn chinese_simplified(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::SettingsTitle => "设置：",
        MessageId::SettingsConfigFile => "配置文件：",
    })
}

fn hindi(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::SettingsTitle => "सेटिंग:",
        MessageId::SettingsConfigFile => "कॉन्फ़िग फ़ाइल:",
    })
}

fn spanish_latin_america(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::SettingsTitle => "Configuraciones:",
        MessageId::SettingsConfigFile => "Archivo de configuración:",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_setting_normalizes_supported_tags() {
        assert_eq!(normalize_configured_locale("auto"), Some("auto"));
        assert_eq!(normalize_configured_locale("en_US.UTF-8"), Some("en"));
        assert_eq!(normalize_configured_locale("zh-CN"), Some("zh-Hans"));
        assert_eq!(normalize_configured_locale("zh-TW"), Some("zh-Hant"));
        assert_eq!(normalize_configured_locale("zh_HK.UTF-8"), Some("zh-Hant"));
        assert_eq!(normalize_configured_locale("hi_IN.UTF-8"), Some("hi"));
        assert_eq!(normalize_configured_locale("es"), Some("es-419"));
        assert_eq!(normalize_configured_locale("es-MX"), Some("es-419"));
    }

    #[test]
    fn locale_parsing_requires_subtag_boundaries() {
        // Unrelated ISO codes that merely share a prefix with a shipped
        // language must not be silently mapped to it.
        assert_eq!(normalize_configured_locale("hil"), None); // Hiligaynon
        assert_eq!(normalize_configured_locale("hif-FJ"), None); // Fiji Hindi
        // Neither may settings typos.
        assert_eq!(normalize_configured_locale("español"), None);
        assert_eq!(normalize_configured_locale("enjoy"), None);
        // Well-formed subtag forms still resolve.
        assert_eq!(normalize_configured_locale("hi-IN"), Some("hi"));
        assert_eq!(normalize_configured_locale("zh-Hant-TW"), Some("zh-Hant"));
        assert_eq!(normalize_configured_locale("es-419"), Some("es-419"));
    }

    #[test]
    fn locale_resolution_uses_config_then_environment_then_english() {
        assert_eq!(
            resolve_locale_with_env("hi", |_| Some("zh_CN.UTF-8".to_string())),
            Locale::Hi
        );
        assert_eq!(
            resolve_locale_with_env("auto", |key| {
                (key == "LANG").then(|| "zh_CN.UTF-8".to_string())
            }),
            Locale::ZhHans
        );
        assert_eq!(
            resolve_locale_with_env("auto", |key| {
                (key == "LANG").then(|| "zh_TW.UTF-8".to_string())
            }),
            Locale::ZhHant
        );
        assert_eq!(
            resolve_locale_with_env("auto", |key| {
                (key == "LANG").then(|| "hi_IN.UTF-8".to_string())
            }),
            Locale::Hi
        );
        assert_eq!(resolve_locale_with_env("auto", |_| None), Locale::En);
    }

    #[test]
    fn shipped_first_pack_has_no_missing_core_messages() {
        for locale in Locale::shipped() {
            assert!(
                missing_message_ids(*locale).is_empty(),
                "{} is missing messages",
                locale.tag()
            );
        }
    }

    #[test]
    fn unsupported_locale_falls_back_to_english() {
        assert_eq!(
            resolve_locale_with_env("ar", |_| None),
            Locale::En,
            "Arabic is not a shipped locale"
        );
        assert_eq!(
            resolve_locale_with_env("ja", |_| None),
            Locale::En,
            "Japanese support was dropped and must fall back to English"
        );
        assert_eq!(
            resolve_locale_with_env("vi", |_| None),
            Locale::En,
            "Vietnamese support was dropped and must fall back to English"
        );
        assert_eq!(
            resolve_locale_with_env("pt-BR", |_| None),
            Locale::En,
            "Brazilian Portuguese support was dropped and must fall back to English"
        );
    }

    #[test]
    fn missing_translation_falls_back_to_english() {
        assert_eq!(
            fallback_translation(None, MessageId::SettingsTitle),
            english(MessageId::SettingsTitle)
        );
    }

    #[test]
    fn width_truncation_handles_cjk_indic_and_latin_samples() {
        let samples = [
            ("zh-Hans", "输入以筛选配置"),
            ("hi", "सेटिंग खोजें"),
            ("es-419", "configuraciones filtradas"),
        ];

        for (tag, sample) in samples {
            let truncated = truncate_to_width(sample, 12);
            assert!(
                truncated.width() <= 12,
                "{tag} sample overflowed: {truncated:?}"
            );
        }
    }

    #[test]
    fn width_truncation_cuts_on_grapheme_cluster_boundaries() {
        // Whatever the column budget, the truncated body must never end on
        // a zero-width joiner or a combining mark split from its base — the
        // chars() loop used to break right after a U+200D, rendering a
        // broken glyph in front of the ellipsis.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let mixed = format!("a{family}बिcd\u{0301}e");
        assert_eq!(truncate_to_width(&mixed, 0), "");
        for budget in 1..=20 {
            let out = truncate_to_width(&mixed, budget);
            let body = out.strip_suffix('…').unwrap_or(&out);
            assert!(
                !body.ends_with('\u{200D}') && !body.ends_with('\u{0301}'),
                "cut inside a grapheme cluster at budget {budget}: {out:?}"
            );
        }
    }
}
