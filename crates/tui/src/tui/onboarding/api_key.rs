//! API key entry screen for onboarding.
//!
//! The screen follows the provider picked on `OnboardingState::Provider`
//! (`App::onboarding_provider`): the title names it, step 1 points at its
//! console — DeepSeek keeps the curated link, everything else gets the generic
//! line — and the hint names the environment variable that also works. Saving
//! routes through `App::submit_api_key_for`, so the key lands in the slot the
//! provider actually reads.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::ApiProvider;
use crate::localization::MessageId;
use crate::palette;
use crate::tui::app::App;
use crate::tui::provider_picker::env_var_for;

pub fn lines(app: &App) -> Vec<Line<'static>> {
    let provider = app.onboarding_provider();
    let mut lines = vec![
        Line::from(Span::styled(
            key_screen_title(app, provider),
            Style::default()
                .fg(palette::CODESMITH_SKY)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.tr(step_1_message(provider)).to_string(),
            Style::default().fg(palette::TEXT_PRIMARY),
        )),
        Line::from(Span::styled(
            app.tr(MessageId::OnboardApiKeyStep2).to_string(),
            Style::default().fg(palette::TEXT_PRIMARY),
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.tr(MessageId::OnboardApiKeySavedHint).to_string(),
            Style::default().fg(palette::TEXT_MUTED),
        )),
        Line::from(Span::styled(
            app.tr(MessageId::OnboardApiKeyFormatHint).to_string(),
            Style::default().fg(palette::TEXT_MUTED),
        )),
        Line::from(Span::styled(
            environment_hint(app, provider),
            Style::default().fg(palette::TEXT_MUTED),
        )),
        Line::from(""),
    ];

    let masked = mask_key(&app.api_key_input);
    let placeholder = app.tr(MessageId::OnboardApiKeyPlaceholder).to_string();
    let display = if masked.is_empty() {
        placeholder
    } else {
        masked
    };
    lines.push(Line::from(vec![
        Span::styled(
            app.tr(MessageId::OnboardApiKeyLabel).to_string(),
            Style::default().fg(palette::TEXT_MUTED),
        ),
        Span::styled(
            display,
            Style::default()
                .fg(palette::TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));

    if let Some(message) = app.status_message.as_deref() {
        lines.push(Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(palette::STATUS_WARNING),
        )));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.tr(MessageId::OnboardApiKeyFooter).to_string(),
        Style::default().fg(palette::TEXT_MUTED),
    )));

    lines
}

fn mask_key(input: &str) -> String {
    let trimmed = input.trim();
    let len = trimmed.chars().count();
    if len == 0 {
        return String::new();
    }
    if len <= 4 {
        return "*".repeat(len);
    }
    let visible: String = trimmed
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{}{}", "*".repeat(len - 4), visible)
}

/// "Connect your <provider> API key" — the provider name is data, so the
/// sentence is split around it rather than duplicated per provider.
fn key_screen_title(app: &App, provider: ApiProvider) -> String {
    format!(
        "{}{}{}",
        app.tr(MessageId::OnboardApiKeyTitlePrefix),
        provider.display_name(),
        app.tr(MessageId::OnboardApiKeyTitleSuffix)
    )
}

/// DeepSeek is the fallback provider and ships a curated console link; every
/// other provider gets the generic "create a key in your provider's console"
/// line (see the known-limitations note in `onboarding::provider`).
fn step_1_message(provider: ApiProvider) -> MessageId {
    if matches!(provider, ApiProvider::Deepseek) {
        MessageId::OnboardApiKeyStep1Deepseek
    } else {
        MessageId::OnboardApiKeyStep1Generic
    }
}

fn environment_hint(app: &App, provider: ApiProvider) -> String {
    format!(
        "{}{}{}",
        app.tr(MessageId::OnboardApiKeyEnvHintPrefix),
        env_var_for(provider),
        app.tr(MessageId::OnboardApiKeyEnvHintSuffix)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::localization::Locale;
    use crate::tui::app::TuiOptions;
    use std::path::PathBuf;

    fn test_app_with_locale(locale: Locale) -> App {
        let options = TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace: PathBuf::from("."),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            use_alt_screen: true,
            use_mouse_capture: false,
            use_bracketed_paste: true,
            max_subagents: 1,
            skills_dir: PathBuf::from("."),
            memory_path: PathBuf::from("memory.md"),
            notes_path: PathBuf::from("notes.txt"),
            mcp_config_path: PathBuf::from("mcp.json"),
            use_memory: false,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        let mut app = App::new(options, &Config::default());
        app.ui_locale = locale;
        app
    }

    fn body(app: &App) -> String {
        lines(app)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn api_key_screen_renders_in_selected_locale() {
        // The most-visible regression of the missing onboarding-localization:
        // after the user picks 简体中文 at step 2, step 3 used to remain
        // English. Pin that the rendered lines actually contain the
        // translated strings for each locale we ship.
        let zh = test_app_with_locale(Locale::ZhHans);
        let zh_body = body(&zh);
        assert!(
            zh_body.contains("连接你的 DeepSeek API 密钥"),
            "expected zh-Hans title, got: {zh_body}"
        );
        assert!(
            zh_body.contains("密钥"),
            "expected zh-Hans 'key' label, got: {zh_body}"
        );
        assert!(
            zh_body.contains("Enter 保存"),
            "expected zh-Hans footer, got: {zh_body}"
        );

        let hi = test_app_with_locale(Locale::Hi);
        let hi_body = body(&hi);
        assert!(
            hi_body.contains("अपनी DeepSeek API key जोड़ें"),
            "expected hi title, got: {hi_body}"
        );

        let en = test_app_with_locale(Locale::En);
        assert!(
            body(&en).contains("Press Enter to save"),
            "expected en footer"
        );
    }

    #[test]
    fn title_and_env_hint_follow_the_selected_provider() {
        let mut app = test_app_with_locale(Locale::En);
        // First-run default: the DeepSeek fallback.
        let deepseek_body = body(&app);
        assert!(
            deepseek_body.contains("Connect your DeepSeek API key"),
            "default title should name DeepSeek: {deepseek_body}"
        );
        assert!(
            deepseek_body.contains("DEEPSEEK_API_KEY"),
            "default hint should name DeepSeek's env var: {deepseek_body}"
        );

        app.set_onboarding_provider(ApiProvider::Openrouter);
        let openrouter_body = body(&app);
        assert!(
            openrouter_body.contains("Connect your OpenRouter API key"),
            "title should follow the picked provider: {openrouter_body}"
        );
        assert!(
            openrouter_body.contains("OPENROUTER_API_KEY"),
            "hint should name the picked provider's env var: {openrouter_body}"
        );
        assert!(
            !openrouter_body.contains("platform.deepseek.com"),
            "non-DeepSeek providers must not get the DeepSeek console link: {openrouter_body}"
        );

        app.set_onboarding_provider(ApiProvider::Deepseek);
        assert!(
            body(&app).contains("platform.deepseek.com"),
            "DeepSeek keeps its curated console link"
        );
    }
}
