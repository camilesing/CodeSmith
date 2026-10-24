//! Status line for the REPL.
//!
//! The prompt's first row — drawn above the input line, and refreshed as a
//! transient line during turns. Being part of the prompt is what lets the
//! mode switch land in place: rustyline redraws its prompt on every key, so
//! `Shift+Tab` relabels the `🚥` segment instead of printing a line and
//! re-prompting (see
//! [`Bindings::repainted_prompt`](super::bindings::Bindings::repainted_prompt)).
//! One line, seven data items plus a leading running-state indicator:
//!
//! ```text
//! ● working │ Full │ 🧭 42% (72k/171k) │ 🚥agent │ 🤖2 │ deepseek-chat │ 🧠max │ v0.9.0 (f83c330)
//! ```
//!
//! - state dot: idle / working / waiting-approval / compacting
//! - preset: the active runtime profile tier (simple | middle | all | ...)
//! - 🧭: session context-window occupancy (estimate-first, see
//!   [`context_usage`])
//! - 🚥: `AppMode`
//! - 🤖: running sub-agent count
//! - model: display label
//! - 🧠: reasoning-effort tier (off | low | med | high | max | auto)
//! - version: build version

use colored::Colorize;

use super::state::ReplState;
use crate::app_state::{AppMode, ReasoningEffort};
use crate::compaction::estimate_input_tokens_conservative;

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
///
/// `window` is caller-resolved — `ReplState::context_window` applies the
/// provider's `context_window` override and otherwise falls back to the static
/// model table. `None` means "no known window": the caller renders `🧭 —`.
pub fn context_usage(
    api_messages: &[crate::models::Message],
    system_prompt: Option<&crate::models::SystemPrompt>,
    last_prompt_tokens: Option<u32>,
    window: Option<u32>,
) -> Option<(i64, u32, f64)> {
    // A zero window (hand-edited `context_window = 0` reaches here verbatim)
    // is treated as unknown: dividing by it produced a NaN percent that
    // `clamp` propagated and every level comparison read as `Normal`,
    // masking the bad config instead of rendering `🧭 —`.
    let max = window.filter(|&w| w > 0)?;
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
pub fn mode_segment(mode: AppMode) -> String {
    format!("🚥{}", mode.label().to_lowercase())
}

/// The mode label's width, in terminal columns.
///
/// The `🚥` badge is the same in every mode, so a relabel moves the row by
/// the difference between two of these — the glyph cancels out. The labels
/// are ASCII, so their byte length is their width.
pub fn mode_label_columns(mode: AppMode) -> usize {
    mode.label().len()
}

/// The widest mode label the `Shift+Tab` ring can show.
///
/// The prompt's status row is padded to this width before rustyline sees it:
/// every label the ring can produce then occupies the same number of rows,
/// so the layout rustyline computed from the composed prompt stays valid
/// when the row is relabelled in place. Without the pad, a wider label on a
/// terminal where the row wraps would shift the cursor by a row.
pub fn widest_mode_label_columns(include_coordinator: bool) -> usize {
    let widest = if include_coordinator {
        AppMode::Coordinator
    } else {
        AppMode::Agent
    };
    mode_label_columns(widest)
}

/// The sub-agent count segment (dimmed at zero, cyan when running), shared
/// by the idle status line and the live line.
fn agents_segment(agents: usize) -> String {
    match agents {
        0 => "🤖0".dimmed().to_string(),
        n => format!("🤖{n}").cyan().to_string(),
    }
}

/// The reasoning-effort segment: the tier `/config reasoning_effort` writes
/// and the engine receives, one of `🧠off` / `🧠low` / `🧠med` / `🧠high` /
/// `🧠max` / `🧠auto`. Under `auto` routing the segment stays `🧠auto` instead
/// of tracking the per-turn pick; `/status` prints the resolved tier.
fn effort_segment(effort: ReasoningEffort) -> String {
    format!("🧠{}", effort.short_label())
}

/// Compose the full status line for a state snapshot. Kept as one `String`
/// (ANSI included) so callers print it verbatim above the prompt.
#[allow(clippy::too_many_arguments)]
pub fn render_status_line(
    state: RunState,
    preset: Option<&str>,
    ctx: Option<(i64, u32, f64)>,
    mode: AppMode,
    agents: usize,
    model: &str,
    effort: ReasoningEffort,
    version: &str,
) -> String {
    let separator = " │ ".dimmed().to_string();
    let mut segments = Vec::with_capacity(8);
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
    segments.push(effort_segment(effort));
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
        state.api_messages.as_slice(),
        state.system_prompt.as_ref(),
        state.session.last_prompt_tokens,
        state.context_window(),
    );
    let mut line = render_status_line(
        run_state,
        state.active_preset.as_deref(),
        ctx,
        state.mode,
        state.running_agent_count(),
        &state.model_display_label(),
        state.reasoning_effort,
        env!("CODESMITH_BUILD_VERSION"),
    );
    // `/statusline <command>`: the user's own segment, refreshed by the REPL
    // loop (this renderer stays synchronous and never spawns anything).
    if let Some(text) = state.statusline_text.as_deref() {
        line.push_str(&" │ ".dimmed().to_string());
        line.push_str(text);
    }
    line
}

/// The transient in-turn live line: a shorter variant without the
/// preset/model/version furniture.
pub fn live_line(state: &ReplState, current_tool: Option<&str>) -> String {
    let elapsed = state
        .turn_started_at
        .map(|started| started.elapsed().as_secs())
        .unwrap_or(0);
    let ctx = context_usage(
        state.api_messages.as_slice(),
        state.system_prompt.as_ref(),
        state.session.last_prompt_tokens,
        state.context_window(),
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
            context_usage(&[huge_user_message()], None, None, Some(128_000)).unwrap();
        assert_eq!(used, i64::from(max));
        assert!((pct - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn context_estimate_wins_over_reported() {
        // a small conversation with an inflated reported value keeps the
        // (smaller) estimate — the #115 anti-bounce rule
        let (used, _, _) =
            context_usage(&[huge_user_message()], None, Some(0), Some(128_000)).unwrap();
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
    fn status_line_contains_all_seven_items() {
        let line = render_status_line(
            RunState::Working,
            Some("Full"),
            Some((72_000, 171_000, 42.0)),
            AppMode::Agent,
            2,
            "deepseek-chat",
            ReasoningEffort::High,
            "0.9.0",
        );
        // Strip ANSI for assertions; the exact string pins the segment order.
        assert_eq!(
            strip_ansi(&line),
            "◉ working │ Full │ 🧭 42% (72k/171k) │ 🚥agent │ 🤖2 │ deepseek-chat │ 🧠high │ v0.9.0"
        );
    }

    #[test]
    fn unknown_model_hides_ctx() {
        let line = render_status_line(
            RunState::Idle,
            None,
            None,
            AppMode::Yolo,
            0,
            "m",
            ReasoningEffort::Max,
            "0.9.0",
        );
        assert!(strip_ansi(&line).contains("🧭 —"));
    }

    #[test]
    fn effort_segment_covers_all_six_tiers() {
        for (effort, expected) in [
            (ReasoningEffort::Off, "🧠off"),
            (ReasoningEffort::Low, "🧠low"),
            (ReasoningEffort::Medium, "🧠med"),
            (ReasoningEffort::High, "🧠high"),
            (ReasoningEffort::Max, "🧠max"),
            (ReasoningEffort::Auto, "🧠auto"),
        ] {
            assert_eq!(strip_ansi(&effort_segment(effort)), expected);
        }
    }

    #[test]
    fn live_line_segments_show_mode_agents_and_ctx() {
        // The segments `live_line` composes with (a full `ReplState` is not
        // constructible here without loading real user settings): the mode
        // segment, the always-on zero-agent segment, and the 🧭 percent text.
        assert_eq!(strip_ansi(&mode_segment(AppMode::Agent)), "🚥agent");
        assert_eq!(strip_ansi(&agents_segment(0)), "🤖0");
        assert_eq!(strip_ansi(&ctx_percent_segment(42.0)), "🧭 42%");
    }

    #[test]
    fn every_ring_label_fits_the_widest_one() {
        assert_eq!(mode_label_columns(AppMode::Agent), 5);
        assert_eq!(mode_label_columns(AppMode::Plan), 4);
        assert_eq!(mode_label_columns(AppMode::Yolo), 4);
        assert_eq!(mode_label_columns(AppMode::Coordinator), 11);
        assert_eq!(
            widest_mode_label_columns(false),
            5,
            "without the feature the ring stops at agent"
        );
        assert_eq!(widest_mode_label_columns(true), 11);
        for include_coordinator in [false, true] {
            let widest = widest_mode_label_columns(include_coordinator);
            for mode in [
                AppMode::Agent,
                AppMode::Plan,
                AppMode::Yolo,
                AppMode::Coordinator,
            ] {
                if !include_coordinator && mode == AppMode::Coordinator {
                    continue;
                }
                assert!(
                    mode_label_columns(mode) <= widest,
                    "{mode:?} is wider than the pad the prompt reserves"
                );
            }
        }
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
