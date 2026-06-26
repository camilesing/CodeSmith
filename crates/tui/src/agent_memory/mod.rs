//! Agent-specific persistent memory support.
//!
//! Agent Memory is a scoped, file-backed memory directory bound to a sub-agent
//! type. It intentionally reuses the KoD `MEMORY.md` entrypoint convention while
//! keeping write tools constrained to the resolved agent memory directory.

pub mod paths;
pub mod prompt;
pub mod snapshot;

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use paths::{
    agent_memory_candidates, ensure_agent_memory_dir, resolve_agent_memory_dir,
    resolve_agent_memory_entrypoint,
};
pub use prompt::compose_agent_memory_prompt;
pub use snapshot::{AgentMemorySnapshotMode, initialize_or_update_snapshot, load_snapshot_status};

/// Scope for an agent memory directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMemoryScope {
    /// User-wide memory shared across workspaces for this agent type.
    User,
    /// Project memory committed/stored with the workspace state directory.
    Project,
    /// Local project memory that should not be shared.
    Local,
}

impl AgentMemoryScope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "local",
        }
    }
}

impl fmt::Display for AgentMemoryScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentMemoryScope {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "user" => Ok(Self::User),
            "project" | "workspace" | "repo" => Ok(Self::Project),
            "local" => Ok(Self::Local),
            other => Err(format!(
                "invalid agent memory scope '{other}', expected user, project, or local"
            )),
        }
    }
}

/// Model-facing request to enable memory for a sub-agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMemoryRequest {
    pub scope: AgentMemoryScope,
    #[serde(default)]
    pub snapshot: AgentMemorySnapshotMode,
}

impl Default for AgentMemoryRequest {
    fn default() -> Self {
        Self {
            scope: AgentMemoryScope::Project,
            snapshot: AgentMemorySnapshotMode::None,
        }
    }
}

/// Resolved memory data threaded into a running child agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentMemory {
    pub agent_type: String,
    pub scope: AgentMemoryScope,
    pub dir: PathBuf,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMemoryMetadata {
    pub agent_type: String,
    pub scope: AgentMemoryScope,
    pub dir: PathBuf,
}

impl From<&ResolvedAgentMemory> for AgentMemoryMetadata {
    fn from(memory: &ResolvedAgentMemory) -> Self {
        Self {
            agent_type: memory.agent_type.clone(),
            scope: memory.scope,
            dir: memory.dir.clone(),
        }
    }
}
