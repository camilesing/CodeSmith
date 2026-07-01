//! Agent-specific persistent memory support.
//!
//! Agent Memory is a scoped, file-backed memory directory bound to a sub-agent
//! type. It intentionally reuses the KoD `MEMORY.md` entrypoint convention while
//! keeping write tools constrained to the resolved agent memory directory.

pub mod paths;
pub mod prompt;
pub mod snapshot;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use paths::{
    agent_memory_candidates, ensure_agent_memory_dir, resolve_agent_memory_dir,
    resolve_agent_memory_entrypoint,
};
pub use prompt::compose_agent_memory_prompt;
pub use snapshot::{AgentMemorySnapshotMode, initialize_or_update_snapshot, load_snapshot_status};

/// Scope for an agent memory directory.
///
/// Re-exported from `crate::subagent::AgentMemoryScope`.
pub use crate::subagent::AgentMemoryScope;

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

/// Metadata for an agent memory binding attached to a sub-agent result.
///
/// Re-exported from `crate::subagent::AgentMemoryMetadata`.
pub use crate::subagent::AgentMemoryMetadata;

impl From<&ResolvedAgentMemory> for AgentMemoryMetadata {
    fn from(memory: &ResolvedAgentMemory) -> Self {
        Self {
            agent_type: memory.agent_type.clone(),
            scope: memory.scope,
            dir: memory.dir.clone(),
        }
    }
}
