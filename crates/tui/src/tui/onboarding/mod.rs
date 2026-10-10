//! Onboarding flow rendering and helpers.

pub mod api_key;
pub mod endpoint;
pub mod language;
pub mod provider;
pub mod trust_directory;
pub mod welcome;

use std::path::{Path, PathBuf};

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Wrap},
};

use crate::palette;
use crate::tui::app::{App, OnboardingState};

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().style(Style::default().bg(palette::CODESMITH_INK));
    f.render_widget(block, area);

    const TOP_MARGIN: u16 = 2;
    // Every screen but the provider list and the key screen fits the
    // default panel.
    const DEFAULT_PANEL_HEIGHT: u16 = 20;
    // The provider list is 16 rows plus title, blurb, and footer.
    const PROVIDER_PANEL_HEIGHT: u16 = 28;
    // The key screen stacks two hint lines that wrap in es (and hit zero
    // slack elsewhere) plus a status line on nearly every keystroke — 17
    // rows against the default panel's 16 usable.
    const API_KEY_PANEL_HEIGHT: u16 = 22;
    // Non-list lines the provider screen draws: title, blurb (wraps to two
    // lines in en/es/hi at the 70-column inner width; zh fits on one — the
    // budget is the worst case), the blank pair above the list, and the
    // blank + footer below it.
    const PROVIDER_LIST_CHROME: u16 = 7;
    // Panel borders (2) plus vertical padding (2).
    const PANEL_INSET: u16 = 4;

    let content_width = 76.min(area.width.saturating_sub(4));
    let wanted_height = match app.onboarding {
        OnboardingState::Provider => PROVIDER_PANEL_HEIGHT,
        OnboardingState::ApiKey => API_KEY_PANEL_HEIGHT,
        _ => DEFAULT_PANEL_HEIGHT,
    };
    let content_height = wanted_height.min(area.height.saturating_sub(TOP_MARGIN + 2));
    let content_area = Rect {
        x: (area.width.saturating_sub(content_width)) / 2,
        y: TOP_MARGIN,
        width: content_width,
        height: content_height,
    };

    let lines = match app.onboarding {
        OnboardingState::Welcome => welcome::lines(),
        OnboardingState::Language => language::lines(app),
        OnboardingState::Provider => {
            // Rows the panel can hold once its own chrome is subtracted; the
            // window then always contains the selected row.
            let list_rows =
                usize::from(content_height.saturating_sub(PANEL_INSET + PROVIDER_LIST_CHROME));
            provider::lines(app, list_rows)
        }
        OnboardingState::Endpoint => endpoint::lines(app),
        OnboardingState::ApiKey => api_key::lines(app),
        OnboardingState::TrustDirectory => trust_directory::lines(app),
        OnboardingState::Tips => tips_lines(app),
        OnboardingState::None => Vec::new(),
    };

    if !lines.is_empty() {
        let mut panel = Block::default()
            .title(Line::from(Span::styled(
                " CodeSmith ",
                Style::default()
                    .fg(palette::CODESMITH_BLUE)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(Style::default().bg(palette::CODESMITH_SLATE))
            .padding(Padding::new(2, 2, 1, 1));
        if !app.onboarding_workspace_trust_gate {
            let (step, total) = onboarding_step(app);
            panel = panel.title_bottom(Line::from(Span::styled(
                format!(" Step {step}/{total} "),
                Style::default()
                    .fg(palette::TEXT_MUTED)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        let inner = panel.inner(content_area);
        f.render_widget(panel, content_area);
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        f.render_widget(paragraph, inner);
    }
}

fn onboarding_step(app: &App) -> (usize, usize) {
    let needs_trust = !app.trust_mode && needs_trust(&app.workspace);
    // Welcome + Language + Tips are always shown.
    let mut total = 3;
    if app.onboarding_needs_api_key {
        total += 1;
    }
    if needs_trust {
        total += 1;
    }

    let step = match app.onboarding {
        OnboardingState::Welcome => 1,
        OnboardingState::Language => 2,
        // Provider, Endpoint, and ApiKey are one credential step: the endpoint
        // screen is asked only for the generic routes and the key screen only
        // when nothing resolves a credential, so all three report the same slot
        // rather than inflating the count.
        OnboardingState::Provider | OnboardingState::Endpoint | OnboardingState::ApiKey => 3,
        OnboardingState::TrustDirectory => {
            // Welcome (1) + Language (2) + optional credential step
            if app.onboarding_needs_api_key { 4 } else { 3 }
        }
        OnboardingState::Tips => total,
        OnboardingState::None => total,
    };

    (step, total)
}

pub fn tips_lines(app: &App) -> Vec<ratatui::text::Line<'static>> {
    use crate::localization::MessageId;
    use ratatui::style::Modifier;
    use ratatui::text::{Line, Span};

    vec![
        Line::from(Span::styled(
            app.tr(MessageId::OnboardTipsTitle).to_string(),
            Style::default()
                .fg(palette::CODESMITH_SKY)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::raw(app.tr(MessageId::OnboardTipsLine1).to_string())),
        Line::from(Span::raw(app.tr(MessageId::OnboardTipsLine2).to_string())),
        Line::from(Span::raw(app.tr(MessageId::OnboardTipsLine3).to_string())),
        Line::from(Span::raw(app.tr(MessageId::OnboardTipsLine4).to_string())),
        Line::from(vec![
            Span::styled(
                app.tr(MessageId::OnboardTipsFooterEnter).to_string(),
                Style::default()
                    .fg(palette::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                app.tr(MessageId::OnboardTipsFooterAction).to_string(),
                Style::default().fg(palette::TEXT_MUTED),
            ),
        ]),
    ]
}

pub fn default_marker_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codesmith").join(".onboarded"))
}

pub fn is_onboarded() -> bool {
    default_marker_path().is_some_and(|path| path.exists())
}

pub fn mark_onboarded() -> std::io::Result<PathBuf> {
    let path = default_marker_path().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "Home directory not found")
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, "")?;
    Ok(path)
}

pub fn needs_trust(workspace: &Path) -> bool {
    if crate::config::is_workspace_trusted(workspace) {
        return false;
    }

    let markers = [
        workspace.join(".codesmith").join("trusted"),
        workspace.join(".codesmith").join("trust.json"),
    ];
    !markers.iter().any(|path| path.exists())
}

pub fn mark_trusted(workspace: &Path) -> anyhow::Result<PathBuf> {
    crate::config::save_workspace_trust(workspace)
}

// ── Input validation and state-machine transitions ───────────────────

/// Result of inspecting a text field entered during onboarding (an API key, a
/// custom endpoint URL).
///
/// `Accept` always lets the user proceed; the optional `warning` is shown
/// as a non-blocking status message (short keys, plain-http endpoints, …).
/// `Reject` blocks the keystroke flow until the user fixes the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputValidation {
    Accept { warning: Option<String> },
    Reject(String),
}

/// Validate an API key entered during onboarding. Whitespace-only or
/// whitespace-containing keys are rejected; short or hyphen-less keys
/// are accepted with a warning so unusual provider key formats still
/// work.
#[must_use]
pub fn validate_api_key_for_onboarding(api_key: &str) -> InputValidation {
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        return InputValidation::Reject("API key cannot be empty.".to_string());
    }
    if trimmed.contains(char::is_whitespace) {
        return InputValidation::Reject(
            "API key appears malformed (contains whitespace).".to_string(),
        );
    }
    if trimmed.len() < 16 {
        return InputValidation::Accept {
            warning: Some(
                "API key looks short. Double-check it, but unusual formats are allowed."
                    .to_string(),
            ),
        };
    }
    if !trimmed.contains('-') {
        return InputValidation::Accept {
            warning: Some(
                "API key format looks unusual. Check that the full key was copied.".to_string(),
            ),
        };
    }
    InputValidation::Accept { warning: None }
}

/// Validate a custom endpoint URL entered during onboarding.
///
/// Requires an absolute `http://` / `https://` URL with a host. Plain `http`
/// to anything but a local address is accepted **with a warning**: a trusted
/// LAN endpoint is a documented setup for this project, but the API key then
/// travels in the clear, and a proxy URL is exactly the case where the
/// credential would be sent somewhere the user did not intend.
#[must_use]
pub fn validate_endpoint_url_for_onboarding(url: &str) -> InputValidation {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return InputValidation::Reject("Endpoint URL cannot be empty.".to_string());
    }
    if trimmed.contains(char::is_whitespace) {
        return InputValidation::Reject("Endpoint URL contains whitespace.".to_string());
    }
    let Some(rest) = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
    else {
        return InputValidation::Reject(
            "Endpoint URL must start with https:// or http://.".to_string(),
        );
    };
    if rest.is_empty() || rest.starts_with('/') {
        return InputValidation::Reject("Endpoint URL is missing a host.".to_string());
    }
    let authority = rest.split('/').next().unwrap_or_default();
    let is_local = authority.starts_with("localhost")
        || authority.starts_with("127.")
        || authority.starts_with("[::1]")
        || authority.ends_with(".local");
    if trimmed.starts_with("http://") && !is_local {
        return InputValidation::Accept {
            warning: Some(
                "Plain http sends your API key in the clear — use https unless this is a trusted \
                 LAN endpoint."
                    .to_string(),
            ),
        };
    }
    InputValidation::Accept { warning: None }
}

/// Welcome → Language transition. Clears the status message bar.
pub fn advance_onboarding_from_welcome(app: &mut App) {
    app.status_message = None;
    app.onboarding = OnboardingState::Language;
}

/// Language → next step. Routes to the provider picker when the session still
/// needs credentials (the picker decides whether the key screen follows), to
/// TrustDirectory when the workspace is untrusted, otherwise to Tips.
///
/// Also used as "advance past the credential step": by then
/// `onboarding_needs_api_key` is `false`, so it lands on trust or tips.
pub fn advance_onboarding_after_language(app: &mut App) {
    app.status_message = None;
    if app.onboarding_needs_api_key {
        app.onboarding = OnboardingState::Provider;
    } else if !app.trust_mode && needs_trust(&app.workspace) {
        app.onboarding = OnboardingState::TrustDirectory;
    } else {
        app.onboarding = OnboardingState::Tips;
    }
}

/// Re-validate the current `api_key_input` and project the result onto
/// `app.status_message`. `show_empty_error` reports the "cannot be empty"
/// message even when the input has not been touched yet (used right
/// before submission); otherwise an empty input clears the status bar.
pub fn sync_api_key_validation_status(app: &mut App, show_empty_error: bool) {
    if app.api_key_input.trim().is_empty() && !show_empty_error {
        app.status_message = None;
        return;
    }

    match validate_api_key_for_onboarding(&app.api_key_input) {
        InputValidation::Accept { warning } => {
            app.status_message = warning;
        }
        InputValidation::Reject(message) => {
            app.status_message = Some(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_empty_or_whitespace() {
        assert!(matches!(
            validate_api_key_for_onboarding(""),
            InputValidation::Reject(_)
        ));
        assert!(matches!(
            validate_api_key_for_onboarding("   "),
            InputValidation::Reject(_)
        ));
        assert!(matches!(
            validate_api_key_for_onboarding("sk live abc"),
            InputValidation::Reject(_)
        ));
    }

    #[test]
    fn validate_warns_on_short_or_no_hyphen_keys_but_accepts() {
        match validate_api_key_for_onboarding("abc123") {
            InputValidation::Accept { warning: Some(_) } => {}
            _ => panic!("expected accept-with-warning"),
        }
        match validate_api_key_for_onboarding("abcdefghijklmnop") {
            InputValidation::Accept { warning: Some(_) } => {}
            _ => panic!("expected accept-with-warning"),
        }
    }

    #[test]
    fn validate_accepts_well_formed_key() {
        assert_eq!(
            validate_api_key_for_onboarding("sk-1234567890abcdef"),
            InputValidation::Accept { warning: None }
        );
    }

    #[test]
    fn endpoint_url_rejects_empty_relative_or_scheme_less_input() {
        for input in ["", "   ", "/v1", "api.openai.com/v1", "ftp://host/v1"] {
            assert!(
                matches!(
                    validate_endpoint_url_for_onboarding(input),
                    InputValidation::Reject(_)
                ),
                "{input:?} should be rejected"
            );
        }
    }

    #[test]
    fn endpoint_url_accepts_https_and_local_http() {
        assert_eq!(
            validate_endpoint_url_for_onboarding("https://proxy.example.com/v1"),
            InputValidation::Accept { warning: None }
        );
        assert_eq!(
            validate_endpoint_url_for_onboarding("http://127.0.0.1:8080/v1"),
            InputValidation::Accept { warning: None }
        );
        assert_eq!(
            validate_endpoint_url_for_onboarding("http://localhost:11434/v1"),
            InputValidation::Accept { warning: None }
        );
    }

    #[test]
    fn endpoint_url_warns_on_plain_http_to_a_remote_host() {
        match validate_endpoint_url_for_onboarding("http://gateway.lan:8000/v1") {
            InputValidation::Accept {
                warning: Some(message),
            } => {
                assert!(message.contains("clear"), "got {message:?}");
            }
            other => panic!("expected accept-with-warning, got {other:?}"),
        }
    }
}
