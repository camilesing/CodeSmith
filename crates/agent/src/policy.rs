//! Scriptable host-behavior policies — the "policy in script, effects in
//! host" seam.
//!
//! A policy is **data**, not code: a mod registers a config map for a
//! [`PolicyKind`] (via the `register_policy` native / `ExtensionApi::
//! register_policy`), the runner resolves registrations **last-wins by
//! load order** (builtin mods < global user mods < project mods — so a
//! user mod overrides a builtin by registering the same kind), and the
//! flushed values are published here, to the process-global
//! [`POLICIES`] snapshot the host's mechanism code reads at call time.
//!
//! What belongs on this seam and what does not:
//!
//! - **Policy** (registerable): knobs and decisions — sizes, thresholds,
//!   lists, formats, prompt templates. The agent can read and retune
//!   these per workspace without a recompile; that is the self-evolution
//!   surface.
//! - **Mechanism** (stays in Rust): algorithms and guards — diff
//!   computation, char-boundary slicing, spill writes, header escaping.
//!   Trust-boundary validation is never policy and never leaves the host.
//!
//! Known limitation: the snapshot is process-global — the CLI's
//! one-session-per-process model makes that exact; an embed driving two
//! engines in one process shares one policy set (last bind wins).

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use serde_json::Value;

/// Which host behavior a registered policy customizes.
///
/// `#[non_exhaustive]`: new kinds land with their consumer + builtin mod
/// in the same change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PolicyKind {
    /// Unified-diff rendering for file-edit tool results
    /// (`context_radius`, …) — the diff algorithm itself stays in Rust.
    OutputDiff,
    /// Shell-output truncation + summarization knobs (`max_output_size`,
    /// `head_bytes`, `tail_bytes`, …) — slicing/spill/summary-line
    /// detection stay in Rust.
    ShellOutput,
    /// Micro-compaction policy: trigger thresholds, the compactable-tool
    /// list, the cleared-result placeholder. The compaction engine (what
    /// to keep, ordering, retries) stays in Rust.
    CompactPolicy,
}

impl PolicyKind {
    /// Kebab-case policy name — the `register_policy(kind, …)` spelling
    /// mods use on the script side.
    #[must_use]
    pub fn kebab_name(self) -> &'static str {
        match self {
            Self::OutputDiff => "output-diff",
            Self::ShellOutput => "shell-output",
            Self::CompactPolicy => "compact-policy",
        }
    }

    /// Parse the kebab-case name (the script-side spelling). `None` for
    /// unknown names — load fails loud on the spot.
    #[must_use]
    pub fn from_kebab_name(name: &str) -> Option<Self> {
        match name {
            "output-diff" => Some(Self::OutputDiff),
            "shell-output" => Some(Self::ShellOutput),
            "compact-policy" => Some(Self::CompactPolicy),
            _ => None,
        }
    }

    /// Every defined kind, in declaration order (the builtin mod tier
    /// registers one entry per kind it owns).
    pub const ALL: &'static [PolicyKind] = &[
        PolicyKind::OutputDiff,
        PolicyKind::ShellOutput,
        PolicyKind::CompactPolicy,
    ];
}

type PolicyMap = HashMap<PolicyKind, Value>;

/// The process-global resolved policy snapshot. Seeded by the runner at
/// `bind_core`/replay time (last-wins by registration order); never
/// written by mechanism code.
fn policies() -> &'static RwLock<PolicyMap> {
    static POLICIES: OnceLock<RwLock<PolicyMap>> = OnceLock::new();
    POLICIES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Publish the resolved policy set (the runner's flush step: replaces the
/// whole snapshot — clear + set in one move so readers never observe a
/// half-applied generation).
pub fn publish_policies(resolved: HashMap<PolicyKind, Value>) {
    let mut guard = policies().write().unwrap_or_else(|e| e.into_inner());
    *guard = resolved;
}

/// Clear every published policy (the reload path's clear step; the next
/// flush republishes).
pub fn clear_published_policies() {
    let mut guard = policies().write().unwrap_or_else(|e| e.into_inner());
    guard.clear();
}

/// Whether anything is published (the runner-less embed path uses this to
/// decide whether to seed the builtin defaults once).
#[must_use]
pub fn any_policies_published() -> bool {
    !policies()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
}

/// Read one published policy value. `None` = no registration won for the
/// kind (callers fall back to their compiled defaults and warn once).
#[must_use]
pub fn published_policy(kind: PolicyKind) -> Option<Value> {
    policies()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&kind)
        .cloned()
}

/// Read a `usize` field of a published policy map with a default +
/// bounds. Bad or out-of-bounds values warn (once per read site) and fall
/// back — a mod bug must not brick the mechanism it tunes.
pub fn policy_usize(kind: PolicyKind, field: &str, default: usize, max: usize) -> usize {
    let Some(Value::Object(map)) = published_policy(kind) else {
        return default;
    };
    match map.get(field) {
        Some(Value::Number(n)) => match n.as_u64() {
            Some(v) if v as usize <= max => v as usize,
            Some(v) => {
                tracing::warn!(
                    target: "codesmith_policy",
                    "policy {} field {field} = {v} exceeds max {max}; using {default}",
                    kind.kebab_name()
                );
                default
            }
            None => {
                tracing::warn!(
                    target: "codesmith_policy",
                    "policy {} field {field} is not a non-negative integer; using {default}",
                    kind.kebab_name()
                );
                default
            }
        },
        Some(other) => {
            tracing::warn!(
                target: "codesmith_policy",
                "policy {} field {field} has non-numeric value {other}; using {default}",
                kind.kebab_name()
            );
            default
        }
        None => default,
    }
}

/// Read a `String` field of a published policy map with a default +
/// length cap (same warn-and-fall-back posture as [`policy_usize`]).
#[must_use]
pub fn policy_string(kind: PolicyKind, field: &str, default: &str, max_chars: usize) -> String {
    let Some(Value::Object(map)) = published_policy(kind) else {
        return default.to_string();
    };
    match map.get(field) {
        Some(Value::String(s)) if s.chars().count() <= max_chars => s.clone(),
        Some(Value::String(too_long)) => {
            tracing::warn!(
                target: "codesmith_policy",
                "policy {} field {field} ({} chars) exceeds {max_chars}; using default",
                kind.kebab_name(),
                too_long.chars().count()
            );
            default.to_string()
        }
        Some(other) => {
            tracing::warn!(
                target: "codesmith_policy",
                "policy {} field {field} has non-string value {other}; using default",
                kind.kebab_name()
            );
            default.to_string()
        }
        None => default.to_string(),
    }
}

/// Read a string-array field of a published policy map with a default +
/// entry cap. Non-string entries or a too-long list warn and fall back
/// to the default wholesale (a policy list is tuning, not protocol).
#[must_use]
pub fn policy_string_list(
    kind: PolicyKind,
    field: &str,
    default: &[&str],
    max_entries: usize,
) -> Vec<String> {
    let Some(Value::Object(map)) = published_policy(kind) else {
        return default.iter().map(|s| (*s).to_string()).collect();
    };
    match map.get(field) {
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) if out.len() < max_entries => out.push(s.clone()),
                    Value::String(_) => {}
                    other => {
                        tracing::warn!(
                            target: "codesmith_policy",
                            "policy {} field {field} has a non-string entry {other}; using default",
                            kind.kebab_name()
                        );
                        return default.iter().map(|s| (*s).to_string()).collect();
                    }
                }
            }
            if out.is_empty() {
                tracing::warn!(
                    target: "codesmith_policy",
                    "policy {} field {field} is empty; using default",
                    kind.kebab_name()
                );
                return default.iter().map(|s| (*s).to_string()).collect();
            }
            out
        }
        Some(other) => {
            tracing::warn!(
                target: "codesmith_policy",
                "policy {} field {field} is not an array ({other}); using default",
                kind.kebab_name()
            );
            default.iter().map(|s| (*s).to_string()).collect()
        }
        None => default.iter().map(|s| (*s).to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kind_names_round_trip() {
        for kind in PolicyKind::ALL {
            assert_eq!(PolicyKind::from_kebab_name(kind.kebab_name()), Some(*kind));
        }
        assert_eq!(PolicyKind::from_kebab_name("no-such"), None);
    }

    #[test]
    fn publish_then_clear_round_trips() {
        // The snapshot is process-global; this test is the only writer
        // among the policy tests (they run on separate threads — tolerate
        // a torn read by asserting only what this test published).
        let mut set = HashMap::new();
        set.insert(PolicyKind::ShellOutput, json!({"max_output_size": 123}));
        publish_policies(set);
        assert_eq!(
            published_policy(PolicyKind::ShellOutput),
            Some(json!({"max_output_size": 123}))
        );
        assert_eq!(
            policy_usize(PolicyKind::ShellOutput, "max_output_size", 9, 200),
            123
        );
        // Missing field → default.
        assert_eq!(policy_usize(PolicyKind::ShellOutput, "absent", 7, 9), 7);
        let mut set = HashMap::new();
        set.insert(PolicyKind::ShellOutput, json!({"max_output_size": 99999}));
        publish_policies(set);
        assert_eq!(
            policy_usize(PolicyKind::ShellOutput, "max_output_size", 9, 9),
            9
        );
        clear_published_policies();
        assert_eq!(published_policy(PolicyKind::ShellOutput), None);
        assert_eq!(
            policy_usize(PolicyKind::ShellOutput, "max_output_size", 9, 9),
            9
        );
    }
}
