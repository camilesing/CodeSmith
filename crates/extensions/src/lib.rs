//! CodeSmith extension runtime.
//!
//! This crate owns the **runtime** half of the extension system (the
//! **contract** half lives in [`codesmith_agent::extension`]). Slice 1 (§F1)
//! lands:
//!
//! - [`ExtensionRunner`] — host runtime: event dispatch (best-effort fan-out
//!   per §8.3), stale-context guard via `Arc<AtomicU64>` generation
//!   (spec §7.3), `ExtensionApi` stub→real two-phase construction (spec §4),
//!   command dispatch lookup.
//! - [`inventory`]-based static discovery (phase 1; spec §7.1) —
//!   [`ExtensionRegistration`] / [`discover_static`].
//! - [`EventBus`] skeleton (spec §10.1; full impl is §F3).
//! - install-source abstraction **traits** only (impls defer to §F5) —
//!   [`ExtensionSource`] / [`ExtensionBuilder`] / [`ExtensionPlacer`].
//!
//! The adapters that bridge extension registrations onto the production
//! `ToolSpec` / command dispatch / `HostAgentExecutor` seams live in
//! `codesmith-agent-runtime` (Task 5/6), mirroring `ToolSpecAdapter` /
//! `CallbackBridge`.
//!
//! # Module map (filled across Tasks 2-4)
//!
//! - [`runner`] — `ExtensionRunner` (Task 3)
//! - [`api`] — `ExtensionApi` stub + real impls (Task 3)
//! - [`bus`] — `EventBus` skeleton (Task 3)
//! - [`state`] — `HostExtensionContext` (Task 3)
//! - [`discovery`] — `inventory` static discovery (Task 4)
//! - [`install_source`] — install-source traits (Task 4)
//!
//! Specific name re-exports are added in Tasks 3/4 as the names come into
//! existence; for Task 2 only the module declarations + the framework-contract
//! glob re-export are present so the crate compiles cleanly.

pub mod api;
pub mod bus;
pub mod discovery;
pub mod install_source;
pub mod manifest;
pub mod loader;
pub mod runner;
pub mod sample_scratchpad;
pub mod state;

// Slice-1 runtime re-exports.
pub use api::{RealExtensionApi, StubExtensionApi};
pub use bus::EventBus;
pub use discovery::{apply_trust_gate, discover_dylib, discover_static, DiscoveredSource, ExtensionRegistration};
pub use install_source::{ExtensionBuilder, ExtensionPlacer, ExtensionSource, SourceArtifact};
pub use manifest::ExtensionManifest;
pub use loader::load_dylib;
pub use runner::{EmitOutcome, ExtensionRunner};
pub use state::HostExtensionContext;

// Re-export the framework contract so extension authors can depend solely on
// `codesmith-extensions` for everything (traits + runtime).
pub use codesmith_agent::extension::*;
