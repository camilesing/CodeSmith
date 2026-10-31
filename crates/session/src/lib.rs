//! Session data plane — the DSH `core/session` + `session/*` counterpart,
//! split out of `codesmith-agent-runtime` in round 9.
//!
//! - [`zones`]: the three-zone request contract — `AppendLog` (the live,
//!   authoritative transcript), `TurnScratch`, `PinnedPrefix` /
//!   `ThreeZoneRequest`, and the audited `RebuildReason` vocabulary.
//! - [`journal`]: the append-only durable record of a session's transcript
//!   and main-turn requests (`SessionJournal` seam + `FileSessionJournal`).
//! - [`store`]: saved-session records, the format-governance decode
//!   pipeline, the `SessionStore` seam and the `FileSessionStore` provider.
//! - [`artifacts`] / [`fact_ledger`]: the session-owned records the snapshot
//!   carries (large-output registry, compaction fact ledger).
//! - [`transcript`] / [`primitives`]: dependency-free helpers (tool-pairing
//!   predicates, path extraction, atomic writes, hashing, token estimates).
//! - [`spillover`]: sha-addressed tool-output spillover (large results land
//!   in the session-owned store and travel by reference).
//!
//! Consumers import from this crate directly; the historical
//! `codesmith_agent_runtime::{session_store, session_journal, prompt_zones,
//! artifacts}` and CLI `session_manager` paths were removed with the split.
//!
//! Known limitation: the workspace snapshot pruning a store call triggers
//! (`prune_workspace_snapshots`) lives with the side-git snapshot code in the
//! engine crate; this crate does not own it.

pub mod artifacts;
pub mod fact_ledger;
pub mod journal;
pub mod primitives;
pub mod spillover;
pub mod store;
pub mod transcript;
pub mod zones;

/// The wire/message model the session records carry (from `codesmith-agent`).
pub use codesmith_agent::models;
