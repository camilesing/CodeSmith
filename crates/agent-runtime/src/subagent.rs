//! Sub-agent type identity, status, and assignment data types.
//!
//! [`SubAgentType`], [`SubAgentStatus`], and [`SubAgentAssignment`] are shared
//! between the engine and the CLI's sub-agent runtime, so they live here in the
//! terminal-agnostic runtime. The CLI re-exports them at the historical
//! `crate::tools::subagent` path for backwards compatibility. The sub-agent
//! *implementation* (spawn orchestration, context forking, result aggregation)
//! still lives in the CLI for now.
//!
//! # External sub-agent seam (capability triple)
//!
//! [`ExternalSubagentProvider`] is the *definition* role of the seam that lets
//! a child run **out of process**: a provider receives an
//! [`ExternalSubagentRequest`] (type, assignment, rendered prompts, model,
//! workspace, spawn-depth budget, approval posture) and returns an
//! [`ExternalSubagentOutcome`] that the consumer maps onto [`SubAgentResult`].
//! The first provider is the CLI's own headless binary (`codesmith exec
//! --json`, `crates/cli/src/tools/subagent/headless.rs`); the consumer is the
//! CLI spawn path (`run_subagent_task`), selected by
//! `[subagents] external_provider`. ACP / Codex / Claude Code providers are
//! deliberate future slots of this seam, not built.
//!
//! What an external child does **not** get — known limitations, each either
//! enforced or made loud by the consumer:
//! - a forked parent context (`fork_context` is rejected at spawn: the request
//!   prefix cannot cross a process boundary);
//! - follow-up input via `agent_send_input` (the input channel is closed, so
//!   the send fails instead of queueing silently);
//! - shared in-process state — team mailboxes, task lists, agent-memory
//!   directories — so `Team` / `CoordinatorWorker` children always stay
//!   in-process, and a headless child's memory writes are not propagated
//!   (its memory *prompt* still rides the system prompt);
//! - approval authority beyond the parent's own: `auto_approve` is forwarded
//!   as a ceiling and never raised.
//!
//! The spawn-depth budget *is* inherited: the child enforces
//! `max_spawn_depth - spawn_depth` locally, so recursion stays bounded across
//! process hops exactly as it is in-process.

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
    #[allow(clippy::should_implement_trait)]
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
    /// **Deprecated since v0.6.6.** Default sub-agents inherit the parent's
    /// effective tool surface (full when the parent is unrestricted, or a
    /// subset under Plan 04 / finding F4 `restrictToSubset` when the parent is
    /// narrowed); the per-type allowlist is advisory only. Pass an explicit
    /// `allowed_tools` array for narrow Custom roles instead.
    #[must_use]
    #[deprecated(
        since = "0.6.6",
        note = "Default sub-agents inherit the parent's effective tool surface (full, or a subset under restrictToSubset); pass an explicit allowed_tools list only for narrow Custom roles."
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
/// the paired receiver) is terminal-agnostic; the CLI re-exports it at the
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
    /// Optional by-value `ToolContext`回流 from the child (finding F4 /
    /// plan 04 §4.2). `None` for the vast majority of completions today —
    /// shared `Arc<Mutex<…>>` state (`SharedTodoList`, `SharedPlanState`,
    /// …) already回流s atomically and never rides this field. When a child
    /// wants to tighten a *by-value* field it cloned at spawn (`auto_approve`,
    /// `trust_mode`), it sets the corresponding `Some(...)` here; the parent
    /// turn loop drains a batch of these and applies them **tighten-only**
    /// after the batch, mirroring Claude Code's `queuedContextModifiers`.
    pub context_patch: Option<ContextPatch>,
}

/// A by-value `ToolContext` patch a child sub-agent sends back to its parent
/// on completion (finding F4 / plan 04 §4.2). Each field is `Some` only when
/// the child wants to change it; `None` means "leave untouched". The parent
/// applies these **tighten-only**: a child may make the parent more
/// restrictive (`auto_approve → false`, `trust_mode → false`) but may never
/// loosen it — an attempt to escalate is rejected and warn-logged.
///
/// This is redundant for `Arc`-shared state (which already回流s atomically);
/// it only matters for the by-value fields cloned per-child at spawn, which
/// the child cannot otherwise mutate on the parent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextPatch {
    /// `Some(false)` tightens (disables auto-approve); `Some(true)` is a
    /// loosen attempt and is rejected. `None` leaves `auto_approve` as-is.
    pub auto_approve: Option<bool>,
    /// `Some(false)` tightens (restricts to workspace, disabling
    /// `trust_mode`); `Some(true)` is a loosen attempt and is rejected.
    /// `None` leaves `trust_mode` as-is.
    pub trust_mode: Option<bool>,
}

impl ContextPatch {
    /// `true` when the patch carries no field changes. Such patches are
    /// skipped by the parent's drain-apply loop without touching session
    /// state.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.auto_approve.is_none() && self.trust_mode.is_none()
    }
}

/// Default cap on sub-agent recursion depth. Override via
/// `[runtime] max_spawn_depth = N` in `~/.codesmith/config.toml`.
pub const DEFAULT_MAX_SPAWN_DEPTH: u32 = 3;

/// One child run handed to an [`ExternalSubagentProvider`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalSubagentRequest {
    pub agent_id: String,
    pub agent_type: SubAgentType,
    pub assignment: SubAgentAssignment,
    /// Role instruction — the same text the in-process path installs as the
    /// child's system prompt (type intro + output format + role + memory).
    pub system_prompt: String,
    /// The rendered objective: what an in-process child sees as its first
    /// user message.
    pub prompt: String,
    /// Model the parent resolved for this child. `None` lets the provider
    /// (or the child binary's own config) pick.
    pub model: Option<String>,
    pub workspace: PathBuf,
    /// The child's depth in the global spawn tree and the inherited cap. A
    /// provider must hand both to the child so it enforces the same budget an
    /// in-process child would (`spawn_depth + 1 > max_spawn_depth` rejects).
    pub spawn_depth: u32,
    pub max_spawn_depth: u32,
    /// Whether the parent runs with auto-approval. This is a ceiling: a
    /// provider must never grant the child more approval authority than the
    /// parent has.
    pub auto_approve: bool,
}

/// What an [`ExternalSubagentProvider`] reports back for one child run.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalSubagentOutcome {
    pub status: SubAgentStatus,
    /// The child's final answer (becomes `SubAgentResult::result`).
    pub output: String,
    /// Tool calls the child made, when the provider can count them.
    pub steps_taken: u32,
    /// Model the child actually ran with, when the provider knows it.
    pub model: Option<String>,
}

/// Definition role of the external sub-agent seam: run one child somewhere
/// other than the parent's process. Implementations own the transport (a
/// subprocess, an RPC, …) and the mapping of its result onto
/// [`ExternalSubagentOutcome`]; the consumer owns bookkeeping (manager
/// state, completion sentinels, hooks) exactly as for an in-process child.
/// Dropping the `run` future is the cancellation path — an implementation
/// must tear down whatever it started (e.g. `kill_on_drop` on a child
/// process).
#[async_trait::async_trait]
pub trait ExternalSubagentProvider: Send + Sync {
    /// Stable provider name for config, logs, and errors (e.g. `"headless"`).
    fn name(&self) -> &'static str;

    /// Run one child to completion.
    async fn run(
        &self,
        request: ExternalSubagentRequest,
    ) -> anyhow::Result<ExternalSubagentOutcome>;
}
