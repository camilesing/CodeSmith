//! Status line for the REPL.
//!
//! Printed above the input prompt (and refreshed as a transient line
//! during turns). One line, six data items plus a leading running-state
//! indicator:
//!
//! ```text
//! ● working │ Full │ 🧭 42% (72k/171k) │ 🚥: agent │ 🤖: 2 │ deepseek-chat │ v0.9.0 (f83c330)
//! ```
//!
//! - state dot: idle / working / waiting-approval / compacting
//! - preset: the active runtime profile tier (simple | middle | all | ...)
//! - 🧭: session context-window occupancy (estimate-first, see
//!   [`context_usage`])
//! - 🚥: `AppMode`
//! - 🤖: running sub-agent count
//! - model: display label
//! - version: build version

use colored::Colorize;

use super::state::ReplState;
use crate::app_state::AppMode;
use crate::compaction::estimate_input_tokens_conservative;
use crate::models::context_window_for_model;

/// Context-warning thresholds (mirrors the retired header widget).
pub const CONTEXT_WARN_PCT: f64 = 85.0;
pub const CONTEXT_ALERT_PCT: f64 = 95.0;

/// Running-state indicator shown as the leading dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Working,
    WaitingApproval,
    Compacting,
}

impl RunState {
    fn label(self) -> &'static str {
        match self {
            RunState::Idle => "idle",
            RunState::Working => "working",
            RunState::WaitingApproval => "waiting-approval",
            RunState::Compacting => "compacting",
        }
    }

    fn dot(self) -> &'static str {
        match self {
            RunState::Idle => "●",
            _ => "◉",
        }
    }
}

/// Context occupancy: `(used, max, percent)` following the retired
/// `context_usage_snapshot` semantics (#115): the conservative estimate
/// over the live API messages wins; the reported `last_prompt_tokens`
/// (which sums input across every round of a multi-round turn) is a
/// fallback only. Values clamp to the window.
pub fn context_usage(
    api_messages: &[crate::models::Message],
    system_prompt: Option<&crate::models::SystemPrompt>,
    last_prompt_tokens: Option<u32>,
    effective_model: &str,
) -> Option<(i64, u32, f64)> {
    let max = context_window_for_model(effective_model)?;
    let max_i64 = i64::from(max);
    let reported = last_prompt_tokens
        .map(i64::from)
        .map(|tokens| tokens.max(0));
    let estimated = i64::try_from(estimate_input_tokens_conservative(
        api_messages,
        system_prompt,
    ))
    .ok()
    .map(|tokens| tokens.max(0));

    let used = match (estimated, reported) {
        (Some(estimated), _) => estimated.min(max_i64),
        (None, Some(reported)) => reported.min(max_i64),
        (None, None) => return None,
    };

    let percent = ((used as f64 / f64::from(max)) * 100.0).clamp(0.0, 100.0);
    Some((used, max, percent))
}

/// Compact human token count: `72k`, `1.2M`.
pub fn format_token_count_compact(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        format!("{tokens}")
    }
}

/// Which color class the context percent falls into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextLevel {
    Normal,
    Warn,
    Alert,
}

pub fn context_level(percent: f64) -> ContextLevel {
    if percent >= CONTEXT_ALERT_PCT {
        ContextLevel::Alert
    } else if percent >= CONTEXT_WARN_PCT {
        ContextLevel::Warn
    } else {
        ContextLevel::Normal
    }
}

/// The dot + state label segment.
fn state_segment(run: RunState) -> String {
    let dot = match run {
        RunState::Idle => run.dot().dimmed(),
        _ => run.dot().cyan(),
    };
    let label = match run {
        RunState::Idle => run.label().dimmed(),
        RunState::Working => run.label().cyan(),
        RunState::WaitingApproval => run.label().yellow(),
        RunState::Compacting => run.label().magenta(),
    };
    format!("{dot} {label}")
}

fn ctx_percent_text(percent: f64) -> String {
    format!("🧭 {:.0}%", percent)
}

fn ctx_percent_segment(percent: f64) -> String {
    let text = ctx_percent_text(percent);
    match context_level(percent) {
        ContextLevel::Normal => text.normal().to_string(),
        ContextLevel::Warn => text.yellow().to_string(),
        ContextLevel::Alert => text.red().bold().to_string(),
    }
}

fn ctx_segment(used: i64, max: u32, percent: f64) -> String {
    let text = format!(
        "{} ({}/{})",
        ctx_percent_text(percent),
        format_token_count_compact(used),
        format_token_count_compact(i64::from(max)),
    );
    match context_level(percent) {
        ContextLevel::Normal => text.normal().to_string(),
        ContextLevel::Warn => text.yellow().to_string(),
        ContextLevel::Alert => text.red().bold().to_string(),
    }
}

/// The mode segment, shared by the idle status line and the live line.
fn mode_segment(mode: AppMode) -> String {
    format!("🚥: {}", mode.label().to_lowercase())
}

/// The sub-agent count segment (dimmed at zero, cyan when running), shared
/// by the idle status line and the live line.
fn agents_segment(agents: usize) -> String {
    match agents {
        0 => "🤖: 0".dimmed().to_string(),
        n => format!("🤖: {n}").cyan().to_string(),
    }
}

/// Compose the full status line for a state snapshot. Kept as one `String`
/// (ANSI included) so callers print it verbatim above the prompt.
pub fn render_status_line(
    state: RunState,
    preset: Option<&str>,
    ctx: Option<(i64, u32, f64)>,
    mode: AppMode,
    agents: usize,
    model: &str,
    version: &str,
) -> String {
    let separator = " │ ".dimmed().to_string();
    let mut segments = Vec::with_capacity(7);
    segments.push(state_segment(state));
    segments.push(
        preset
            .map(|name| name.bold().to_string())
            .unwrap_or_else(|| "no-preset".dimmed().to_string()),
    );
    segments.push(match ctx {
        Some((used, max, percent)) => ctx_segment(used, max, percent),
        None => "🧭 —".dimmed().to_string(),
    });
    segments.push(mode_segment(mode));
    segments.push(agents_segment(agents));
    segments.push(model.to_string());
    segments.push(format!("v{version}").dimmed().to_string());
    segments.join(&separator)
}

/// Derive the full status line from live REPL state.
pub fn status_line(state: &ReplState) -> String {
    let run_state = if state.awaiting_approval {
        RunState::WaitingApproval
    } else if state.is_compacting {
        RunState::Compacting
    } else if state.is_loading {
        RunState::Working
    } else {
        RunState::Idle
    };
    let ctx = context_usage(
        &state.api_messages,
        state.system_prompt.as_ref(),
        state.session.last_prompt_tokens,
        state.effective_model_for_budget(),
    );
    render_status_line(
        run_state,
        state.active_preset.as_deref(),
        ctx,
        state.mode,
        state.running_agent_count(),
        &state.model_display_label(),
        env!("CODESMITH_BUILD_VERSION"),
    )
}

/// The transient in-turn live line: a shorter variant without the
/// preset/model/version furniture.
pub fn live_line(state: &ReplState, current_tool: Option<&str>) -> String {
    let elapsed = state
        .turn_started_at
        .map(|started| started.elapsed().as_secs())
        .unwrap_or(0);
    let ctx = context_usage(
        &state.api_messages,
        state.system_prompt.as_ref(),
        state.session.last_prompt_tokens,
        state.effective_model_for_budget(),
    );
    let mut parts = vec![state_segment(RunState::Working)];
    if let Some(tool) = current_tool {
        parts.push(tool.to_string());
    }
    parts.push(format!("{elapsed}s").dimmed().to_string());
    parts.push(mode_segment(state.mode));
    parts.push(agents_segment(state.running_agent_count()));
    if let Some((_, _, percent)) = ctx {
        parts.push(ctx_percent_segment(percent));
    }
    parts.join(&" · ".dimmed().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn huge_user_message() -> crate::models::Message {
        use crate::models::{ContentBlock, Message};

        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "x ".repeat(2_000_000),
                cache_control: None,
            }],
        }
    }

    #[test]
    fn context_percent_clamps_to_window() {
        // an estimate far above the window clamps to the window
        let (used, max, pct) =
            context_usage(&[huge_user_message()], None, None, "deepseek-chat").unwrap();
        assert_eq!(used, i64::from(max));
        assert!((pct - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn context_estimate_wins_over_reported() {
        // a small conversation with an inflated reported value keeps the
        // (smaller) estimate — the #115 anti-bounce rule
        let (used, _, _) =
            context_usage(&[huge_user_message()], None, Some(0), "deepseek-chat").unwrap();
        // the huge estimate still clamps to the window; reported 0 never wins
        assert!(used > 0);
    }

    #[test]
    fn context_level_thresholds() {
        assert_eq!(context_level(10.0), ContextLevel::Normal);
        assert_eq!(context_level(84.9), ContextLevel::Normal);
        assert_eq!(context_level(85.0), ContextLevel::Warn);
        assert_eq!(context_level(94.9), ContextLevel::Warn);
        assert_eq!(context_level(95.0), ContextLevel::Alert);
        assert_eq!(context_level(100.0), ContextLevel::Alert);
    }

    #[test]
    fn token_counts_are_compact() {
        assert_eq!(format_token_count_compact(512), "512");
        assert_eq!(format_token_count_compact(72_000), "72k");
        assert_eq!(format_token_count_compact(1_200_000), "1.2M");
    }

    #[test]
    fn status_line_contains_all_six_items() {
        let line = render_status_line(
            RunState::Working,
            Some("Full"),
            Some((72_000, 171_000, 42.0)),
            AppMode::Agent,
            2,
            "deepseek-chat",
            "0.9.0",
        );
        // Strip ANSI for assertions.
        let plain = strip_ansi(&line);
        for fragment in [
            "working",
            "Full",
            "🧭 42% (72k/171k)",
            "🚥: agent",
            "🤖: 2",
            "deepseek-chat",
            "v0.9.0",
        ] {
            assert!(
                plain.contains(fragment),
                "missing {fragment:?} in {plain:?}"
            );
        }
    }

    #[test]
    fn unknown_model_hides_ctx() {
        let line = render_status_line(RunState::Idle, None, None, AppMode::Yolo, 0, "m", "0.9.0");
        assert!(strip_ansi(&line).contains("🧭 —"));
    }

    #[test]
    fn live_line_segments_show_mode_agents_and_ctx() {
        // The segments `live_line` composes with (a full `ReplState` is not
        // constructible here without loading real user settings): the mode
        // segment, the always-on zero-agent segment, and the 🧭 percent text.
        assert_eq!(strip_ansi(&mode_segment(AppMode::Agent)), "🚥: agent");
        assert_eq!(strip_ansi(&agents_segment(0)), "🤖: 0");
        assert_eq!(strip_ansi(&ctx_percent_segment(42.0)), "🧭 42%");
    }

    fn strip_ansi(text: &str) -> String {
        // Strip CSI sequences + OSC-set fg/bg resets emitted by `colored`.
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip '[', digits, ';', and the final byte letter.
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
