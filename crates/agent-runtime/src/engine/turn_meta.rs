//! `<turn_meta>` block construction — host-agnostic free functions.
//!
//! Production wraps mid-turn injected messages (steer input, LSP diagnostics
//! flush) in a `<turn_meta>` block carrying the current date, the auto-routed
//! model / reasoning-effort (when auto-routing), the working-set summary, and
//! the matched conditional-skills block. The block is the first `ContentBlock`
//! of the `user` message so the model sees the context before the injected
//! text. Historically these lived as `&self Engine` methods reading
//! `self.session.working_set` + `self.config.*`; this module lifts the bodies
//! into free functions taking explicit parameters so two callers share one
//! source: the `Engine` wrapper methods (which lock the now-`Arc<Mutex>`
//! working_set and forward) and the framework-core `TurnMetaProbe` (which
//! holds its own `Arc` clone — see `host_executor.rs` slice 22 §E — so it can
//! enrich steer/LSP messages pushed *during* `executor.run` despite the
//! `&mut self.session` borrow held by `SessionChatHistory`).
//!
//! All three functions are `pub(crate)` — they are an intra-`engine` concern,
//! not part of the public `engine::` surface consumed by `codesmith-tui`.

use std::path::Path;

use crate::models::{ContentBlock, Message};
use crate::working_set::WorkingSet;

/// Render the matched conditional-skills block for the working set's top
/// paths. Mirrors the retired `Engine::conditional_skills_block`
/// (`mod.rs:931-973`). Returns `None` when the working set has no paths or no
/// skills match.
pub(crate) fn conditional_skills_block(
    working_set: &WorkingSet,
    workspace: &Path,
    skills_dir: &Path,
) -> Option<String> {
    let paths = working_set.top_paths(16);
    if paths.is_empty() {
        return None;
    }
    let registry = crate::skills::discover_for_workspace_and_dir(workspace, skills_dir);
    let matches = crate::skills::matching_conditional_skills(&registry, &paths);
    if matches.is_empty() {
        return None;
    }
    let mut lines = vec!["## Matched Conditional Skills".to_string()];
    for skill in matches.into_iter().take(6) {
        let reason = skill
            .when_to_use
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                let description = skill.description.trim();
                (!description.is_empty()).then_some(description)
            })
            .unwrap_or("");
        if reason.is_empty() {
            lines.push(format!(
                "- {} matched paths [{}]. Load with `load_skill` if relevant. Source: {}",
                skill.name,
                skill.paths.join(", "),
                skill.path.display()
            ));
        } else {
            lines.push(format!(
                "- {}: {} Matched paths [{}]. Load with `load_skill` if relevant. Source: {}",
                skill.name,
                reason,
                skill.paths.join(", "),
                skill.path.display()
            ));
        }
    }
    Some(lines.join("\n"))
}

/// Build the `<turn_meta>` `ContentBlock::Text`. Mirrors the retired
/// `Engine::turn_metadata_block` (`mod.rs:893-929`). Reads the working-set
/// summary + conditional skills at call time (faithful to production, which
/// re-reads on every wrap so a just-observed steer's paths surface in the
/// next block).
pub(crate) fn turn_metadata_block(
    working_set: &WorkingSet,
    workspace: &Path,
    skills_dir: &Path,
    routed_model: &str,
    auto_model: bool,
    reasoning_effort: Option<&str>,
    reasoning_effort_auto: bool,
) -> ContentBlock {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let working_set_summary = working_set
        .summary_block(workspace)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let conditional_skills = conditional_skills_block(working_set, workspace, skills_dir);

    let mut lines = vec![format!("Current local date: {today}")];
    if auto_model {
        lines.push(format!("Auto model route: {routed_model}"));
    }
    if reasoning_effort_auto && let Some(reasoning_effort) = reasoning_effort {
        lines.push(format!("Auto reasoning effort: {reasoning_effort}"));
    }
    if let Some(working_set_summary) = working_set_summary {
        lines.push(working_set_summary);
    }
    if let Some(conditional_skills) = conditional_skills {
        lines.push(conditional_skills);
    }
    let summary = lines.join("\n");

    ContentBlock::Text {
        text: format!("<turn_meta>\n{summary}\n</turn_meta>"),
        cache_control: None,
    }
}

/// Build a `user` message whose first content block is the `<turn_meta>`
/// block and whose second is the raw `text`. Mirrors the retired
/// `Engine::user_text_message_with_turn_metadata_for_route`
/// (`mod.rs:985-1008`).
pub(crate) fn user_text_message_with_turn_metadata(
    working_set: &WorkingSet,
    workspace: &Path,
    skills_dir: &Path,
    text: String,
    routed_model: &str,
    auto_model: bool,
    reasoning_effort: Option<&str>,
    reasoning_effort_auto: bool,
) -> Message {
    Message {
        role: "user".to_string(),
        content: vec![
            turn_metadata_block(
                working_set,
                workspace,
                skills_dir,
                routed_model,
                auto_model,
                reasoning_effort,
                reasoning_effort_auto,
            ),
            ContentBlock::Text {
                text,
                cache_control: None,
            },
        ],
    }
}
