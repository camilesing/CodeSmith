//! Custom endpoint URL screen for onboarding.
//!
//! Asked only for the two generic routes — an OpenAI-compatible gateway/proxy
//! and an Anthropic-compatible one (`config::provider_supports_custom_endpoint`)
//! — because those are the providers users legitimately point at their own
//! URL. Hosted providers keep their service URLs; every provider can still be
//! re-pointed by hand through `[providers.<name>] base_url` or its
//! `*_BASE_URL` environment variable.
//!
//! Enter with an empty input keeps whatever is configured (including the
//! environment variable, which this screen never writes into the config file);
//! a pasted URL is validated, written to `[providers.<name>] base_url`, and
//! mirrored into the live config by the caller.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::provider_base_url_env_var;
use crate::localization::MessageId;
use crate::palette;
use crate::tui::app::App;

pub fn lines(app: &App) -> Vec<Line<'static>> {
    let provider = app.onboarding_provider();
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            app.tr(MessageId::OnboardEndpointTitle).to_string(),
            Style::default()
                .fg(palette::CODESMITH_SKY)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.tr(MessageId::OnboardEndpointBlurb).to_string(),
            Style::default().fg(palette::TEXT_PRIMARY),
        )),
        Line::from(""),
    ];

    if let Some(default) = app.onboarding_endpoint_default.as_deref() {
        lines.push(Line::from(vec![
            Span::styled(
                app.tr(MessageId::OnboardEndpointDefaultLabel).to_string(),
                Style::default().fg(palette::TEXT_MUTED),
            ),
            Span::styled(
                default.to_string(),
                Style::default().fg(palette::TEXT_PRIMARY),
            ),
        ]));
    }
    if let Some(var) = provider_base_url_env_var(provider) {
        lines.push(Line::from(Span::styled(
            format!(
                "{}{}{}",
                app.tr(MessageId::OnboardEndpointEnvHintPrefix),
                var,
                app.tr(MessageId::OnboardEndpointEnvHintSuffix)
            ),
            Style::default().fg(palette::TEXT_MUTED),
        )));
    }
    let display = if app.onboarding_endpoint_input.is_empty() {
        app.tr(MessageId::OnboardEndpointPlaceholder).to_string()
    } else {
        app.onboarding_endpoint_input.clone()
    };
    lines.push(Line::from(vec![
        Span::styled(
            app.tr(MessageId::OnboardEndpointLabel).to_string(),
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

    lines.push(Line::from(Span::styled(
        app.tr(MessageId::OnboardEndpointFooter).to_string(),
        Style::default().fg(palette::TEXT_MUTED),
    )));

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiProvider, Config};
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
    fn shows_the_providers_default_url_and_env_var() {
        let mut app = test_app_with_locale(Locale::En);
        app.begin_onboarding_endpoint(&Config::default(), ApiProvider::Openai);
        let rendered = body(&app);
        assert!(
            rendered.contains("Default: https://api.openai.com/v1"),
            "expected OpenAI's service URL as the default, got: {rendered}"
        );
        assert!(
            rendered.contains("OPENAI_BASE_URL"),
            "expected the env-var alternative, got: {rendered}"
        );

        app.begin_onboarding_endpoint(&Config::default(), ApiProvider::Anthropic);
        let rendered = body(&app);
        assert!(
            rendered.contains("Default: https://api.anthropic.com/v1"),
            "expected Anthropic's service URL as the default, got: {rendered}"
        );
        assert!(rendered.contains("ANTHROPIC_BASE_URL"));
    }

    #[test]
    fn configured_url_outranks_the_builtin_default() {
        let mut app = test_app_with_locale(Locale::En);
        let config = Config {
            provider: Some("openai".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                openai: crate::config::ProviderConfig {
                    base_url: Some("https://proxy.internal/v1".to_string()),
                    ..crate::config::ProviderConfig::default()
                },
                ..crate::config::ProvidersConfig::default()
            }),
            ..Config::default()
        };
        app.begin_onboarding_endpoint(&config, ApiProvider::Openai);
        assert_eq!(
            app.onboarding_endpoint_default.as_deref(),
            Some("https://proxy.internal/v1")
        );
        assert!(body(&app).contains("https://proxy.internal/v1"));
    }

    #[test]
    fn typed_url_replaces_the_placeholder() {
        let mut app = test_app_with_locale(Locale::ZhHans);
        app.begin_onboarding_endpoint(&Config::default(), ApiProvider::Openai);
        assert!(
            body(&app).contains("（粘贴自定义地址）"),
            "empty input should show the localized placeholder"
        );
        for c in "https://proxy.example/v1".chars() {
            app.insert_onboarding_endpoint_char(c);
        }
        let rendered = body(&app);
        assert!(rendered.contains("地址：https://proxy.example/v1"));
        assert!(rendered.contains("自定义接入地址"), "localized title");
    }
}
