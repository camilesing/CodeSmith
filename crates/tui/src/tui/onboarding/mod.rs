//! Onboarding gate rendering and helpers.
//!
//! The first-run wizard (Welcome/Language/Provider/Endpoint/Tips) moved to
//! the `codesmith` dispatcher's CLI question flow. What remains here renders
//! the two re-entry gates — `ApiKey` for a session whose effective provider
//! resolves no credential (e.g. after `/logout`), `TrustDirectory` for a
//! first visit to an untrusted workspace — and owns the input validation
//! those screens share with the CLI flow's counterparts.

pub mod api_key;
pub mod trust_directory;

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
    const DEFAULT_PANEL_HEIGHT: u16 = 20;
    // The key screen stacks two hint lines that wrap in es (and hit zero
    // slack elsewhere) plus a status line on nearly every keystroke — 17
    // rows against the default panel's 16 usable.
    const API_KEY_PANEL_HEIGHT: u16 = 22;

    let content_width = 76.min(area.width.saturating_sub(4));
    let wanted_height = match app.onboarding {
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
        OnboardingState::ApiKey => api_key::lines(app),
        OnboardingState::TrustDirectory => trust_directory::lines(app),
        OnboardingState::None => Vec::new(),
    };

    if !lines.is_empty() {
        let panel = Block::default()
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
        let inner = panel.inner(content_area);
        f.render_widget(panel, content_area);
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        f.render_widget(paragraph, inner);
    }
}

// Marker + trust-gate inputs moved to `codesmith_agent_runtime::onboarding`
// so the CLI first-run wizard and the TUI gates share one definition of
// "already onboarded"; re-exported here for the historical paths.
pub use codesmith_agent_runtime::onboarding::{is_onboarded, mark_onboarded, needs_trust};

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

/// Advance past the credential gate: to the trust gate when the workspace
/// still needs a decision, otherwise out of onboarding entirely — writing
/// the first-run marker on the way out (idempotent for already-onboarded
/// sessions; the CLI wizard normally owns it).
pub fn advance_onboarding_after_credentials(app: &mut App) {
    app.status_message = None;
    if !app.trust_mode && needs_trust(&app.workspace) {
        app.onboarding = OnboardingState::TrustDirectory;
    } else {
        app.onboarding = OnboardingState::None;
        if let Err(err) = mark_onboarded() {
            app.status_message = Some(format!("Failed to mark onboarding: {err}"));
        }
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
}
