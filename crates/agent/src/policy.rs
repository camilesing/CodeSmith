//! Scriptable host-behavior policies — the "policy in script, effects in
//! host" seam.
//!
//! A policy is **data**, not code: a mod registers a config map for a
//! [`PolicyKind`] (via the `register_policy` native / `ExtensionApi::
//! register_policy`), the runner resolves registrations **last-wins by
//! load order** (builtin mods < global user mods < project mods — so a
//! user mod overrides a builtin by registering the same kind), and the
//! flushed values are published to a [`PolicyStore`] the host's
//! mechanism code reads at call time.
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
//! ## Scoping
//!
//! The store is **engine-scoped**, not process-global (the
//! `retry_status` global→handle move, applied here): the `Session`
//! carries the store, `Engine::run` enters it as the ambient scope for
//! everything the turn awaits, and the runner publishes into it — so a
//! process hosting several engines reads each engine's own set instead
//! of one last-bind-wins snapshot. Outside a scope (tests, headless
//! utilities) reads resolve the **process-default store**
//! ([`PolicyStore::process_default`]); the single-engine CLI registers
//! its engine's store there, so its scope-less paths behave exactly as
//! the old process-wide snapshot did.
//!
//! Known limitations, written down beside the behaviour this owns:
//! detached tasks (`tokio::spawn`, `spawn_blocking`) do not inherit the
//! scope — they read the process default — and a multi-engine host that
//! builds child engines must hand each child the parent's
//! [`PolicyStore`] explicitly (construction does not inherit it).
//! Audited 2026-10-18: no spawn reachable from `Engine::run`
//! ambient-reads a store today, so no re-entry wrapper exists. The one
//! spawn on the tool-execution path — the speculative early-tool-start
//! in `engine/turn/stream.rs` — runs only read-only / non-network /
//! non-approval specs, and none of those reads policy (read tools cap
//! output with compiled constants; working-set recording happens
//! engine-side after the tool returns; large-output routing sits on the
//! dispatcher path, not the ToolSet-adapter path the spawn uses);
//! shell-output truncation runs inline on the turn's task on every
//! live path (the `spawn_blocking` shell wrappers that would lose the
//! scope serve only the never-sent `Op::StartBackgroundShell` and the
//! interactive start, whose output is truncated by later inline
//! polls); and Rhai handlers have no runtime policy-read API (only
//! `register_policy`), so the blocking-pool emit/hook dispatches read
//! nothing. If a read-only spec ever starts reading policy, re-enter
//! the scope at that spawn: capture `PolicyStore::ambient()` before it
//! and wrap the spawned future in `.scope()`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, OnceLock, RwLock};

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
    /// Large-output routing thresholds (`threshold_tokens`, `per_tool`) —
    /// the workshop synthesis call and its security framing stay in Rust.
    /// Explicit `[workshop]` TOML sits above this kind: config values
    /// win, the script tier supplies the default when TOML is silent.
    LargeOutputRoute,
    /// Working-set sizing: the recent-read-files window the session
    /// keeps (`recent_read_file_limit`, `recent_read_file_snippet_chars`)
    /// — retention mechanics (dedup, ordering, eviction) stay in Rust.
    WorkingSet,
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
            Self::LargeOutputRoute => "large-output-route",
            Self::WorkingSet => "working-set",
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
            "large-output-route" => Some(Self::LargeOutputRoute),
            "working-set" => Some(Self::WorkingSet),
            _ => None,
        }
    }

    /// Every defined kind, in declaration order (the builtin mod tier
    /// registers one entry per kind it owns).
    pub const ALL: &'static [PolicyKind] = &[
        PolicyKind::OutputDiff,
        PolicyKind::ShellOutput,
        PolicyKind::CompactPolicy,
        PolicyKind::LargeOutputRoute,
        PolicyKind::WorkingSet,
    ];
}

/// One last-wins-resolved policy entry: the effective config plus the id
/// of the mod whose registration won (`builtin-*` for the builtin tier).
/// The runner's flush builds these and keeps them for the
/// resolved-policy observability view; the [`PolicyStore`] publishes
/// only the kind → config half (mechanism readers stay source-agnostic).
#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub struct ResolvedPolicy {
    pub kind: PolicyKind,
    pub config: Value,
    pub source: String,
}

type PolicyMap = HashMap<PolicyKind, Value>;

tokio::task_local! {
    /// The store of the engine whose turn is on this task's stack —
    /// entered by [`PolicyStore::scope`] at `Engine::run`. Reads outside
    /// any scope fall back to the process-default store.
    static SCOPED: PolicyStore;
}

/// One engine's resolved-policy set: a cheap-to-clone shared handle the
/// `Session` carries, the runner's flush publishes into, and
/// [`scope`](Self::scope) installs as the ambient set for a turn. Never
/// written by mechanism code — only the runner flush / reload clear and
/// the runner-less builtin seed do.
#[derive(Debug, Clone)]
pub struct PolicyStore(Arc<RwLock<PolicyMap>>);

impl Default for PolicyStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PolicyStore {
    /// A fresh private store — the multi-engine-safe default (nothing is
    /// shared until a host chooses to share it).
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(HashMap::new())))
    }

    /// The process-default store: what scope-less reads resolve (tests,
    /// headless utilities, detached tasks). A single-engine host points
    /// its engine's store here (the CLI does); a multi-engine host keeps
    /// each engine's store private.
    #[must_use]
    pub fn process_default() -> Self {
        static DEFAULT: OnceLock<PolicyStore> = OnceLock::new();
        DEFAULT.get_or_init(Self::new).clone()
    }

    /// The ambient store — the scoped one when a turn is on the stack,
    /// else the process default.
    #[must_use]
    pub fn ambient() -> Self {
        SCOPED
            .try_with(Clone::clone)
            .ok()
            .unwrap_or_else(Self::process_default)
    }

    /// Run `fut` with this store as the ambient set. `Engine::run` wraps
    /// its whole loop here, so every read the turn awaits — tool-result
    /// formatting, compaction, working set — resolves this engine's set.
    pub fn scope<F: Future>(&self, fut: F) -> impl Future<Output = F::Output> {
        SCOPED.scope(self.clone(), fut)
    }

    /// Publish the resolved policy set (the runner's flush step:
    /// replaces the whole map — clear + set in one move so readers never
    /// observe a half-applied generation).
    pub fn publish(&self, resolved: HashMap<PolicyKind, Value>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = resolved;
    }

    /// Seed the builtin-tier defaults when nothing is published yet (the
    /// runner-less embed path — one implementation of the defaults, two
    /// materialization paths). Idempotent per store; a benign race
    /// publishes the same defaults twice.
    pub fn seed_if_empty(&self, defaults: HashMap<PolicyKind, Value>) {
        let mut guard = self.0.write().unwrap_or_else(|e| e.into_inner());
        if guard.is_empty() {
            *guard = defaults;
        }
    }

    /// Clear every published policy (the reload path's clear step; the
    /// next flush republishes).
    pub fn clear(&self) {
        self.0.write().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Read one published policy value. `None` = no registration won for
    /// the kind (callers fall back to their compiled defaults and warn
    /// once).
    #[must_use]
    pub fn get(&self, kind: PolicyKind) -> Option<Value> {
        self.0
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&kind)
            .cloned()
    }

    /// Read a `usize` field of a published policy map with a default +
    /// bounds. Bad or out-of-bounds values warn (once per read site) and
    /// fall back — a mod bug must not brick the mechanism it tunes.
    #[must_use]
    pub fn read_usize(&self, kind: PolicyKind, field: &str, default: usize, max: usize) -> usize {
        let Some(Value::Object(map)) = self.get(kind) else {
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
    /// length cap (same warn-and-fall-back posture as
    /// [`read_usize`](Self::read_usize)).
    #[must_use]
    pub fn read_string(
        &self,
        kind: PolicyKind,
        field: &str,
        default: &str,
        max_chars: usize,
    ) -> String {
        let Some(Value::Object(map)) = self.get(kind) else {
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

    /// Read a string-array field of a published policy map with a
    /// default + entry cap. Non-string entries or a too-long list warn
    /// and fall back to the default wholesale (a policy list is tuning,
    /// not protocol).
    #[must_use]
    pub fn read_string_list(
        &self,
        kind: PolicyKind,
        field: &str,
        default: &[&str],
        max_entries: usize,
    ) -> Vec<String> {
        let Some(Value::Object(map)) = self.get(kind) else {
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

    /// Read a string→usize map field of a published policy map. Entries
    /// are independent overrides, so — unlike the wholesale fallback of
    /// [`read_string_list`](Self::read_string_list) — a bad entry
    /// (non-numeric value, over-ceiling number) is skipped with a warn
    /// and the rest of the map still applies. `None` when the kind has
    /// no registration or the field is absent.
    #[must_use]
    pub fn read_usize_map(
        &self,
        kind: PolicyKind,
        field: &str,
        max_entries: usize,
        max_value: usize,
    ) -> Option<HashMap<String, usize>> {
        let value = self.get(kind)?;
        let Value::Object(entries) = value.get(field)? else {
            tracing::warn!(
                target: "codesmith_policy",
                "policy {} field {field} is not a map ({value}); ignoring it",
                kind.kebab_name()
            );
            return None;
        };
        let mut out = HashMap::with_capacity(entries.len().min(max_entries));
        for (tool, v) in entries {
            if out.len() >= max_entries {
                tracing::warn!(
                    target: "codesmith_policy",
                    "policy {} field {field} exceeds {max_entries} entries; ignoring the rest",
                    kind.kebab_name()
                );
                break;
            }
            match v.as_u64() {
                Some(n) if (n as usize) <= max_value => {
                    out.insert(tool.clone(), n as usize);
                }
                Some(too_big) => {
                    tracing::warn!(
                        target: "codesmith_policy",
                        "policy {} field {field}[{tool}] = {too_big} exceeds {max_value}; skipping entry",
                        kind.kebab_name()
                    );
                }
                None => {
                    tracing::warn!(
                        target: "codesmith_policy",
                        "policy {} field {field}[{tool}] is not a non-negative integer; skipping entry",
                        kind.kebab_name()
                    );
                }
            }
        }
        Some(out)
    }
}

/// Publish to the ambient store (the scope's when a turn is running,
/// else the process default). See [`PolicyStore::publish`].
pub fn publish_policies(resolved: HashMap<PolicyKind, Value>) {
    PolicyStore::ambient().publish(resolved);
}

/// Clear the ambient store. See [`PolicyStore::clear`].
pub fn clear_published_policies() {
    PolicyStore::ambient().clear();
}

/// Read one policy value from the ambient store. `None` = no
/// registration won for the kind (callers fall back to their compiled
/// defaults and warn once).
#[must_use]
pub fn published_policy(kind: PolicyKind) -> Option<Value> {
    PolicyStore::ambient().get(kind)
}

/// Read a `usize` field ambiently — [`PolicyStore::read_usize`] on the
/// ambient store.
#[must_use]
pub fn policy_usize(kind: PolicyKind, field: &str, default: usize, max: usize) -> usize {
    PolicyStore::ambient().read_usize(kind, field, default, max)
}

/// Read a `String` field ambiently — [`PolicyStore::read_string`] on the
/// ambient store.
#[must_use]
pub fn policy_string(kind: PolicyKind, field: &str, default: &str, max_chars: usize) -> String {
    PolicyStore::ambient().read_string(kind, field, default, max_chars)
}

/// Read a string-array field ambiently — [`PolicyStore::
/// read_string_list`] on the ambient store.
#[must_use]
pub fn policy_string_list(
    kind: PolicyKind,
    field: &str,
    default: &[&str],
    max_entries: usize,
) -> Vec<String> {
    PolicyStore::ambient().read_string_list(kind, field, default, max_entries)
}

/// Read a string→usize map field ambiently — [`PolicyStore::
/// read_usize_map`] on the ambient store.
#[must_use]
pub fn policy_usize_map(
    kind: PolicyKind,
    field: &str,
    max_entries: usize,
    max_value: usize,
) -> Option<HashMap<String, usize>> {
    PolicyStore::ambient().read_usize_map(kind, field, max_entries, max_value)
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
        // The ambient store here is the process default (no scope);
        // this test is the only writer among the policy tests (they run
        // on separate threads — tolerate a torn read by asserting only
        // what this test published).
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

    /// The defect this pins: two engines in one process each publish
    /// their own set — a process-global snapshot would read the last
    /// bind for both, and a detached `spawn`ed task would lose the scope.
    #[tokio::test]
    async fn scoped_stores_isolate_engines() {
        let a = PolicyStore::new();
        let mut set = HashMap::new();
        set.insert(PolicyKind::OutputDiff, json!({"context_radius": 5}));
        a.publish(set);
        let b = PolicyStore::new();
        let mut set = HashMap::new();
        set.insert(PolicyKind::OutputDiff, json!({"context_radius": 9}));
        b.publish(set);

        a.scope(async {
            assert_eq!(
                policy_usize(PolicyKind::OutputDiff, "context_radius", 3, 64),
                5
            );
        })
        .await;
        b.scope(async {
            assert_eq!(
                policy_usize(PolicyKind::OutputDiff, "context_radius", 3, 64),
                9
            );
        })
        .await;

        // A spawned subtask does NOT inherit the scope (documented
        // limitation): it resolves the process default, never engine b.
        let detached = b.scope(async {
            tokio::spawn(async {
                assert_ne!(
                    policy_usize(PolicyKind::OutputDiff, "context_radius", 3, 64),
                    9
                );
            })
            .await
            .unwrap();
        });
        detached.await;

        // Outside any scope → the process default (no OutputDiff entry:
        // the round-trip test above publishes ShellOutput only).
        assert_eq!(published_policy(PolicyKind::OutputDiff), None);
    }

    /// Per-store seeding replaces the process-once seed: two runner-less
    /// stores each materialize the builtin tier independently.
    #[test]
    fn seed_if_empty_is_per_store() {
        let a = PolicyStore::new();
        let b = PolicyStore::new();
        let mut defaults = HashMap::new();
        defaults.insert(PolicyKind::ShellOutput, json!({"max_output_size": 42}));
        a.seed_if_empty(defaults.clone());
        a.seed_if_empty(defaults); // idempotent: second seed is a no-op
        b.seed_if_empty(HashMap::new()); // empty defaults seed nothing
        assert_eq!(
            a.get(PolicyKind::ShellOutput),
            Some(json!({"max_output_size": 42}))
        );
        assert_eq!(b.get(PolicyKind::ShellOutput), None);
    }
}
