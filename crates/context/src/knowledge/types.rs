//! Memory type taxonomy for Knowledge On Demand.
//!
//! Each type determines when and how a memory should be saved and surfaced,
//! mirroring the TypeScript Claude Code memory type definitions.

use std::collections::HashSet;

use codesmith_agent::policy::{PolicyKind, policy_string_list};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The four memory categories used in KoD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    /// User profile, role, knowledge level, and preferences.
    /// Saved when learning any detail about who the user is.
    /// Example: "User is a data scientist investigating logging".
    User,
    /// Guidance the user gave about how to approach work — what to avoid
    /// and what to keep doing. Record from both corrections and confirmations.
    /// Include *why* so future sessions can judge edge cases.
    /// Example: "Integration tests must hit a real database, not mocks.
    /// Reason: prior incident where mock/prod divergence masked a broken migration".
    Feedback,
    /// Ongoing work, goals, initiatives, bugs, or incidents that are not
    /// otherwise derivable from code or git history. Convert relative dates
    /// to absolute when saving.
    /// Example: "Merge freeze begins 2026-03-05 for mobile release cut".
    Project,
    /// Pointers to where information lives in external systems.
    /// Example: "Pipeline bugs are tracked in Linear project INGEST".
    Reference,
}

impl MemoryType {
    /// Human-readable description for this memory type, used in system
    /// prompt guidance and frontmatter defaults.
    pub fn description(self) -> &'static str {
        match self {
            Self::User => "User profile, role, knowledge, and preferences",
            Self::Feedback => "Behavioral guidance — corrections and validated approaches",
            Self::Project => "Ongoing work, goals, bugs, and initiatives",
            Self::Reference => "Pointers to information in external systems",
        }
    }

    /// Parse from a string, matching serde rename format or display format.
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "user" => Some(Self::User),
            "feedback" => Some(Self::Feedback),
            "project" => Some(Self::Project),
            "reference" => Some(Self::Reference),
            _ => None,
        }
    }
}

impl fmt::Display for MemoryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        })
    }
}

/// Every memory type — the compiled default for `enabled_types`.
const ALL_TYPE_NAMES: &[&str] = &["user", "feedback", "project", "reference"];

fn all_memory_types() -> HashSet<MemoryType> {
    [
        MemoryType::User,
        MemoryType::Feedback,
        MemoryType::Project,
        MemoryType::Reference,
    ]
    .into_iter()
    .collect()
}

/// The memory types eligible for surfacing — the `enabled_types` knob of
/// the `knowledge-on-demand` policy kind, read ambiently with an
/// all-types compiled fallback (same posture as micro-compaction's
/// `policy_read`: an unknown type name warns and falls back wholesale;
/// an empty list means "disable everything" and also falls back — a
/// policy list is tuning, not a way to silence memory). Memories
/// without a frontmatter type are always eligible: the knob disables
/// typed categories, it does not silence untyped files.
pub fn enabled_memory_types() -> HashSet<MemoryType> {
    let names = policy_string_list(
        PolicyKind::KnowledgeOnDemand,
        "enabled_types",
        ALL_TYPE_NAMES,
        16,
    );
    let mut out = HashSet::with_capacity(names.len());
    for name in &names {
        match MemoryType::from_str_loose(name) {
            Some(t) => {
                out.insert(t);
            }
            None => {
                tracing::warn!(
                    target: "codesmith_policy",
                    "policy knowledge-on-demand field enabled_types has unknown type {name}; using default"
                );
                return all_memory_types();
            }
        }
    }
    if out.is_empty() {
        return all_memory_types();
    }
    out
}

/// A memory file selected and read for surfacing into context.
#[derive(Debug, Clone)]
pub struct SurfacedMemory {
    /// Absolute path to the memory file.
    pub path: std::path::PathBuf,
    /// Staleness header (e.g. "[Memory: role.md, last modified 7 days ago]").
    pub staleness_header: String,
    /// Truncated content of the memory file (max 30 lines / 10KB).
    pub content: String,
    /// Whether content was truncated to fit budget.
    pub was_truncated: bool,
    /// Byte count of the surfaced content.
    pub byte_count: usize,
}
