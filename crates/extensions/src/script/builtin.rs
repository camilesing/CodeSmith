//! Builtin mod tier — embedded Rhai mods that ship in the binary and
//! provide the **default** policies (and other scriptable defaults) the
//! host mechanisms consult. This is the "everything is a plugin" tier
//! split: the default values a hardcoded `const` used to hold now live in
//! readable, overridable script beside the user's mods.
//!
//! Trust + lifecycle:
//! - Builtins skip the discovery/consent/hash gates — they ship inside the
//!   binary, exactly as trusted as the Rust they replaced. `[mods]
//!   enabled = false` disables *user* mods only; the always-on builtin
//!   tier loads regardless (it IS the default behavior).
//! - **Opt-in builtins** (`opt_in: true`, ids without the `builtin-`
//!   prefix) are different: they change behavior (a replacement, a loop, a
//!   strategy), so they load only when `[mods] builtin = ["<id>", …]`
//!   names them. That list is the config-selected loading surface for
//!   every behavior-changing builtin — an unnamed opt-in mod is compiled
//!   into the binary but never runs.
//! - A user mod overrides a builtin **by registering the same policy
//!   kind** (last-wins by load order), not by shadowing the id. Same-id
//!   shadowing is not offered: builtin ids are not discoverable, so
//!   `/mods` never lists them.
//! - KV is ephemeral — builtins are pure data registrations; a builtin
//!   that grows state later gets a scoped store decision then.

use std::sync::Arc;

use codesmith_agent::extension::MessageProjectionHubArc;

use super::kv::ModKvStore;
use super::rhai_mod::RhaiMod;

/// One embedded builtin mod. `source` is the entry script (no `mod.toml`;
/// the manifest is the struct).
pub struct BuiltinMod {
    pub id: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub source: &'static str,
    /// Load only when `[mods] builtin` names this id. `false` = the
    /// always-on default tier (policy defaults); `true` = a
    /// behavior-changing mod the user opts into by id.
    pub opt_in: bool,
}

/// The builtin tier. One mod per policy family; the defaults below were
/// the compiled constants before 0.11.0 (see the owning mechanism's
/// `policy_*` reader for the authoritative fallbacks). Opt-in entries
/// (behavior-changing ports) carry no `builtin-` id prefix: their id is
/// the user-facing selection key shared with the config that names them.
pub const BUILTIN_MODS: &[BuiltinMod] = &[
    BuiltinMod {
        id: "builtin-output-diff",
        version: "0.1.0",
        description: "Default unified-diff rendering policy (context radius)",
        source: include_str!("builtin/output_diff.rhai"),
        opt_in: false,
    },
    BuiltinMod {
        id: "builtin-shell-output",
        version: "0.1.0",
        description: "Default shell-output truncation policy (sizes and budgets)",
        source: include_str!("builtin/shell_output.rhai"),
        opt_in: false,
    },
    BuiltinMod {
        id: "builtin-compact-policy",
        version: "0.1.0",
        description: "Default micro-compaction policy (thresholds, tools, placeholder)",
        source: include_str!("builtin/compact_policy.rhai"),
        opt_in: false,
    },
    BuiltinMod {
        id: "builtin-large-output-route",
        version: "0.1.0",
        description: "Default large-output routing policy (thresholds)",
        source: include_str!("builtin/large_output_route.rhai"),
        opt_in: false,
    },
    BuiltinMod {
        id: "builtin-working-set",
        version: "0.1.0",
        description: "Default working-set policy (recent-read window + preview size)",
        source: include_str!("builtin/working_set.rhai"),
        opt_in: false,
    },
    BuiltinMod {
        id: "builtin-knowledge-on-demand",
        version: "0.1.0",
        description: "Default Knowledge On Demand policy (memory budgets + enabled types)",
        source: include_str!("builtin/knowledge_policy.rhai"),
        opt_in: false,
    },
    BuiltinMod {
        id: "builtin-lats-policy",
        version: "0.1.0",
        description: "Default loop-script search policy (width / depth / beam)",
        source: include_str!("builtin/lats_policy.rhai"),
        opt_in: false,
    },
];

/// Compile + run the builtin tier against the runner (used by the host's
/// populate step, before user mods — so user registrations win the
/// last-wins policy resolution). `selected` names opt-in entries to load
/// (config `[mods] builtin`); an opt-in mod not in the list stays
/// compiled-out of the session, and unknown names in the list are
/// surfaced per-entry as a load failure (fail loud, not silent skip).
/// Returns `(id, error)` per failure: a broken builtin is a binary
/// defect; the host surfaces it loudly instead of silently shipping
/// different defaults.
pub fn load_builtin_mods(
    hub: MessageProjectionHubArc,
    selected: &[String],
) -> Vec<(String, Result<RhaiMod, crate::ExtensionError>)> {
    BUILTIN_MODS
        .iter()
        .filter(|m| !m.opt_in || selected.iter().any(|s| s == m.id))
        .map(|m| {
            let result = RhaiMod::load_source(
                m.id,
                m.id,
                m.version,
                m.source.to_string(),
                ModKvStore::ephemeral(),
                MessageProjectionHubArc::clone(&hub),
            );
            (m.id.to_string(), result)
        })
        .chain(unknown_builtin_selections(selected).map(|name| {
            let error = crate::ExtensionError::Load(format!(
                "[mods] builtin names {name:?}, but no opt-in builtin mod carries that id \
                 (see docs/MODS.md for the available ids)"
            ));
            (name, Err(error))
        }))
        .collect()
}

/// Names in `selected` that match no opt-in builtin entry — each becomes
/// an audited load failure so a typo'd id cannot silently disable the
/// behavior the user asked for.
fn unknown_builtin_selections(selected: &[String]) -> impl Iterator<Item = String> + '_ {
    selected.iter().filter_map(|name| {
        let known = BUILTIN_MODS
            .iter()
            .any(|m| m.opt_in && m.id == name.as_str());
        (!known).then(|| name.clone())
    })
}

/// Convenience for hosts that only need the compiled defaults without a
/// runner (embeds): compile the builtin tier once per process and return
/// the policy registrations it captures. Cache is per-process (`OnceLock`)
/// — compiled ASTs are shared.
pub fn builtin_policy_defaults() -> Vec<(codesmith_agent::policy::PolicyKind, serde_json::Value)> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<(codesmith_agent::policy::PolicyKind, serde_json::Value)>> =
        OnceLock::new();
    CACHE
        .get_or_init(|| {
            let hub = MessageProjectionHubArc::new(
                codesmith_agent::extension::MessageProjectionHub::new(),
            );
            let mut out = Vec::new();
            // Defaults only — opt-in builtins never contribute ambient
            // policy defaults (they run when selected, or not at all).
            for (_, m) in load_builtin_mods(Arc::clone(&hub), &[]) {
                match m {
                    Ok(rhai_mod) => {
                        for reg in rhai_mod.registrations() {
                            if let super::rhai_mod::ScriptRegistration::Policy { kind, config } =
                                reg
                            {
                                out.push((*kind, config.clone()));
                            }
                        }
                    }
                    Err(e) => {
                        // A binary defect; the mechanism's compiled
                        // defaults still apply. Loud, once.
                        tracing::error!(
                            target: "codesmith_mods",
                            "builtin mod failed to compile (compiled defaults in effect): {e}"
                        );
                    }
                }
            }
            out
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent::extension::MessageProjectionHubArc;

    fn hub() -> MessageProjectionHubArc {
        MessageProjectionHubArc::new(codesmith_agent::extension::MessageProjectionHub::new())
    }

    /// The default tier always loads regardless of the selection list —
    /// `[mods] builtin` only adds opt-in entries, it never removes a
    /// default. An empty selection loads exactly the non-opt-in set with
    /// zero failures.
    #[test]
    fn default_tier_loads_with_an_empty_selection() {
        let entries = load_builtin_mods(hub(), &[]);
        assert!(entries.iter().all(|(_, r)| r.is_ok()), "builtins compile");
        assert_eq!(
            entries.len(),
            BUILTIN_MODS.iter().filter(|m| !m.opt_in).count()
        );
    }

    /// A selection naming no opt-in builtin is an audited load failure
    /// (fail loud) — a typo must not silently disable the requested
    /// behavior.
    #[test]
    fn unknown_selection_is_an_audited_failure() {
        let entries = load_builtin_mods(hub(), &["compact-nonexistent".to_string()]);
        let (_, result) = entries
            .iter()
            .find(|(id, _)| id == "compact-nonexistent")
            .expect("the unknown name appears as its own audit entry");
        let err = result.as_ref().expect_err("unknown selection fails");
        assert!(err.to_string().contains("no opt-in builtin mod"));
    }

    /// `builtin_policy_defaults` (the embed path) excludes opt-in mods:
    /// they contribute ambient defaults only when selected.
    #[test]
    fn policy_defaults_exclude_opt_in_mods() {
        let defaults = builtin_policy_defaults();
        // Every policy default traces to a non-opt-in mod id by
        // construction; with no opt-in entries shipped yet this asserts
        // the shape (non-empty, all Ok) and pins the call path.
        assert!(!defaults.is_empty());
    }
}
