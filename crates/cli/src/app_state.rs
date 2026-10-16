//! Frontend-agnostic session and bootstrap types.
//!
//! These lived in the retired ratatui `App` (`tui/app.rs`); they are the
//! data half that outlives every rendering frontend. The REPL
//! (`interactive/`) and the headless runners construct them directly.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::models::Tool;

/// Supported application modes.
///
/// Re-exported from `codesmith-agent-runtime` so the runtime can mode-switch
/// toolsets without depending on the terminal binary.
pub use codesmith_agent_runtime::mode::{AppMode, ReasoningEffort};

/// One row in the per-turn cache-telemetry ring (`/cache` debug surface, #263).
#[derive(Debug, Clone)]
pub struct TurnCacheRecord {
    /// Provider-reported total input tokens for the turn (cache-hit +
    ///   cache-miss + uncategorized). Useful for sanity-checking that hits +
    ///   misses sum back to roughly the prompt size.
    pub input_tokens: u32,
    /// Provider-reported output tokens.
    pub output_tokens: u32,
    /// `prompt_cache_hit_tokens` from DeepSeek's usage payload. `None` when
    /// the model in use does not report cache telemetry (see
    /// `Capabilities::cache_telemetry_supported`).
    pub cache_hit_tokens: Option<u32>,
    /// `prompt_cache_miss_tokens`. `None` when the provider did not report it
    /// — in that case the `/cache` formatter infers the miss as
    /// `input_tokens − cache_hit_tokens`.
    pub cache_miss_tokens: Option<u32>,
}

/// Configuration required to bootstrap an interactive frontend session.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct InteractiveOptions {
    pub model: String,
    pub workspace: PathBuf,
    pub config_path: Option<PathBuf>,
    pub config_profile: Option<String>,
    pub allow_shell: bool,
    /// Maximum number of concurrent sub-agents.
    pub max_subagents: usize,
    pub skills_dir: PathBuf,
    pub memory_path: PathBuf,
    pub mcp_config_path: PathBuf,
    pub use_memory: bool,
    /// Start in agent mode when true and `yolo` is false. When both flags
    /// are false, mode selection falls through to the coordinator flag or
    /// the settings default (see `interactive::state`).
    pub start_in_agent_mode: bool,
    /// Auto-approve tool executions (yolo mode)
    pub yolo: bool,
    /// Resume a previous session by ID
    pub resume_session_id: Option<String>,
    /// Pre-populate the input with this text when the session starts.
    /// Used by `codesmith pr <N>` (#451) to drop the model into a session
    /// with the PR context already typed — the user can edit before sending
    /// or hit Enter to fire as-is.
    pub initial_input: Option<InitialInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialInput {
    /// Pre-populate the input and wait for the user to press Enter.
    ///
    /// Used by `codesmith pr <N>` (#451) to drop the model into a session
    /// with the PR context already typed so the user can edit before sending.
    Prefill(String),
    /// Pre-populate the input, submit it once startup is ready, then keep
    /// the interactive session open for follow-up messages (#2370).
    Submit(String),
}

/// Verdict for a hunt (#2092).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HuntVerdict {
    #[default]
    Hunting,
    Hunted,
    Wounded,
    Escaped,
}

/// Hunt tracking state (#2092 — was GoalState).
#[derive(Debug, Clone, Default)]
pub struct HuntState {
    pub quarry: Option<String>,
    pub token_budget: Option<u32>,
    pub started_at: Option<Instant>,
    pub verdict: HuntVerdict,
}

/// Session cost and token telemetry state.
#[derive(Debug, Clone)]
pub struct SessionState {
    pub session_cost: f64,
    pub session_cost_cny: f64,
    pub subagent_cost: f64,
    pub subagent_cost_cny: f64,
    pub displayed_cost_high_water: f64,
    pub displayed_cost_high_water_cny: f64,
    pub last_prompt_tokens: Option<u32>,
    pub last_completion_tokens: Option<u32>,
    pub last_prompt_cache_hit_tokens: Option<u32>,
    pub last_prompt_cache_miss_tokens: Option<u32>,
    pub last_reasoning_replay_tokens: Option<u32>,
    pub total_tokens: u32,
    pub total_conversation_tokens: u32,
    /// Accumulated token breakdown for the session.
    pub total_input_tokens: u32,
    pub total_cache_hit_tokens: u32,
    pub total_cache_miss_tokens: u32,
    pub total_output_tokens: u32,
    pub turn_cache_history: VecDeque<TurnCacheRecord>,
    /// Tool catalog from the most recent model request.
    ///
    /// `/cache inspect` uses this to inspect the same tool schema bytes
    /// that were eligible for the provider's prefix cache.
    pub last_tool_catalog: Option<Vec<Tool>>,
    /// API base URL used by the most recent model request or cache warmup.
    pub last_base_url: Option<String>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            session_cost: 0.0,
            session_cost_cny: 0.0,
            subagent_cost: 0.0,
            subagent_cost_cny: 0.0,
            displayed_cost_high_water: 0.0,
            displayed_cost_high_water_cny: 0.0,
            last_prompt_tokens: None,
            last_completion_tokens: None,
            last_prompt_cache_hit_tokens: None,
            last_prompt_cache_miss_tokens: None,
            last_reasoning_replay_tokens: None,
            total_tokens: 0,
            total_conversation_tokens: 0,
            total_input_tokens: 0,
            total_cache_hit_tokens: 0,
            total_cache_miss_tokens: 0,
            total_output_tokens: 0,
            turn_cache_history: VecDeque::new(),
            last_tool_catalog: None,
            last_base_url: None,
        }
    }
}
