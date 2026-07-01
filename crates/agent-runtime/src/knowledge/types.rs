//! Memory type taxonomy for Knowledge On Demand.
//!
//! Each type determines when and how a memory should be saved and surfaced,
//! mirroring the TypeScript Claude Code memory type definitions.

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
