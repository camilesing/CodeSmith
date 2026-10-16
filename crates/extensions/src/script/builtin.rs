//! Builtin mod tier — embedded Rhai mods that ship in the binary and
//! provide the **default** policies (and other scriptable defaults) the
//! host mechanisms consult. This is the "everything is a plugin" tier
//! split: the default values a hardcoded `const` used to hold now live in
//! readable, overridable script beside the user's mods.
//!
//! Trust + lifecycle:
//! - Builtins skip the discovery/consent/hash gates — they ship inside the
//!   binary, exactly as trusted as the Rust they replaced. `[mods]
//!   enabled = false` disables *user* mods only; the builtin tier always
//!   loads (it IS the default behavior).
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
}

/// The builtin tier. One mod per policy family; the defaults below were
/// the compiled constants before 0.11.0 (see the owning mechanism's
/// `policy_*` reader for the authoritative fallbacks).
pub const BUILTIN_MODS: &[BuiltinMod] = &[
    BuiltinMod {
        id: "builtin-output-diff",
        version: "0.1.0",
        description: "Default unified-diff rendering policy (context radius)",
        source: include_str!("builtin/output_diff.rhai"),
    },
    BuiltinMod {
        id: "builtin-shell-output",
        version: "0.1.0",
        description: "Default shell-output truncation policy (sizes and budgets)",
        source: include_str!("builtin/shell_output.rhai"),
    },
    BuiltinMod {
        id: "builtin-compact-policy",
        version: "0.1.0",
        description: "Default micro-compaction policy (thresholds, tools, placeholder)",
        source: include_str!("builtin/compact_policy.rhai"),
    },
    BuiltinMod {
        id: "builtin-large-output-route",
        version: "0.1.0",
        description: "Default large-output routing policy (thresholds)",
        source: include_str!("builtin/large_output_route.rhai"),
    },
];

/// Compile + run every builtin mod against the runner (used by the host's
/// populate step, before user mods — so user registrations win the
/// last-wins policy resolution). Returns `(id, error)` per failure: a
/// broken builtin is a binary defect; the host surfaces it loudly instead
/// of silently shipping different defaults.
pub fn load_builtin_mods(
    hub: MessageProjectionHubArc,
) -> Vec<(String, Result<RhaiMod, crate::ExtensionError>)> {
    BUILTIN_MODS
        .iter()
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
        .collect()
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
            for (_, m) in load_builtin_mods(Arc::clone(&hub)) {
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
