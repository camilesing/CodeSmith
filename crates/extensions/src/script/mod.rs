//! Script Mods layer (§F script mod layer) — Rhai-based Mods that implement
//! the same [`Extension`](codesmith_agent::extension::Extension) contract as
//! compiled-in / dylib extensions, so they ride the existing
//! [`ExtensionRunner`](crate::ExtensionRunner) unchanged (graft, not a
//! parallel system).
//!
//! A mod is a directory under a mods root (`~/.codesmith/mods/<id>/` global,
//! `<workspace>/.codesmith/mods/<id>/` project) holding:
//!
//! ```toml
//! # mod.toml
//! id = "commit-guard"
//! version = "0.1.0"
//! description = "…"      # optional — shown at activation approval
//! entry = "mod.rhai"     # optional; no absolute paths / `..`
//! ```
//!
//! …and the entry script. The top-level script runs ONCE at load
//! (= `configure`); its `on` / `register_tool` / `register_command` calls
//! are captured by native functions and replayed against the runner's
//! [`ExtensionApi`](codesmith_agent::extension::ExtensionApi).
//!
//! Capability surface (MVP): `mod_state_get/set` (per-mod persistent KV),
//! `mod_log`, `now_ms`, and the control-value constructors
//! `proceed/block/cancel/transform/ok/err/message/send`. No fs / net /
//! process natives are registered — absence is the sandbox.
//!
//! Resource limits + fail-open: `Engine::set_max_operations(200_000)` +
//! `set_max_call_levels(64)` bound runaway scripts; a hook error is
//! `tracing::warn` + `Continue` (mirrors `emit`'s `catch_unwind` isolation
//! semantics — one broken mod cannot break the chain).

pub mod adapters;
pub mod kv;
pub mod mod_manifest;
pub mod rhai_mod;

pub use adapters::{ScriptCommandDefinition, ScriptHandler, ScriptToolDefinition};
pub use kv::ModKvStore;
pub use mod_manifest::{DiscoveredMod, ModManifest, apply_mod_trust_gate, discover_mods};
pub use rhai_mod::RhaiMod;
