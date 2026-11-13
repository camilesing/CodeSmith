//! Micro-compaction: lightweight tool-result clearing without API calls.
//!
//! Clears content from old tool results (file reads, shell output, grep, etc.)
//! while preserving message structure. Triggered by time gaps (>=60 min since
//! last assistant message) or byte thresholds. Not triggered in subagents.

use std::time::Instant;

use crate::models::{ContentBlock, Message};

// Tool names whose results can be micro-compacted (content cleared).
// The knobs below moved to the scriptable policy seam in 0.11.0 (builtin
// `compact-policy` mod → `register_policy("compact-policy", #{...})`,
// pre-0.11.0 values as compiled fallbacks). The engine (what to keep,
// ordering, retries) stays here.

/// Compiled fallback for the compactable-tool list (see `policy_read`).
const DEFAULT_MICRO_COMPACT_TOOL_NAMES: &[&str] = &[
    "file_read",
    "exec_shell",
    "exec_shell_wait",
    "exec_shell_interact",
    "grep",
    "glob",
    "web_search",
    "web_fetch",
    "write_file",
    "apply_patch",
    "edit_file",
];

/// Compiled fallback for the placeholder text.
const DEFAULT_CLEARED_PLACEHOLDER: &str = "[tool result cleared for context economy]";
/// Compiled fallback for the time trigger (60-minute threshold).
const DEFAULT_TIME_TRIGGER_GAP_SECS: u64 = 3600;

/// The effective compact-policy knobs (published policy → compiled
/// fallbacks). Cached per call — callers read fields directly.
pub(crate) fn policy_read() -> CompactPolicyKnobs {
    CompactPolicyKnobs {
        cache_trigger_threshold: codesmith_agent::policy::policy_usize(
            codesmith_agent::policy::PolicyKind::CompactPolicy,
            "cache_trigger_threshold",
            32 * 1024,
            10 * 1024 * 1024,
        ),
        time_trigger_gap_secs: codesmith_agent::policy::policy_usize(
            codesmith_agent::policy::PolicyKind::CompactPolicy,
            "time_trigger_gap_secs",
            DEFAULT_TIME_TRIGGER_GAP_SECS as usize,
            30 * 24 * 3600,
        ) as u64,
        micro_compactable_tools: codesmith_agent::policy::policy_string_list(
            codesmith_agent::policy::PolicyKind::CompactPolicy,
            "micro_compactable_tools",
            DEFAULT_MICRO_COMPACT_TOOL_NAMES,
            64,
        ),
        cleared_placeholder: codesmith_agent::policy::policy_string(
            codesmith_agent::policy::PolicyKind::CompactPolicy,
            "cleared_placeholder",
            DEFAULT_CLEARED_PLACEHOLDER,
            512,
        ),
        keep_head_chars: codesmith_agent::policy::policy_usize(
            codesmith_agent::policy::PolicyKind::CompactPolicy,
            "keep_head_chars",
            0,
            64 * 1024,
        ),
        keep_tail_chars: codesmith_agent::policy::policy_usize(
            codesmith_agent::policy::PolicyKind::CompactPolicy,
            "keep_tail_chars",
            0,
            64 * 1024,
        ),
    }
}

/// The compact-policy knobs (see `policy_read`).
pub(crate) struct CompactPolicyKnobs {
    pub cache_trigger_threshold: usize,
    pub time_trigger_gap_secs: u64,
    pub micro_compactable_tools: Vec<String>,
    pub cleared_placeholder: String,
    /// dsh-style partial clear (decision point 3, "sweep before move"):
    /// when > 0, a cleared result keeps its head/tail around the
    /// placeholder instead of collapsing to the placeholder alone —
    /// errors live at the tail of a shell log, hits at the head of a
    /// search. `0` (the default) keeps the bare-placeholder behavior.
    /// These parameterize the compiled builtin sweep rule; script rules
    /// (`register_sweep_rule`) carry their own replacement parameters.
    pub keep_head_chars: usize,
    pub keep_tail_chars: usize,
}

/// Per-session micro-compaction state.
#[derive(Debug, Clone)]
pub struct MicroCompactState {
    /// Timestamp of the last assistant message in the session.
    /// Used for time-based trigger (>=60 min gap).
    pub last_assistant_message_at: Option<Instant>,
    /// Bytes cleared by the most recent micro-compaction pass. Reset at the
    /// end of every pass — the old never-reset accumulator crossed the
    /// trigger threshold once and latched it for the session's lifetime,
    /// re-triggering compaction on every turn with nothing left to clear.
    pub bytes_cleared: usize,
    /// Byte threshold for cache-trigger micro-compaction.
    pub cache_trigger_threshold: usize,
    /// tool_use_ids already cleared by a previous pass. Cleared-ness is
    /// tracked by identity, not by comparing against the current
    /// placeholder text: the placeholder is policy-controlled and can
    /// change between passes, and string comparison under placeholder
    /// drift re-cleared (and double-counted) already-cleared results.
    pub cleared_tool_ids: std::collections::HashSet<String>,
}

impl Default for MicroCompactState {
    fn default() -> Self {
        Self {
            last_assistant_message_at: None,
            bytes_cleared: 0,
            // Policy-seeded default (builtin compact-policy mod; the
            // compiled fallback matches — see policy_read).
            cache_trigger_threshold: policy_read().cache_trigger_threshold,
            cleared_tool_ids: std::collections::HashSet::new(),
        }
    }
}

/// Check whether a tool name is micro-compactable.
pub fn is_micro_compactable_tool(name: &str) -> bool {
    policy_read()
        .micro_compactable_tools
        .iter()
        .any(|t| t == name)
}

// === sweep rules: the "sweep before move" dispatch =========================
//
// WHICH old results a pass may clear is compiled code, not a host branch:
// a mod's `register_sweep_rule` closures (load-time compiled, resolved via
// `ExtensionRunner::sweep_rules`) come first in load order, and the
// compiled builtin rule rides last as the fallback. The host owns only the
// clearing primitive below (replacement execution, byte accounting,
// idempotence, pairing) and the time/byte trigger cadence. Loading a rule
// IS the behavior switch — no config knob gates it in Rust.

/// The host-built view a sweep rule's `eligible` closure sees — pure data,
/// token/char counts precomputed, `content_head` capped so a script never
/// touches a full payload.
pub struct SweepView<'a> {
    pub tool: &'a str,
    pub is_error: bool,
    pub has_media: bool,
    pub content_chars: usize,
    /// Lowercased head of the content (≤ 512 chars) — enough for an
    /// eligibility judgment (error-marker scans), never the whole body.
    pub content_head_lower: String,
    /// Inside the live round (at/after the last assistant message)?
    pub in_live_round: bool,
}

impl SweepView<'_> {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "tool": self.tool,
            "is_error": self.is_error,
            "has_media": self.has_media,
            "content_chars": self.content_chars,
            "content_head": self.content_head_lower,
            "in_live_round": self.in_live_round,
        })
    }
}

/// One resolved sweep rule — an eligibility callable (compiled builtin
/// predicate or a load-time-compiled script closure) plus its replacement
/// parameters.
pub struct ResolvedSweepRule {
    /// Observability label ("builtin" or "<mod>/<rule>").
    pub label: String,
    eligible: Box<dyn Fn(&SweepView) -> bool + Send + Sync>,
    pub placeholder: String,
    pub keep_head_chars: usize,
    pub keep_tail_chars: usize,
}

impl ResolvedSweepRule {
    #[must_use]
    pub fn eligible(&self, view: &SweepView) -> bool {
        (self.eligible)(view)
    }
}

/// The compiled builtin rule: eligibility is the compactable-tool list
/// (policy-owned), replacement parameters the compact-policy knobs.
fn compiled_builtin_rule() -> ResolvedSweepRule {
    let knobs = policy_read();
    ResolvedSweepRule {
        label: "builtin".to_string(),
        eligible: Box::new(|view| is_micro_compactable_tool(view.tool)),
        placeholder: knobs.cleared_placeholder,
        keep_head_chars: knobs.keep_head_chars,
        keep_tail_chars: knobs.keep_tail_chars,
    }
}

/// The rule set a micro-compaction pass runs against — the single
/// dispatch surface. Script rules (in load order) claim first; the
/// compiled builtin rule always answers last, so behavior without mods is
/// byte-for-byte the pre-seam pass.
pub struct SweepRules {
    script_rules: Vec<ResolvedSweepRule>,
    builtin_rule: ResolvedSweepRule,
}

impl Default for SweepRules {
    fn default() -> Self {
        Self::compiled()
    }
}

impl SweepRules {
    /// Resolve from the loaded mods (their compiled `eligible` closures)
    /// plus the compiled builtin fallback. Constructed per pass, so a
    /// `/mods reload` takes effect on the next pass.
    #[must_use]
    pub fn from_runner(
        runner: Option<&std::sync::Arc<codesmith_extensions::ExtensionRunner>>,
    ) -> Self {
        let script_rules = runner
            .map(|r| r.sweep_rules())
            .unwrap_or_default()
            .into_iter()
            .map(|rule| {
                let label = format!("{}/{}", rule.mod_id, rule.name);
                let name = rule.name.clone();
                let placeholder = rule.placeholder.clone();
                let keep_head_chars = rule.keep_head_chars;
                let keep_tail_chars = rule.keep_tail_chars;
                ResolvedSweepRule {
                    label,
                    eligible: Box::new(move |view: &SweepView| {
                        rule.eligible(&view.to_json()).unwrap_or_else(|err| {
                            tracing::error!(
                                target: "compaction",
                                "sweep rule {name:?} eligible failed: {err} — treating as not-eligible"
                            );
                            false
                        })
                    }),
                    placeholder,
                    keep_head_chars,
                    keep_tail_chars,
                }
            })
            .collect();
        Self {
            script_rules,
            builtin_rule: compiled_builtin_rule(),
        }
    }

    /// No mod system (embeds/tests): the compiled builtin rule alone.
    #[must_use]
    pub fn compiled() -> Self {
        Self {
            script_rules: Vec::new(),
            builtin_rule: compiled_builtin_rule(),
        }
    }

    /// First-match dispatch: script rules in load order, then the
    /// compiled builtin rule. `None` = nobody claims (leave intact).
    fn first_claim(&self, view: &SweepView) -> Option<&ResolvedSweepRule> {
        self.script_rules
            .iter()
            .chain(std::iter::once(&self.builtin_rule))
            .find(|rule| rule.eligible(view))
    }

    /// The script-rule trigger: any loaded rule claims some result in a
    /// COMPLETED round. This is how "load a rule" changes the pass's
    /// cadence too (omp's elision fires on presence, not on the byte
    /// threshold) — the builtin rule never participates here; its cadence
    /// stays the time/byte triggers below.
    fn any_script_rule_claims(&self, messages: &[Message]) -> bool {
        if self.script_rules.is_empty() {
            return false;
        }
        let tool_use_names = collect_tool_use_names(messages);
        let live_from = messages
            .iter()
            .rposition(|m| m.role == "assistant")
            .map_or(0, |idx| idx + 1);
        messages
            .iter()
            .enumerate()
            .take_while(|(idx, _)| idx < &live_from)
            .flat_map(|(_, msg)| msg.content.iter())
            .filter_map(|block| sweep_view_of(block, &tool_use_names, false))
            .any(|view| self.script_rules.iter().any(|rule| rule.eligible(&view)))
    }
}

/// Build the [`SweepView`] for one tool-result block (None for
/// non-results / unknown tools).
fn sweep_view_of<'a>(
    block: &'a ContentBlock,
    tool_use_names: &'a std::collections::HashMap<String, String>,
    in_live_round: bool,
) -> Option<SweepView<'a>> {
    let ContentBlock::ToolResult {
        tool_use_id,
        content,
        is_error,
        content_blocks,
        ..
    } = block
    else {
        return None;
    };
    let tool = tool_use_names.get(tool_use_id)?;
    let has_media = content_blocks.as_ref().is_some_and(|b| {
        b.iter()
            .any(|blk| blk.get("type").and_then(|v| v.as_str()) == Some("image"))
    });
    let content_head_lower: String = content
        .chars()
        .take(512)
        .flat_map(char::to_lowercase)
        .collect();
    Some(SweepView {
        tool,
        is_error: *is_error == Some(true),
        has_media,
        content_chars: content.chars().count(),
        content_head_lower,
        in_live_round,
    })
}

/// Determine whether micro-compaction should be triggered.
///
/// Triggers when:
/// 1. Time trigger: >=60 minutes since last assistant message
/// 2. Cache trigger: accumulated bytes_cleared exceeds threshold
/// 3. Enough compactable bytes to clear, OR any loaded sweep rule claims
///    a completed-round result (loading a rule switches the cadence — no
///    host knob gates it).
///
/// Does NOT trigger in subagents (is_subagent=true).
pub fn should_trigger_micro_compact(
    messages: &[Message],
    state: &MicroCompactState,
    is_subagent: bool,
    sweep: &SweepRules,
) -> bool {
    if is_subagent {
        return false;
    }

    // Time trigger: large gap since last assistant message means cache is stale.
    if let Some(last_at) = state.last_assistant_message_at
        && last_at.elapsed().as_secs() >= policy_read().time_trigger_gap_secs
    {
        return true;
    }

    // Cache trigger: accumulated tool result bytes exceed threshold.
    if state.bytes_cleared >= state.cache_trigger_threshold {
        return true;
    }

    // Check if there are enough micro-compactable tool results to warrant clearing.
    let compactable_count = count_compactable_tool_results(messages);
    compactable_count > 0 && estimate_compactable_bytes(messages) >= state.cache_trigger_threshold
        || sweep.any_script_rule_claims(messages)
}

/// Count how many tool results in messages are from micro-compactable tools.
fn count_compactable_tool_results(messages: &[Message]) -> usize {
    let tool_use_names = collect_tool_use_names(messages);

    messages
        .iter()
        .flat_map(|msg| msg.content.iter())
        .filter(|block| {
            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                tool_use_names
                    .get(tool_use_id)
                    .is_some_and(|name| is_micro_compactable_tool(name))
            } else {
                false
            }
        })
        .count()
}

/// Estimate total bytes in micro-compactable tool results.
fn estimate_compactable_bytes(messages: &[Message]) -> usize {
    let tool_use_names = collect_tool_use_names(messages);

    messages
        .iter()
        .flat_map(|msg| msg.content.iter())
        .filter_map(|block| {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                content_blocks,
                ..
            } = block
            {
                if tool_use_names
                    .get(tool_use_id)
                    .is_some_and(|name| is_micro_compactable_tool(name))
                {
                    // The payload can live in `content_blocks` with `content`
                    // as a short fallback — size both, or large structured
                    // results look uncompactable here and under-trigger.
                    let blocks_len = content_blocks
                        .as_ref()
                        .and_then(|b| serde_json::to_string(b).ok())
                        .map_or(0, |s| s.len());
                    Some(content.len() + blocks_len)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .sum()
}

/// Build a map of tool_use_id -> tool_name from messages.
fn collect_tool_use_names(messages: &[Message]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for msg in messages {
        for block in &msg.content {
            if let ContentBlock::ToolUse { id, name, .. } = block {
                map.insert(id.clone(), name.clone());
            }
        }
    }
    map
}

/// The decide/execute split's decision half: everything a pass needs to
/// know about one candidate, computed under a read-only borrow —
/// `(tool_use_id, replacement, bytes_saved)`. `None` = leave intact.
fn sweep_decision_for(
    block: &ContentBlock,
    tool_use_names: &std::collections::HashMap<String, String>,
    in_live_round: bool,
    state: &MicroCompactState,
    builtin_placeholder: &str,
    sweep: &SweepRules,
) -> Option<(String, String, usize)> {
    let ContentBlock::ToolResult {
        tool_use_id,
        content,
        content_blocks,
        ..
    } = block
    else {
        return None;
    };
    // Identity check (not placeholder-text equality): the placeholder is
    // policy/rule-controlled; drift between passes used to make string
    // comparison re-clear and double-count already-cleared results.
    if state.cleared_tool_ids.contains(tool_use_id) || content == builtin_placeholder {
        return None;
    }
    let view = sweep_view_of(block, tool_use_names, in_live_round)?;
    // Size both payload holders: `content_blocks` can carry the real
    // payload while `content` is a short fallback, and a content-only
    // length skips clearing exactly those results.
    let blocks_len = content_blocks
        .as_ref()
        .and_then(|b| serde_json::to_string(b).ok())
        .map_or(0, |s| s.len());
    let original_len = content.len() + blocks_len;
    // ONE dispatch: the first claiming rule's replacement — the bare
    // placeholder, or (keep_head/keep_tail > 0) a dsh-style
    // head+placeholder+tail slice (errors live at a shell log's tail,
    // hits at a search's head, so the kept ends carry exactly what a
    // later turn re-reads for).
    let rule = sweep.first_claim(&view)?;
    let replacement = if rule.keep_head_chars > 0 || rule.keep_tail_chars > 0 {
        let head = super::compact::truncate_chars(content, rule.keep_head_chars).to_string();
        let tail = super::compact::tail_chars(content, rule.keep_tail_chars);
        if head.len() + tail.len() >= original_len {
            // Keeping both ends costs more than clearing saves.
            return None;
        }
        format!("{head}\n{}\n{tail}", rule.placeholder)
    } else {
        rule.placeholder.clone()
    };
    if original_len <= replacement.len() {
        return None; // Too small to benefit from clearing
    }
    let saved = original_len.saturating_sub(replacement.len());
    Some((tool_use_id.clone(), replacement, saved))
}

/// Apply micro-compaction to messages.
///
/// ONE dispatch per candidate: the resolved sweep rules (script rules in
/// load order, then the compiled builtin rule) decide eligibility and the
/// replacement parameters; this primitive executes the clear —
/// idempotence, byte accounting, pairing preservation. Preserves message
/// structure and tool_use/tool_result pairs.
///
/// Returns the number of bytes cleared by this pass.
pub fn micro_compact_messages(
    messages: &mut [Message],
    state: &mut MicroCompactState,
    sweep: &SweepRules,
) -> usize {
    let tool_use_names = collect_tool_use_names(messages);
    let builtin_placeholder = policy_read().cleared_placeholder;
    let live_from = messages
        .iter()
        .rposition(|m| m.role == "assistant")
        .map_or(0, |idx| idx + 1);
    let mut bytes_cleared = 0usize;

    for (msg_idx, msg) in messages.iter_mut().enumerate() {
        for block in msg.content.iter_mut() {
            // Decide (read-only borrow), then execute (mutable) — the
            // split keeps the borrow checker and the dispatch honest.
            let Some((tool_use_id, replacement, saved)) = sweep_decision_for(
                block,
                &tool_use_names,
                msg_idx >= live_from,
                state,
                &builtin_placeholder,
                sweep,
            ) else {
                continue;
            };
            bytes_cleared += saved;
            state.cleared_tool_ids.insert(tool_use_id);
            if let ContentBlock::ToolResult {
                content,
                content_blocks,
                ..
            } = block
            {
                *content = replacement;
                *content_blocks = None;
            }
        }
    }

    state.bytes_cleared = bytes_cleared;
    bytes_cleared
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The policy snapshot is process-global; tests in this module
    /// serialize through this guard (publish/assert/clear as one section).
    fn policy_test_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::policy_test_lock()
    }
    use serde_json::json;

    fn msg(role: &str, text: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn tool_use_msg(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
                caller: None,
            }],
        }
    }

    fn tool_result_msg(id: &str, content: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: None,
                content_blocks: None,
            }],
        }
    }

    #[test]
    fn trigger_does_not_latch_after_a_pass_and_drift_does_not_reclear() {
        let _guard = policy_test_lock();
        // One pass clears a big result; the counter reflects only that
        // pass (not a lifetime accumulator), and a placeholder change
        // between passes must not re-clear the already-cleared result.
        let mut messages = vec![
            tool_use_msg("c1", "file_read", json!({"path": "a"})),
            tool_result_msg("c1", &"x".repeat(64 * 1024)),
        ];
        let mut state = MicroCompactState {
            cache_trigger_threshold: 1024,
            ..MicroCompactState::default()
        };
        let first = micro_compact_messages(&mut messages, &mut state, &SweepRules::compiled());
        assert!(first > 0);
        assert_eq!(state.bytes_cleared, first);

        // Simulate placeholder drift: rewrite the cleared content to an
        // old-style placeholder text that no longer equals the current one.
        if let ContentBlock::ToolResult { content, .. } = &mut messages[1].content[0] {
            *content = "[cleared]".to_string();
        }
        let second = micro_compact_messages(&mut messages, &mut state, &SweepRules::compiled());
        assert_eq!(second, 0, "already-cleared id must not re-clear on drift");
        // bytes_cleared reflects the (empty) latest pass — the trigger
        // re-arms instead of latching.
        assert_eq!(state.bytes_cleared, 0);
        assert!(state.bytes_cleared < state.cache_trigger_threshold);
    }

    #[test]
    fn is_micro_compactable_tool_matches_known_tools() {
        let _guard = policy_test_lock();
        assert!(is_micro_compactable_tool("file_read"));
        assert!(is_micro_compactable_tool("exec_shell"));
        assert!(is_micro_compactable_tool("grep"));
        assert!(is_micro_compactable_tool("glob"));
        assert!(is_micro_compactable_tool("web_search"));
        assert!(is_micro_compactable_tool("web_fetch"));
        assert!(!is_micro_compactable_tool("agent_open"));
        assert!(!is_micro_compactable_tool("unknown_tool"));
    }

    #[test]
    fn should_trigger_returns_false_for_subagent() {
        let _guard = policy_test_lock();
        let state = MicroCompactState::default();
        let messages = vec![msg("user", "test")];
        assert!(!should_trigger_micro_compact(
            &messages,
            &state,
            true,
            &SweepRules::compiled()
        ));
    }

    /// A sweep set resolved from the opt-in `elide-uneventful` mod's
    /// compiled rule (the real port source), with the byte lane kept
    /// quiet so only the rule's presence-cadence can fire.
    fn elision_sweep() -> (SweepRules, MicroCompactState) {
        use codesmith_agent::policy::{PolicyKind, publish_policies};
        use std::collections::HashMap;
        let mut set = HashMap::new();
        set.insert(
            PolicyKind::CompactPolicy,
            serde_json::json!({
                "cache_trigger_threshold": 999_999, // keep the byte lane quiet
                "micro_compactable_tools": ["grep"],
            }),
        );
        publish_policies(set);
        let m = codesmith_extensions::script::RhaiMod::load_source(
            "elide-uneventful",
            "Elide Uneventful",
            "0.1.0",
            include_str!("../../../extensions/src/script/builtin/elide_uneventful.rhai")
                .to_string(),
            codesmith_extensions::script::ModKvStore::ephemeral(),
            std::sync::Arc::new(codesmith_agent::extension::MessageProjectionHub::new()),
        )
        .expect("elide-uneventful compiles");
        let runner = codesmith_extensions::ExtensionRunner::new();
        runner.retain_mod(std::sync::Arc::new(m));
        let rules = SweepRules::from_runner(Some(&std::sync::Arc::new(runner)));
        (rules, MicroCompactState::default())
    }

    /// omp-style elision cadence — the rule's compiled `eligible` closure
    /// decides: an uneventful completed-round result trips the trigger by
    /// presence; the live round and error-marker bodies never do; with
    /// the rule NOT loaded (compiled set), presence alone never trips.
    #[test]
    fn elision_rule_switches_the_trigger_by_presence() {
        let _guard = policy_test_lock();
        let (sweep, state) = elision_sweep();

        // Completed round: assistant tool_use → user result → assistant.
        let completed = vec![
            tool_use_msg("t1", "grep", serde_json::json!({})),
            tool_result_msg("t1", &"hit ".repeat(200)),
            msg("assistant", "done with that search"),
        ];
        assert!(
            should_trigger_micro_compact(&completed, &state, false, &sweep),
            "the loaded rule claims an uneventful completed-round result"
        );

        // Live round only (no assistant after the result) — stays intact.
        let live = vec![
            msg("assistant", "starting"),
            tool_use_msg("t2", "grep", serde_json::json!({})),
            tool_result_msg("t2", &"hit ".repeat(200)),
        ];
        assert!(
            !should_trigger_micro_compact(&live, &state, false, &sweep),
            "the live round is never an elision target"
        );

        // Error-marker bodies are never uneventful.
        let failing = vec![
            tool_use_msg("t3", "grep", serde_json::json!({})),
            tool_result_msg("t3", "error: pattern not found"),
            msg("assistant", "handled"),
        ];
        assert!(
            !should_trigger_micro_compact(&failing, &state, false, &sweep),
            "failure lines stay for later turns"
        );

        // Without the rule (the compiled set), presence alone never trips.
        assert!(
            !should_trigger_micro_compact(
                &completed,
                &MicroCompactState::default(),
                false,
                &SweepRules::compiled()
            ),
            "elision rides the loaded rule, not a host knob"
        );
        codesmith_agent::policy::clear_published_policies();
    }

    /// The elision clear dispatch: the script rule's placeholder claims
    /// the uneventful result; the error-marker result falls through to
    /// the compiled builtin rule's placeholder.
    #[test]
    fn elision_rule_claims_with_its_own_placeholder() {
        let _guard = policy_test_lock();
        let (sweep, _) = elision_sweep();

        let mut messages = vec![
            tool_use_msg("t1", "grep", serde_json::json!({})),
            tool_result_msg("t1", &"hit ".repeat(200)),
            tool_use_msg("t2", "grep", serde_json::json!({})),
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t2".into(),
                    content: format!("error: pattern invalid\n{}", "context ".repeat(40)),
                    is_error: Some(true),
                    content_blocks: None,
                }],
            },
            msg("assistant", "moving on"),
        ];
        let mut st = MicroCompactState::default();
        let cleared = micro_compact_messages(&mut messages, &mut st, &sweep);
        assert!(cleared > 0);
        let ContentBlock::ToolResult { content, .. } = &messages[1].content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(content, "[Uneventful result elided]");
        let ContentBlock::ToolResult { content, .. } = &messages[3].content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(content, "[tool result cleared for context economy]");

        codesmith_agent::policy::clear_published_policies();
    }

    #[test]
    fn should_trigger_time_based() {
        let _guard = policy_test_lock();
        let mut state = MicroCompactState::default();
        // Simulate 61 minutes since last assistant message. Subtracting a
        // large duration from `Instant::now()` panics when the monotonic
        // clock is younger than that gap — freshly booted CI VMs (Windows
        // runners boot per job) hit exactly this — so skip when
        // unrepresentable instead of panicking.
        let Some(stale_at) = Instant::now().checked_sub(std::time::Duration::from_secs(
            DEFAULT_TIME_TRIGGER_GAP_SECS + 60,
        )) else {
            eprintln!("monotonic clock younger than simulated gap; skipping");
            return;
        };
        state.last_assistant_message_at = Some(stale_at);
        let messages = vec![msg("user", "test")];
        assert!(should_trigger_micro_compact(
            &messages,
            &state,
            false,
            &SweepRules::compiled()
        ));
    }

    #[test]
    fn micro_compact_clears_compactable_tool_results() {
        let _guard = policy_test_lock();
        let mut messages = vec![
            tool_use_msg("call-1", "file_read", json!({"path": "Cargo.toml"})),
            tool_result_msg("call-1", &"x".repeat(500)),
            msg("user", "question"),
            msg("assistant", "answer"),
        ];
        let mut state = MicroCompactState::default();

        let cleared = micro_compact_messages(&mut messages, &mut state, &SweepRules::compiled());

        assert!(cleared > 0);
        let ContentBlock::ToolResult { content, .. } = &messages[1].content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(content, DEFAULT_CLEARED_PLACEHOLDER);
    }

    #[test]
    fn micro_compact_preserves_non_compactable_results() {
        let _guard = policy_test_lock();
        let mut messages = vec![
            tool_use_msg("call-1", "agent_open", json!({"prompt": "test"})),
            tool_result_msg("call-1", "agent response content"),
            msg("user", "question"),
        ];
        let mut state = MicroCompactState::default();

        let cleared = micro_compact_messages(&mut messages, &mut state, &SweepRules::compiled());
        assert_eq!(cleared, 0);

        let ContentBlock::ToolResult { content, .. } = &messages[1].content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(content, "agent response content");
    }

    #[test]
    fn micro_compact_skips_small_results() {
        let _guard = policy_test_lock();
        let mut messages = vec![
            tool_use_msg("call-1", "file_read", json!({"path": "a.rs"})),
            tool_result_msg("call-1", "ok"), // shorter than placeholder
            msg("user", "question"),
        ];
        let mut state = MicroCompactState::default();

        let cleared = micro_compact_messages(&mut messages, &mut state, &SweepRules::compiled());
        assert_eq!(cleared, 0);
    }

    #[test]
    fn micro_compact_skips_already_cleared() {
        let _guard = policy_test_lock();
        let mut messages = vec![
            tool_use_msg("call-1", "file_read", json!({"path": "a.rs"})),
            tool_result_msg("call-1", DEFAULT_CLEARED_PLACEHOLDER),
            msg("user", "question"),
        ];
        let mut state = MicroCompactState::default();

        let cleared = micro_compact_messages(&mut messages, &mut state, &SweepRules::compiled());
        assert_eq!(cleared, 0);
    }

    #[test]
    fn micro_compact_sees_content_blocks_payloads() {
        let _guard = policy_test_lock();
        // Short `content` + large structured `content_blocks`: the payload
        // lives in the blocks, so both clearing and the trigger estimate
        // must count them (a content-only length made this uncompactable).
        let mut messages = vec![
            tool_use_msg("b1", "file_read", json!({"path": "a.rs"})),
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "b1".into(),
                    content: "see blocks".into(),
                    is_error: None,
                    content_blocks: Some(vec![json!({"blob": "x".repeat(4096)})]),
                }],
            },
        ];
        assert!(
            estimate_compactable_bytes(&messages) >= 4096,
            "estimate must count content_blocks bytes"
        );

        let mut state = MicroCompactState::default();
        let cleared = micro_compact_messages(&mut messages, &mut state, &SweepRules::compiled());
        // 4KB of blocks minus the ~42-char placeholder still clears >4KB.
        assert!(cleared >= 4_000, "cleared bytes must include the blocks");
        let ContentBlock::ToolResult {
            content,
            content_blocks,
            ..
        } = &messages[1].content[0]
        else {
            panic!("expected tool result");
        };
        assert_eq!(content, DEFAULT_CLEARED_PLACEHOLDER);
        assert!(content_blocks.is_none());
    }

    #[test]
    fn policy_override_changes_placeholder_and_tool_set() {
        let _guard = policy_test_lock();
        // Publish an override, assert behavior follows, clear (the
        // snapshot is process-global — restore for the other tests).
        use codesmith_agent::policy::{PolicyKind, clear_published_policies, publish_policies};
        use std::collections::HashMap;
        let mut set = HashMap::new();
        set.insert(
            PolicyKind::CompactPolicy,
            serde_json::json!({
                "cache_trigger_threshold": 1,
                "micro_compactable_tools": ["custom_tool"],
                "cleared_placeholder": "[gone]",
            }),
        );
        publish_policies(set);

        assert!(is_micro_compactable_tool("custom_tool"));
        assert!(
            !is_micro_compactable_tool("exec_shell"),
            "default set replaced"
        );
        // Threshold 1 + any compactable bytes trips the cache trigger.
        let state = MicroCompactState::default();
        assert!(state.cache_trigger_threshold == 1);

        // A custom_tool result is cleared to the custom placeholder.
        let mut messages = vec![
            tool_use_msg("t1", "custom_tool", serde_json::json!({})),
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "x".repeat(100),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];
        let mut st = MicroCompactState::default();
        let cleared = micro_compact_messages(&mut messages, &mut st, &SweepRules::compiled());
        // The return is bytes cleared: 100 original − 6 for "[gone]".
        assert_eq!(cleared, 94, "custom_tool result compacted");
        let ContentBlock::ToolResult { content, .. } = &messages[1].content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(content, "[gone]");

        clear_published_policies();
    }

    /// dsh-style partial clear (decision point 3, "sweep before move"):
    /// with `keep_head_chars` / `keep_tail_chars` > 0, a cleared result
    /// keeps its head and tail around the placeholder — hits at the head
    /// of a search, errors at the tail of a shell log.
    #[test]
    fn keep_head_tail_keeps_the_around_placeholder() {
        let _guard = policy_test_lock();
        use codesmith_agent::policy::{PolicyKind, clear_published_policies, publish_policies};
        use std::collections::HashMap;
        let mut set = HashMap::new();
        set.insert(
            PolicyKind::CompactPolicy,
            serde_json::json!({
                "cache_trigger_threshold": 1,
                "micro_compactable_tools": ["grep"],
                "cleared_placeholder": "[cleared]",
                "keep_head_chars": 10,
                "keep_tail_chars": 5,
            }),
        );
        publish_policies(set);

        let body = format!("{}END", "x".repeat(100));
        let original_len = body.len();
        let mut messages = vec![
            tool_use_msg("t1", "grep", serde_json::json!({})),
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: body,
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];
        let mut st = MicroCompactState::default();
        let cleared = micro_compact_messages(&mut messages, &mut st, &SweepRules::compiled());
        let ContentBlock::ToolResult { content, .. } = &messages[1].content[0] else {
            panic!("expected tool result");
        };
        // head(10) + placeholder + tail(5), joined around the marker line.
        assert!(content.starts_with("xxxxxxxxxx"), "head kept: {content:?}");
        assert!(content.contains("[cleared]"), "marker rides: {content:?}");
        assert!(content.ends_with("xxEND"), "tail kept: {content:?}");
        // Bytes cleared is measured against the real replacement length.
        let replacement_len = 10 + 1 + "[cleared]".len() + 1 + 5;
        assert_eq!(cleared, original_len - replacement_len);

        // A result smaller than head+tail keeps everything (nothing to save).
        let mut small = vec![
            tool_use_msg("t2", "grep", serde_json::json!({})),
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t2".into(),
                    content: "short".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];
        let mut st2 = MicroCompactState::default();
        let cleared_small = micro_compact_messages(&mut small, &mut st2, &SweepRules::compiled());
        assert_eq!(cleared_small, 0, "too small to benefit");

        clear_published_policies();
    }
}
