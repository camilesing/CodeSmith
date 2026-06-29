//! Sub-agent type identity, status, and assignment data types.
//!
//! [`SubAgentType`], [`SubAgentStatus`], and [`SubAgentAssignment`] are shared
//! between the engine and the TUI's sub-agent runtime, so they live here in the
//! terminal-agnostic runtime. The TUI re-exports them at the historical
//! `crate::tools::subagent` path for backwards compatibility. The sub-agent
//! *implementation* (spawn orchestration, context forking, result aggregation)
//! still lives in the TUI for now.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubAgentAssignment {
    pub objective: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl SubAgentAssignment {
    pub fn new(objective: String, role: Option<String>) -> Self {
        Self { objective, role }
    }
}

/// Sub-agent execution types with specialized behavior and tool access.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentType {
    /// General purpose - full tool access for multi-step tasks.
    #[default]
    General,
    /// Fast exploration - read-only tools for codebase search.
    Explore,
    /// Planning - analysis tools only for architectural planning.
    Plan,
    /// Code review - read + analysis tools.
    Review,
    /// Implementation — focused on writing / patching code to satisfy
    /// a specific change. Distinct from `General` in that the prompt
    /// posture pushes hard on landing the change cleanly with the
    /// minimum surrounding edit (#404).
    Implementer,
    /// Verification — focused on running the test suite or other
    /// validation gates and reporting pass/fail with evidence.
    /// Distinct from `Review` in that Review reads code and grades it;
    /// Verifier *runs* tests and reports the outcome (#404).
    Verifier,
    /// Tool execution — a fast, non-thinking Flash V4 executor for simple
    /// machine-bound tasks. Intended as the experimental "Fin" lane: the
    /// parent does planning/synthesis while this child runs tools and reports
    /// compact facts.
    ToolAgent,
    /// Custom tool access defined at spawn time.
    Custom,
    /// Team member — an in-process teammate running in swarm mode.
    /// Shares a task list with other team members and communicates
    /// via file-based mailbox.
    Team,
    /// Worker spawned by a coordinator agent. Gets full tool access
    /// minus team management tools. Uses a worker-specific system prompt
    /// that emphasizes executing the assigned task independently.
    CoordinatorWorker,
}

impl SubAgentType {
    /// Parse a sub-agent type from user input.
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "general" | "general-purpose" | "general_purpose" | "worker" | "default" => {
                Some(Self::General)
            }
            "explore" | "exploration" | "explorer" => Some(Self::Explore),
            "plan" | "planning" | "awaiter" => Some(Self::Plan),
            "review" | "code-review" | "code_review" | "reviewer" => Some(Self::Review),
            "implementer" | "implement" | "implementation" | "builder" => Some(Self::Implementer),
            "verifier" | "verify" | "verification" | "validator" | "tester" => Some(Self::Verifier),
            "tool-agent" | "tool_agent" | "toolagent" | "executor" | "execution" | "fin" => {
                Some(Self::ToolAgent)
            }
            "custom" => Some(Self::Custom),
            "team" | "teammate" | "swarm" => Some(Self::Team),
            "coordinator-worker" | "coordinator_worker" | "coord_worker" => {
                Some(Self::CoordinatorWorker)
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Explore => "explore",
            Self::Plan => "plan",
            Self::Review => "review",
            Self::Implementer => "implementer",
            Self::Verifier => "verifier",
            Self::ToolAgent => "tool_agent",
            Self::Custom => "custom",
            Self::Team => "team",
            Self::CoordinatorWorker => "coordinator_worker",
        }
    }

    /// Get the system prompt for this agent type.
    #[must_use]
    pub fn system_prompt(&self) -> String {
        let role_intro = match self {
            Self::General => GENERAL_AGENT_INTRO,
            Self::Explore => EXPLORE_AGENT_INTRO,
            Self::Plan => PLAN_AGENT_INTRO,
            Self::Review => REVIEW_AGENT_INTRO,
            Self::Implementer => IMPLEMENTER_AGENT_INTRO,
            Self::Verifier => VERIFIER_AGENT_INTRO,
            Self::ToolAgent => TOOL_AGENT_INTRO,
            Self::Custom => CUSTOM_AGENT_INTRO,
            Self::Team => GENERAL_AGENT_INTRO, // Team agents reuse general prompt with team context
            Self::CoordinatorWorker => COORDINATOR_WORKER_INTRO,
        };
        format!("{role_intro}{SUBAGENT_OUTPUT_FORMAT}")
    }

    /// Get the default allowed tools for this agent type.
    ///
    /// **Deprecated since v0.6.6.** Default sub-agents now inherit the full
    /// parent registry; the per-type allowlist is advisory only. Pass an explicit
    /// `allowed_tools` array for narrow Custom roles instead.
    #[must_use]
    #[deprecated(
        since = "0.6.6",
        note = "Default sub-agents inherit the full parent registry; pass an explicit allowed_tools list only for narrow Custom roles."
    )]
    pub fn allowed_tools(&self) -> Vec<&'static str> {
        match self {
            Self::General => vec![
                "list_dir",
                "read_file",
                "write_file",
                "edit_file",
                "apply_patch",
                "grep_files",
                "file_search",
                "web.run",
                "web_search",
                "exec_shell",
                "exec_shell_wait",
                "exec_shell_interact",
                "exec_wait",
                "exec_interact",
                "note",
                "checklist_write",
                "checklist_add",
                "checklist_update",
                "checklist_list",
                "todo_write",
                "todo_add",
                "todo_update",
                "todo_list",
                "update_plan",
            ],
            Self::Explore => vec![
                "list_dir",
                "read_file",
                "grep_files",
                "file_search",
                "web.run",
                "web_search",
                "exec_shell",
                "exec_shell_wait",
                "exec_shell_interact",
                "exec_wait",
                "exec_interact",
            ],
            Self::Plan => vec![
                "list_dir",
                "read_file",
                "grep_files",
                "file_search",
                "web.run",
                "note",
                "update_plan",
                "checklist_write",
                "checklist_add",
                "checklist_update",
                "checklist_list",
                "todo_write",
                "todo_add",
                "todo_update",
                "todo_list",
            ],
            Self::Review => vec!["list_dir", "read_file", "grep_files", "file_search", "note"],
            Self::Implementer => vec![
                "list_dir",
                "read_file",
                "write_file",
                "edit_file",
                "apply_patch",
                "grep_files",
                "file_search",
                "exec_shell",
                "exec_shell_wait",
                "exec_shell_interact",
                "exec_wait",
                "exec_interact",
                "note",
                "checklist_write",
                "checklist_add",
                "checklist_update",
                "checklist_list",
                "todo_write",
                "todo_add",
                "todo_update",
                "todo_list",
                "update_plan",
            ],
            Self::Verifier => vec![
                "list_dir",
                "read_file",
                "grep_files",
                "file_search",
                "exec_shell",
                "exec_shell_wait",
                "exec_shell_interact",
                "exec_wait",
                "exec_interact",
                "run_tests",
                "diagnostics",
                "note",
            ],
            Self::ToolAgent => vec![
                "list_dir",
                "read_file",
                "grep_files",
                "file_search",
                "image_ocr",
                "fetch_url",
                "web_search",
                "web.run",
                "exec_shell",
                "exec_shell_wait",
                "exec_shell_interact",
                "exec_wait",
                "exec_interact",
                "handle_read",
            ],
            Self::Custom => vec![], // Must be provided by caller.
            Self::Team => vec![
                "list_dir",
                "read_file",
                "write_file",
                "edit_file",
                "grep_files",
                "file_search",
                "task_create_v2",
                "task_update_v2",
                "task_get_v2",
                "task_list_v2",
                "send_message",
                "exec_shell",
                "exec_shell_wait",
                "exec_shell_interact",
            ],
            Self::CoordinatorWorker => vec![
                "list_dir",
                "read_file",
                "write_file",
                "edit_file",
                "apply_patch",
                "grep_files",
                "file_search",
                "web.run",
                "web_search",
                "fetch_url",
                "exec_shell",
                "exec_shell_wait",
                "exec_shell_interact",
                "exec_wait",
                "exec_interact",
                "note",
                "checklist_write",
                "checklist_add",
                "checklist_update",
                "checklist_list",
                "todo_write",
                "todo_add",
                "todo_update",
                "todo_list",
                "update_plan",
                "diagnostics",
            ],
        }
    }
}

/// Status of a sub-agent execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SubAgentStatus {
    Running,
    Completed,
    Interrupted(String),
    Failed(String),
    Cancelled,
}

const SUBAGENT_OUTPUT_FORMAT: &str = include_str!("prompts/subagent_output_format.md");

const GENERAL_AGENT_INTRO: &str = concat!(
    "You are a general-purpose sub-agent spawned to handle a specific task autonomously.\n",
    "Stay inside the assigned scope; put adjacent work under RISKS/BLOCKERS.\n",
    "Plan multi-step work with `checklist_write`; add `update_plan` for complex strategy.\n",
    "**Stop quickly on failure**: if the same tool call fails 2 times in a row, stop retrying and return what you have so far with a one-line note explaining what's missing. Do not loop on impossible queries (e.g. external API unreachable, rate-limited, or returning empty).\n",
    "**Bounded effort**: prefer one focused attempt over many speculative retries. If you cannot complete the task with available data within 3-5 tool calls, return your current partial findings — the parent agent can compensate with its own knowledge.\n\n"
);

const EXPLORE_AGENT_INTRO: &str = concat!(
    "You are an exploration sub-agent (role: `explore`). Map the relevant code quickly and stay read-only.\n",
    "Orient first: confirm the workspace/project root, read relevant AGENTS.md/README guidance when the tree is unfamiliar, then search only the likely scope.\n",
    "Use list_dir/file_search, grep_files, and read_file; use RLM only for long inputs or many semantic slices, not basic path discovery.\n",
    "DeepSeek V4 can hold broad evidence, but your value is compressed reconnaissance: cite `path:line-range` for each finding and stop once evidence is sufficient.\n",
    "CHANGES will almost always be \"None.\" for an explorer.\n\n"
);

const PLAN_AGENT_INTRO: &str = concat!(
    "You are a planning sub-agent. Produce a grounded, prioritized plan, not patches.\n",
    "Read enough code to avoid guessing; each step names its artifact and verification.\n",
    "Use update_plan/checklist_write for plan artifacts and explain key trade-offs.\n",
    "CHANGES should list plan artifacts only, not future speculative edits.\n\n"
);

const REVIEW_AGENT_INTRO: &str = concat!(
    "You are a code review sub-agent. Stay read-only and report severity-scored findings.\n",
    "Read the diff/files, grep sibling patterns/tests, then order EVIDENCE by severity.\n",
    "Use BLOCKER/MAJOR/MINOR/NIT and include path:line-range plus suggested fix.\n",
    "If no MAJOR+ issues exist, say so plainly in SUMMARY.\n",
    "CHANGES will almost always be \"None.\" for a reviewer.\n\n"
);

const CUSTOM_AGENT_INTRO: &str = concat!(
    "You are a custom sub-agent with a narrowed tool registry.\n",
    "Use only tools available at runtime; put missing capabilities under BLOCKERS and stop.\n",
    "Stay tightly scoped to the assigned objective.\n\n"
);

const IMPLEMENTER_AGENT_INTRO: &str = concat!(
    "You are an implementation sub-agent. Land the assigned change with minimal surrounding edits.\n",
    "Read target files before editing; prefer edit_file for narrow changes and apply_patch for hunks.\n",
    "Run relevant verification after edit batches; write needed tests with the implementation.\n",
    "CHANGES is load-bearing: list every modified file with a one-line why.\n\n"
);

const VERIFIER_AGENT_INTRO: &str = concat!(
    "You are a verification sub-agent. Run requested gates and stay read-only.\n",
    "Report PASS/FAIL/FLAKY at the top of SUMMARY with exact command evidence.\n",
    "Capture failing assertion and file:line; put obvious fixes under RISKS.\n",
    "CHANGES will almost always be \"None.\" for a verifier.\n\n"
);

const TOOL_AGENT_INTRO: &str = concat!(
    "You are a tool execution sub-agent (experimental Fin fast lane). You run simple tools quickly and report compact facts.\n",
    "The parent model owns planning, trade-offs, and synthesis; do not expand the task or narrate strategy.\n",
    "Prefer direct tool calls, concise evidence, and one-pass results. Stop after the requested machine-bound action is done.\n",
    "CHANGES should be \"None.\" unless an explicitly allowed tool made a real edit.\n\n"
);

const COORDINATOR_WORKER_INTRO: &str = concat!(
    "You are a worker agent spawned by a coordinator. Execute the assigned task independently and report results.\n",
    "You have full tool access to read/write files, run commands, search code, and use web tools.\n",
    "Focus on completing the task thoroughly and report your findings clearly.\n",
    "You cannot use team management tools or send messages to other agents — focus solely on your assigned task.\n\n"
);

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMemoryMetadata {
    pub agent_type: String,
    pub scope: AgentMemoryScope,
    pub dir: PathBuf,
}

/// Snapshot of sub-agent state for tool results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentResult {
    pub name: String,
    pub agent_id: String,
    pub context_mode: String,
    pub fork_context: bool,
    pub agent_type: SubAgentType,
    pub assignment: SubAgentAssignment,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    pub status: SubAgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_memory: Option<AgentMemoryMetadata>,
    pub result: Option<String>,
    pub steps_taken: u32,
    pub duration_ms: u64,
    /// `true` when this agent was loaded from a prior-session persisted
    /// state file rather than spawned in the current session (#405).
    /// Lets `agent_list` filter out historical noise by default while
    /// keeping the records reachable via `include_archived=true`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub from_prior_session: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Terminal-state notification emitted to the engine's parent turn loop
/// when one of its direct children finishes (issue #756). Carries the
/// already-rendered `<codesmith:subagent.done>` sentinel that the model
/// expects in the transcript per `prompts/base.md`.
///
/// Lives in the runtime crate because the engine's turn loop (which drains
/// the paired receiver) is terminal-agnostic; the TUI re-exports it at the
/// historical `crate::tools::subagent` path.
#[derive(Debug, Clone)]
pub struct SubAgentCompletion {
    /// The completing child's agent id. Held for routing/logging — the
    /// engine's turn loop does not currently key on it (it just injects the
    /// payload), but downstream tooling and tests need the field.
    #[allow(dead_code)]
    pub agent_id: String,
    /// Human summary on line 1, sentinel on line 2. Same payload shape as
    /// `Event::AgentComplete::result`.
    pub payload: String,
}

/// Default cap on sub-agent recursion depth. Override via
/// `[runtime] max_spawn_depth = N` in `~/.deepseek/config.toml`.
pub const DEFAULT_MAX_SPAWN_DEPTH: u32 = 3;
