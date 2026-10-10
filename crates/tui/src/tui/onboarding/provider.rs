//! Provider picker for first-run onboarding.
//!
//! Every shipped provider is first-class, so the first run asks which one to
//! talk to instead of assuming the DeepSeek fallback. Rows use the same
//! vocabulary as the `/provider` modal (provider display names, plus whether a
//! key is already resolvable) but render inside the onboarding panel: while the
//! wizard is open every key is consumed by the onboarding branch and `render`
//! returns before the view stack, so the modal itself is unreachable here.
//!
//! Known limitations, owned here: the list is the builtin providers from
//! [`ApiProvider::all`] — hand-written `[[providers.custom]]` entries are not
//! offered — and the screen selects a provider only. Base URLs, models, and
//! per-provider key consoles (DeepSeek keeps its curated link on the next
//! screen, others get a generic line) remain `/config` and `config.toml` work.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::{ApiProvider, kimi_cli_credentials_present, provider_is_self_hosted};
use crate::localization::MessageId;
use crate::palette;
use crate::tui::app::App;

/// Column the status hint starts at. Wide enough for the longest display name
/// (`OpenAI-compatible`, 17) plus a gap, so hints line up down the list.
const HINT_COLUMN: usize = 20;

/// Smallest list window we will draw, so the marker always has room to move
/// even in a very short terminal.
pub const MIN_VISIBLE_ROWS: usize = 3;

/// Render the screen. `visible_rows` is the list window the caller's panel can
/// hold; the drawn window always contains the selected row.
pub fn lines(app: &App, visible_rows: usize) -> Vec<Line<'static>> {
    let selected = app.onboarding_provider_idx;
    // A status line (a failed switch) takes the place of one list row so
    // the trailing footer stays inside the panel.
    let status_rows: usize = usize::from(app.status_message.is_some());
    let visible_rows = visible_rows
        .saturating_sub(status_rows)
        .max(MIN_VISIBLE_ROWS);

    let mut out: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            app.tr(MessageId::OnboardProviderTitle).to_string(),
            Style::default()
                .fg(palette::CODESMITH_SKY)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.tr(MessageId::OnboardProviderBlurb).to_string(),
            Style::default().fg(palette::TEXT_MUTED),
        )),
        Line::from(""),
    ];

    let rows = &app.onboarding_provider_rows;
    let start = selected
        .saturating_add(1)
        .saturating_sub(visible_rows)
        .min(rows.len().saturating_sub(visible_rows));
    for (idx, (provider, has_key)) in rows.iter().enumerate().skip(start).take(visible_rows) {
        let is_selected = idx == selected;
        let marker = if is_selected { "▸" } else { " " };
        let marker_style = if is_selected {
            Style::default()
                .fg(palette::CODESMITH_BLUE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette::TEXT_MUTED)
        };
        let name_style = if is_selected {
            Style::default()
                .fg(palette::TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette::TEXT_PRIMARY)
        };
        let status = status_message(*provider, *has_key);
        out.push(Line::from(vec![
            Span::styled(format!("  {marker} "), marker_style),
            Span::styled(
                format!("{:<HINT_COLUMN$}", provider.display_name()),
                name_style,
            ),
            Span::styled(
                app.tr(status).to_string(),
                Style::default().fg(status_color(status)),
            ),
        ]));
    }

    if let Some(message) = app.status_message.as_deref() {
        out.push(Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(palette::STATUS_WARNING),
        )));
    }
    out.push(Line::from(""));
    out.push(Line::from(Span::styled(
        app.tr(MessageId::OnboardProviderFooter).to_string(),
        Style::default().fg(palette::TEXT_MUTED),
    )));

    out
}

/// Status label for one row. Order matters and mirrors the `/provider`
/// picker's list stage, so the label always names the credential Enter
/// would actually use: self-hosted runtimes never need a key (and
/// `has_api_key_for` answers `true` for them, so they are split off before
/// the generic configured/needs-key split), an explicit key outranks a
/// registered Kimi CLI credential, and the OAuth label is reached only
/// when no key resolves.
fn status_message(provider: ApiProvider, has_key: bool) -> MessageId {
    if provider_is_self_hosted(provider) {
        return MessageId::OnboardProviderNoKeyRequired;
    }
    if has_key {
        return MessageId::OnboardProviderConfigured;
    }
    if provider == ApiProvider::Moonshot && kimi_cli_credentials_present() {
        return MessageId::OnboardProviderKimiOAuthReady;
    }
    MessageId::OnboardProviderNeedsKey
}

fn status_color(status: MessageId) -> ratatui::style::Color {
    match status {
        MessageId::OnboardProviderNeedsKey => palette::STATUS_WARNING,
        _ => palette::TEXT_MUTED,
    }
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

    /// Rendered text of the screen, one line per line and spans concatenated —
    /// substring assertions need the spans of a row joined, not separated.
    fn body(app: &App, visible_rows: usize) -> String {
        lines(app, visible_rows)
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
    fn every_builtin_provider_is_offered() {
        let app = test_app_with_locale(Locale::En);
        let rendered = body(&app, 32);
        assert_eq!(app.onboarding_provider_rows.len(), ApiProvider::all().len());
        for provider in ApiProvider::all() {
            assert!(
                rendered.contains(provider.display_name()),
                "provider screen omits {}: {rendered}",
                provider.display_name()
            );
        }
    }

    #[test]
    fn deepseek_is_marked_as_the_selection() {
        let mut app = test_app_with_locale(Locale::En);
        app.set_onboarding_provider(ApiProvider::Deepseek);
        let rendered = body(&app, 32);
        assert!(
            rendered.contains("▸ DeepSeek"),
            "selection marker missing: {rendered}"
        );
    }

    #[test]
    fn short_panel_keeps_the_selected_row_visible() {
        let mut app = test_app_with_locale(Locale::En);
        app.set_onboarding_provider(ApiProvider::Anthropic);
        let rendered = body(&app, MIN_VISIBLE_ROWS);
        assert!(
            rendered.contains("▸ Anthropic"),
            "windowed list dropped the selected row: {rendered}"
        );
    }

    #[test]
    fn status_labels_are_localized() {
        let mut app = test_app_with_locale(Locale::ZhHans);
        // Rows are pinned rather than derived from the host environment: the
        // label depends on whether a key is resolvable.
        app.onboarding_provider_rows =
            vec![(ApiProvider::Deepseek, false), (ApiProvider::Ollama, true)];
        let rendered = body(&app, 32);
        assert!(
            rendered.contains("需要 API 密钥"),
            "expected zh-Hans needs-key label, got: {rendered}"
        );
        assert!(
            rendered.contains("无需 API 密钥"),
            "expected zh-Hans no-key label, got: {rendered}"
        );
        assert!(
            rendered.contains("选择服务商"),
            "expected zh-Hans title, got: {rendered}"
        );
    }
}
