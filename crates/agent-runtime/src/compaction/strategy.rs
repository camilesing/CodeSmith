//! Compaction-strategy seam — script-ownable *decisions*, host-owned
//! *effects* (docs/MODS.md "Writing a compaction strategy").
//!
//! A mod registers a strategy with `register_compaction_strategy(name, #{
//! trigger, plan })`; config `[compaction] strategy = "<name>"` selects it
//! (`"builtin"` — the default — keeps the compiled algorithm). The
//! strategy owns two pure decisions over a host-built aggregate snapshot:
//!
//! - **trigger** — `|snapshot| -> bool`, when a compaction attempt should
//!   fire (the five-agent trigger formulas: reserve arithmetic, window
//!   ratios, …);
//! - **plan** — `|snapshot| -> #{ keep_recent_rounds | keep_recent_tokens,
//!   summary_instructions?, summary_max_tokens? }`, how much survives and
//!   how the summary is instructed.
//!
//! ## Immune zone (host-enforced, not strategy-ownable)
//!
//! Tool-call/result pairing (`enforce_tool_call_pairs`), the system-prompt
//! prefix (never compacted), append-only storage (`ChatHistory::
//! replace_all` with an audited reason), the summary LLM call and its
//! retries, the circuit breaker, the rapid-refill detector, and the
//! minimum-summarizable floor. A plan that names a boundary splitting a
//! pair is aligned by the host; a script error is logged loudly and the
//! attempt falls back to the builtin boundary — a strategy must never
//! wedge compaction (it is a safety mechanism).
//!
//! ## Known limitations (deliberate, v1)
//!
//! - The strategy governs the **auto-compaction path** (`run_compaction`).
//!   Capacity interventions (`capacity_flow`), the local-prune early-exit
//!   inside `compact_messages_safe`, and recovery compaction keep builtin
//!   thresholds in v1.
//! - The summary request *shape* (cache-aligned vs formatted) stays the
//!   host's; a strategy adjusts the instruction tail and the output cap
//!   only.
//! - `output_cap` is not in the snapshot (the per-model output ceiling is
//!   not threaded to the compaction probe); strategies that need it
//!   approximate with `window / 4` — the same cap minimax applies.

use std::sync::Arc;

use codesmith_agent::models::{ContentBlock, Message};
use codesmith_extensions::ExtensionRunner;
use serde_json::{Value, json};

use super::CompactionConfig;
use super::compact::{KEEP_RECENT_MESSAGES, MIN_SUMMARIZE_MESSAGES};

/// The config sentinel selecting the compiled builtin algorithm.
pub const BUILTIN_COMPACTION_STRATEGY: &str = "builtin";

/// What a strategy's plan decided to keep — interpreted by the host into
/// the `keep_recent` message count `plan_compaction` consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyKeep {
    /// Keep the last N messages (the builtin default).
    Messages(usize),
    /// Keep the last N assistant-start rounds (turn-granular retention).
    Rounds(usize),
    /// Backward-accumulate whole rounds until at least N tokens are kept
    /// (token-granular retention, aligned to a round boundary so pairing
    /// survives by construction).
    Tokens(usize),
}

/// Host-interpreted strategy directives for one compaction attempt.
#[derive(Debug, Clone)]
pub struct StrategyDirectives {
    /// The strategy's registered name (status messages + observability).
    pub name: String,
    pub keep: StrategyKeep,
    /// Extra instruction text appended to the summary prompt (e.g. a
    /// split-turn handoff checklist). `None` = the builtin instruction.
    pub summary_instructions: Option<String>,
    /// Output cap override for the summary request. `None` = the
    /// window-derived default.
    pub summary_max_tokens: Option<u32>,
}

impl StrategyDirectives {
    /// The fallback directives when a strategy's plan fails after its
    /// trigger fired: the builtin boundary, so the attempt still runs
    /// (the trigger already decided compaction is needed).
    #[must_use]
    pub fn builtin_fallback(name: &str) -> Self {
        Self {
            name: name.to_string(),
            keep: StrategyKeep::Messages(KEEP_RECENT_MESSAGES),
            summary_instructions: None,
            summary_max_tokens: None,
        }
    }
}

/// Cap `summary_instructions` — it rides the summary prompt verbatim, so a
/// runaway script must not bloat it.
const MAX_SUMMARY_INSTRUCTIONS_CHARS: usize = 16 * 1024;
/// Bounds for `summary_max_tokens` (clamped, not rejected — a strategy
/// asking for 16 KiB of summary gets exactly the omp-style absolute cap).
const MIN_SUMMARY_MAX_TOKENS: u32 = 256;
const MAX_SUMMARY_MAX_TOKENS: u32 = 65_536;

/// One assistant-start round: the assistant message plus every
/// user-role follow-up (tool results, steers) before the next assistant
/// message. Round boundaries are the pairing-safe cut points.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Round {
    /// Index of the round's first message.
    start: usize,
    /// Message count in the round.
    len: usize,
    tokens: usize,
    has_tool_use: bool,
    has_media: bool,
}

fn rounds_of(messages: &[Message]) -> Vec<Round> {
    let mut rounds: Vec<Round> = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        let starts_round = msg.role == "assistant" || rounds.is_empty();
        if starts_round {
            rounds.push(Round {
                start: idx,
                len: 0,
                tokens: 0,
                has_tool_use: false,
                has_media: false,
            });
        }
        let round = rounds.last_mut().expect("non-empty by construction");
        round.len += 1;
        round.tokens += super::estimate_tokens_for_message(msg, false);
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { .. } => round.has_tool_use = true,
                ContentBlock::Image { .. } => round.has_media = true,
                _ => {}
            }
        }
    }
    rounds
}

/// The aggregate snapshot handed to a strategy's closures. Token counts
/// are host-precomputed (the script engine has no tokenizer); rounds are
/// pre-aggregated so a plan over a long session stays inside the script
/// op budget.
#[must_use]
pub fn snapshot(messages: &[Message], config: &CompactionConfig) -> Value {
    let rounds = rounds_of(messages);
    let total_tokens: usize = rounds.iter().map(|r| r.tokens).sum();
    json!({
        "total_tokens": total_tokens,
        "window": config.context_window.unwrap_or(0),
        "threshold": config.token_threshold,
        "messages": messages.len(),
        "rounds": rounds.iter().map(|r| json!({
            "start": r.start,
            "len": r.len,
            "tokens": r.tokens,
            "has_tool_use": r.has_tool_use,
            "has_media": r.has_media,
        })).collect::<Vec<_>>(),
    })
}

/// Host-side trigger gates a strategy cannot bypass: the auto-compaction
/// floor and the minimum-summarizable message count. (`enabled` and the
/// circuit breaker are checked by the caller before the strategy is
/// consulted at all.)
#[must_use]
pub fn host_trigger_gates(messages: &[Message], config: &CompactionConfig) -> bool {
    if messages.len() < MIN_SUMMARIZE_MESSAGES {
        return false;
    }
    if config.auto_floor_tokens > 0 {
        let total: usize = messages
            .iter()
            .map(|m| super::estimate_tokens_for_message(m, false))
            .sum();
        if total < config.auto_floor_tokens {
            return false;
        }
    }
    true
}

/// Interpret a plan closure's returned map into [`StrategyDirectives`].
/// Validation is loud (every rejection names the field) but the caller
/// decides the fallback — `Err` here means "use [`StrategyDirectives::
/// builtin_fallback`]", not "abort compaction".
pub fn interpret_plan(value: Value, name: &str) -> Result<StrategyDirectives, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "plan must return a map (#{ ... })".to_string())?;
    let rounds = obj.get("keep_recent_rounds").and_then(Value::as_u64);
    let tokens = obj.get("keep_recent_tokens").and_then(Value::as_u64);
    let messages = obj.get("keep_recent_messages").and_then(Value::as_u64);
    let keep = match (rounds, tokens, messages) {
        (Some(n), None, None) if n > 0 => StrategyKeep::Rounds(n as usize),
        (None, Some(n), None) if n > 0 => StrategyKeep::Tokens(n as usize),
        (None, None, Some(n)) if n > 0 => StrategyKeep::Messages(n as usize),
        _ => {
            return Err("plan must set exactly one of keep_recent_rounds / \
                 keep_recent_tokens / keep_recent_messages (> 0)"
                .to_string());
        }
    };
    let summary_instructions = match obj.get("summary_instructions") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if s.chars().count() > MAX_SUMMARY_INSTRUCTIONS_CHARS {
                return Err(format!(
                    "summary_instructions exceeds {MAX_SUMMARY_INSTRUCTIONS_CHARS} chars"
                ));
            }
            Some(s.clone())
        }
        Some(_) => return Err("summary_instructions must be a string".to_string()),
    };
    let summary_max_tokens = match obj.get("summary_max_tokens") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let n = v
                .as_u64()
                .ok_or_else(|| "summary_max_tokens must be a non-negative integer".to_string())?;
            if n == 0 {
                return Err("summary_max_tokens must be > 0".to_string());
            }
            Some((n as u32).clamp(MIN_SUMMARY_MAX_TOKENS, MAX_SUMMARY_MAX_TOKENS))
        }
    };
    Ok(StrategyDirectives {
        name: name.to_string(),
        keep,
        summary_instructions,
        summary_max_tokens,
    })
}

/// Convert a keep spec into the `keep_recent` message count for
/// `plan_compaction` (which pins `len - keep_recent..len` as the tail
/// before `enforce_tool_call_pairs` runs — the immune zone holds either
/// way; round-aligned specs just make the boundary pairing-safe by
/// construction).
#[must_use]
pub fn keep_recent_messages_for(messages: &[Message], keep: &StrategyKeep) -> usize {
    match keep {
        StrategyKeep::Messages(n) => (*n).min(messages.len().max(1)),
        StrategyKeep::Rounds(n) => {
            let rounds = rounds_of(messages);
            rounds
                .iter()
                .rev()
                .take(*n)
                .map(|r| r.len)
                .sum::<usize>()
                .max(1)
        }
        StrategyKeep::Tokens(budget) => {
            let rounds = rounds_of(messages);
            let mut kept = 0usize;
            let mut kept_tokens = 0usize;
            for round in rounds.iter().rev() {
                kept += round.len;
                kept_tokens += round.tokens;
                if kept_tokens >= *budget {
                    break;
                }
            }
            kept.max(1)
        }
    }
}

/// The unified decision unit every compaction attempt runs through —
/// ONE call path in the host. "Which way compaction behaves" is decided
/// by *which compiled unit resolves*, never by branching at the call
/// site: the builtin algorithm ships as [`CompiledBuiltinDecisions`]
/// (compiled into the binary), a mod's `register_compaction_strategy`
/// lands as a load-time-compiled script unit, and both answer the same
/// two questions over the same inputs.
pub trait CompactionDecisions: Send + Sync {
    /// Status/observability label (the unit's name — "builtin" or the
    /// registered strategy name).
    fn label(&self) -> String;
    /// Whether this attempt should fire. Errors inside a script unit are
    /// the unit's own loud-then-fallback concern (`false` after logging).
    fn trigger(
        &self,
        snapshot: &Value,
        messages: &[Message],
        config: &CompactionConfig,
        workspace: Option<&std::path::Path>,
    ) -> bool;
    /// The boundary + summary directives for this attempt. Never fails:
    /// a rejected script plan falls back to the builtin boundary (the
    /// trigger already decided compaction is needed).
    fn plan(&self, snapshot: &Value) -> StrategyDirectives;
}

/// The builtin algorithm as a decision unit: the pre-seam formulas
/// (`should_compact`'s token gate, the `KEEP_RECENT_MESSAGES` tail).
/// Equivalence is pinned by test against those free functions, which
/// remain the other internal consumers' path (capacity, recovery).
pub struct CompiledBuiltinDecisions;

impl CompactionDecisions for CompiledBuiltinDecisions {
    fn label(&self) -> String {
        BUILTIN_COMPACTION_STRATEGY.to_string()
    }

    fn trigger(
        &self,
        _snapshot: &Value,
        messages: &[Message],
        config: &CompactionConfig,
        workspace: Option<&std::path::Path>,
    ) -> bool {
        super::should_compact(messages, config, workspace, None, None)
    }

    fn plan(&self, _snapshot: &Value) -> StrategyDirectives {
        StrategyDirectives::builtin_fallback(BUILTIN_COMPACTION_STRATEGY)
    }
}

/// A mod-registered strategy (`register_compaction_strategy`) wrapped in
/// the unified interface — the script closures are compiled at load;
/// invoking them here is the only thing that differs from builtin.
pub struct ScriptDecisions {
    strategy: codesmith_extensions::script::ScriptCompactionStrategy,
}

impl ScriptDecisions {
    #[must_use]
    pub fn new(strategy: codesmith_extensions::script::ScriptCompactionStrategy) -> Self {
        Self { strategy }
    }
}

impl CompactionDecisions for ScriptDecisions {
    fn label(&self) -> String {
        self.strategy.name.clone()
    }

    fn trigger(
        &self,
        snapshot: &Value,
        _messages: &[Message],
        _config: &CompactionConfig,
        _workspace: Option<&std::path::Path>,
    ) -> bool {
        self.strategy.call_trigger(snapshot).unwrap_or_else(|err| {
            tracing::error!(
                target: "compaction",
                "strategy {:?} trigger failed: {err} — treating as not-triggered",
                self.strategy.name
            );
            false
        })
    }

    fn plan(&self, snapshot: &Value) -> StrategyDirectives {
        self.strategy
            .call_plan(snapshot)
            .and_then(|plan| interpret_plan(plan, &self.strategy.name))
            .unwrap_or_else(|err| {
                tracing::warn!(
                    target: "compaction",
                    "strategy {:?} plan rejected ({err}) — using the builtin boundary",
                    self.strategy.name
                );
                StrategyDirectives::builtin_fallback(&self.strategy.name)
            })
    }
}

/// Resolve the configured strategy into ONE decision unit — the single
/// construction-time dispatch (never a call-site branch). `"builtin"` /
/// empty / no mod system / an unregistered name all resolve to the
/// compiled builtin unit; an unregistered name is a loud per-attempt
/// error (logged here) first — a config typo must not wedge compaction.
/// The strategy set is session state (mods load/reload around config),
/// so unlike `turn_executor` this cannot be validated at config-load
/// time.
#[must_use]
pub fn resolve_configured(
    runner: Option<&Arc<ExtensionRunner>>,
    config: &CompactionConfig,
) -> Arc<dyn CompactionDecisions> {
    let compiled = || Arc::new(CompiledBuiltinDecisions) as Arc<dyn CompactionDecisions>;
    if config.strategy == BUILTIN_COMPACTION_STRATEGY || config.strategy.is_empty() {
        return compiled();
    }
    let Some(runner) = runner else {
        tracing::error!(
            target: "compaction",
            "compaction strategy {:?} selected but no extension runtime is loaded — \
             using the builtin algorithm",
            config.strategy
        );
        return compiled();
    };
    match runner.compaction_strategy(&config.strategy) {
        Some(strategy) => Arc::new(ScriptDecisions::new(strategy)),
        None => {
            tracing::error!(
                target: "compaction",
                "compaction strategy {:?} is not registered by any loaded mod \
                 (registered: {:?}) — using the builtin algorithm. Check \
                 [compaction] strategy and /mods status.",
                config.strategy,
                runner.compaction_strategy_names()
            );
            compiled()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compiled builtin unit IS the pre-seam formulas — pinned by
    /// running both over a trigger matrix. `should_compact` stays the
    /// other internal consumers' path (capacity, recovery), so drift
    /// between the two would split the engine's compaction story.
    #[test]
    fn compiled_builtin_decisions_match_the_rust_formulas() {
        let unit = CompiledBuiltinDecisions;
        let configs = [
            CompactionConfig {
                token_threshold: 100,
                auto_floor_tokens: 0,
                ..CompactionConfig::default()
            },
            CompactionConfig {
                token_threshold: 50,
                auto_floor_tokens: 1_000_000,
                ..CompactionConfig::default()
            },
            CompactionConfig::default(),
        ];
        for config in &configs {
            for rounds in [0usize, 1, 3, 8, 20] {
                let messages = history(rounds);
                let snap = snapshot(&messages, config);
                assert_eq!(
                    unit.trigger(&snap, &messages, config, None),
                    super::super::should_compact(&messages, config, None, None, None),
                    "trigger equivalence at {rounds} rounds / threshold {}",
                    config.token_threshold,
                );
            }
        }
        // The compiled unit's plan is the builtin tail.
        let directives = unit.plan(&serde_json::json!({}));
        assert_eq!(
            directives.keep,
            StrategyKeep::Messages(super::KEEP_RECENT_MESSAGES)
        );
        assert_eq!(unit.label(), BUILTIN_COMPACTION_STRATEGY);
    }

    fn user_msg(text: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn assistant_msg(text: &str) -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    /// u/a pairs × n — each assistant message starts a round.
    fn history(rounds: usize) -> Vec<Message> {
        let mut messages = vec![user_msg("task instruction")];
        for i in 0..rounds {
            messages.push(assistant_msg(&format!(
                "assistant turn {i} with some tokens"
            )));
            messages.push(user_msg(&format!("tool result {i}")));
        }
        messages
    }

    #[test]
    fn rounds_group_on_assistant_starts() {
        let messages = history(3);
        let rounds = rounds_of(&messages);
        // Leading user message forms round 0, then one round per assistant.
        assert_eq!(rounds.len(), 4);
        assert_eq!(rounds[0].len, 1);
        assert_eq!(rounds[1].start, 1);
        assert_eq!(rounds.iter().map(|r| r.len).sum::<usize>(), messages.len());
    }

    #[test]
    fn snapshot_carries_totals_and_window() {
        let messages = history(2);
        let config = CompactionConfig {
            context_window: Some(200_000),
            token_threshold: 166_000,
            ..CompactionConfig::default()
        };
        let snap = snapshot(&messages, &config);
        assert_eq!(snap["window"], json!(200_000));
        assert_eq!(snap["threshold"], json!(166_000));
        assert_eq!(snap["messages"], json!(messages.len()));
        let rounds = snap["rounds"].as_array().expect("rounds array");
        let total: u64 = rounds.iter().map(|r| r["tokens"].as_u64().unwrap()).sum();
        assert!(total > 0);
    }

    #[test]
    fn interpret_plan_accepts_rounds_and_summary_knobs() {
        let directives = interpret_plan(
            json!({
                "keep_recent_rounds": 5,
                "summary_instructions": "**Turn Context (split turn):** …",
                "summary_max_tokens": 16_384,
            }),
            "compact-minimax",
        )
        .expect("valid plan");
        assert_eq!(directives.keep, StrategyKeep::Rounds(5));
        assert_eq!(directives.summary_max_tokens, Some(16_384));
        assert!(
            directives
                .summary_instructions
                .as_deref()
                .unwrap_or_default()
                .starts_with("**Turn Context")
        );
    }

    #[test]
    fn interpret_plan_rejects_conflicting_and_missing_keeps() {
        assert!(
            interpret_plan(
                json!({ "keep_recent_rounds": 5, "keep_recent_tokens": 500 }),
                "s"
            )
            .is_err()
        );
        assert!(interpret_plan(json!({}), "s").is_err());
        assert!(interpret_plan(json!({ "keep_recent_rounds": 0 }), "s").is_err());
        assert!(
            interpret_plan(
                json!({ "keep_recent_rounds": 3, "summary_max_tokens": 0 }),
                "s"
            )
            .is_err()
        );
    }

    #[test]
    fn interpret_plan_clamps_the_summary_cap() {
        let directives = interpret_plan(
            json!({ "keep_recent_tokens": 20_000, "summary_max_tokens": 1_000_000 }),
            "compact-pi",
        )
        .expect("valid plan");
        assert_eq!(directives.summary_max_tokens, Some(MAX_SUMMARY_MAX_TOKENS));
    }

    #[test]
    fn keep_tokens_backward_accumulates_whole_rounds() {
        let messages = history(4);
        let rounds = rounds_of(&messages);
        // A budget that only the last round satisfies still keeps ≥ 1 round.
        let last_round_tokens = rounds.last().unwrap().tokens;
        let kept =
            keep_recent_messages_for(&messages, &StrategyKeep::Tokens(last_round_tokens.max(1)));
        assert_eq!(kept, rounds.last().unwrap().len);
        // A budget larger than everything keeps the whole transcript.
        let kept_all = keep_recent_messages_for(&messages, &StrategyKeep::Tokens(usize::MAX / 2));
        assert_eq!(kept_all, messages.len());
    }

    #[test]
    fn keep_rounds_keeps_exactly_n_rounds() {
        let messages = history(5);
        let rounds = rounds_of(&messages);
        let expected: usize = rounds.iter().rev().take(2).map(|r| r.len).sum();
        assert_eq!(
            keep_recent_messages_for(&messages, &StrategyKeep::Rounds(2)),
            expected
        );
    }

    #[test]
    fn host_gates_enforce_floor_and_minimum_messages() {
        let mut config = CompactionConfig::default();
        assert!(!host_trigger_gates(&history(1), &config)); // < MIN_SUMMARIZE_MESSAGES
        config.auto_floor_tokens = 1_000_000;
        assert!(!host_trigger_gates(&history(10), &config)); // under floor
        config.auto_floor_tokens = 0;
        assert!(host_trigger_gates(&history(10), &config));
    }

    /// The four opt-in builtin strategies (Compare ports, sources compiled
    /// into the extensions crate) produce plans that pass the host's real
    /// interpretation — a port the host would reject is a binary defect,
    /// not a runtime surprise.
    #[test]
    fn builtin_opt_in_strategy_plans_pass_interpretation() {
        let ports: &[(&str, &str)] = &[
            (
                "compact-pi",
                include_str!("../../../extensions/src/script/builtin/compact_pi.rhai"),
            ),
            (
                "compact-omp",
                include_str!("../../../extensions/src/script/builtin/compact_omp.rhai"),
            ),
            (
                "compact-minimax",
                include_str!("../../../extensions/src/script/builtin/compact_minimax.rhai"),
            ),
            (
                "compact-dsh",
                include_str!("../../../extensions/src/script/builtin/compact_dsh.rhai"),
            ),
        ];
        let snapshot = json!({
            "total_tokens": 190_000, "window": 200_000, "threshold": 160_000,
            "messages": 40,
            "rounds": [
                {"start": 0, "len": 4, "tokens": 50_000, "has_tool_use": true, "has_media": false},
                {"start": 4, "len": 4, "tokens": 50_000, "has_tool_use": true, "has_media": false},
                {"start": 8, "len": 4, "tokens": 50_000, "has_tool_use": true, "has_media": false},
            ],
        });
        for (name, source) in ports {
            let m = codesmith_extensions::script::RhaiMod::load_source(
                name,
                name,
                "0.1.0",
                source.to_string(),
                codesmith_extensions::script::ModKvStore::ephemeral(),
                std::sync::Arc::new(codesmith_agent::extension::MessageProjectionHub::new()),
            )
            .unwrap_or_else(|e| panic!("{name} loads: {e}"));
            let strategy = m
                .compaction_strategy(name)
                .unwrap_or_else(|| panic!("{name} registered"));
            let plan = strategy
                .call_plan(&snapshot)
                .unwrap_or_else(|e| panic!("{name} plan: {e}"));
            let directives = interpret_plan(plan, name)
                .unwrap_or_else(|e| panic!("{name} passes host interpretation: {e}"));
            // The ported keep specs convert to a real message count.
            let messages = history(6);
            let keep = keep_recent_messages_for(&messages, &directives.keep);
            assert!(keep >= 1 && keep <= messages.len(), "{name}: keep {keep}");
        }
    }
}
