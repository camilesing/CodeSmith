//! Tool dispatch — plan/execute helpers for the per-turn tool batch.
//!
//! Extracted from `core/engine.rs` (P1.3). The high-level ordering now
//! lives in `HostAgentExecutor` — the legacy engine turn loop, along with
//! the streaming-buffer salvage ladder this module used to own, was
//! retired (stream decoding lives in `turn/stream.rs`); this module owns:
//!
//! * The `multi_tool_use.parallel` payload parser.
//! * Policy predicates the turn loop consults — when a batch can run in
//!   parallel, when an `update_plan` step should stop the turn, when a Plan
//!   prompt should force a plan-first hop, and the small set of read-only
//!   MCP tools that are safe to run in parallel.
//! * The tool execution plan/outcome types the batch driver passes around,
//!   and `format_tool_error`, the error renderer shared by every channel.
//!
//! All items are `pub`-only: the public engine surface (Op/Event,
//! `EngineHandle`, `spawn_engine`) stays in `core/engine.rs`.

use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;

use crate::mode::AppMode;
use crate::models::{Tool, ToolCaller};
use crate::tools::spec::{ToolError, ToolResult};

// === Types ============================================================

#[allow(dead_code)] // `index` mirrors batch order for diagnostic ergonomics.
pub struct ToolExecOutcome {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    pub started_at: std::time::Instant,
    pub result: Result<ToolResult, ToolError>,
    /// Optional by-value context patch a parallel tool outcome may carry back
    /// to the turn loop (finding F4 / plan 04 §4.4). The turn loop collects a
    /// batch's patches and applies them **tighten-only** once after the
    /// `FuturesUnordered` drains, mirroring Claude Code's
    /// `queuedContextModifiers`. Redundant for `Arc`-shared state (which
    /// already回流s atomically) and usually `None` — CodeSmith's concurrent
    /// tools are read-only by design, so they produce no context patches.
    /// The field exists so a future write-capable parallel tool can回流 a
    /// tighten without touching the drain shape.
    pub context_patch: Option<crate::subagent::ContextPatch>,
}

#[derive(Debug)]
pub struct ToolExecutionPlan {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    pub caller: Option<ToolCaller>,
    pub interactive: bool,
    pub approval_required: bool,
    pub approval_description: String,
    /// Whether the call may run concurrently at all (see
    /// [`tool_plan_is_parallel_safe`]).
    pub supports_parallel: bool,
    pub read_only: bool,
    /// `WritesFiles`: the call can mutate the workspace. Writers are the one
    /// class whose concurrency depends on *which* files they name, so the
    /// classifier needs the bit that separates them from a read.
    pub writes_files: bool,
    /// Whether the tool+input can destroy data. A destructive call is never
    /// concurrent, whatever its paths say.
    pub destructive: bool,
    /// Resolved paths the call declares — the files it reads or writes.
    /// Empty means no path information, which the classifier reads as a
    /// refusal to guess (see [`ToolDispatcher::touched_paths`]).
    ///
    /// [`ToolDispatcher::touched_paths`]: crate::tool_dispatch::ToolDispatcher::touched_paths
    pub touched_paths: Vec<PathBuf>,
    pub stream_early_start_safe: bool,
    pub blocked_error: Option<ToolError>,
    pub guard_result: Option<ToolResult>,
}

pub enum ToolExecutionBatch {
    Parallel(Vec<ToolExecutionPlan>),
    Serial(Box<ToolExecutionPlan>),
}

#[derive(Debug, serde::Serialize)]
pub struct ParallelToolResultEntry {
    pub tool_name: String,
    pub success: bool,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ParallelToolResult {
    pub results: Vec<ParallelToolResultEntry>,
}

// Hold the lock guard for the duration of a tool execution.
// The inner guards are held for RAII purposes (dropped when the guard is dropped).
pub enum ToolExecGuard<'a> {
    Read(#[allow(dead_code)] tokio::sync::RwLockReadGuard<'a, ()>),
    Write(#[allow(dead_code)] tokio::sync::RwLockWriteGuard<'a, ()>),
}

// === Caller policy and errors ========================================

pub fn caller_type_for_tool_use(caller: Option<&ToolCaller>) -> &str {
    caller.map_or("direct", |c| c.caller_type.as_str())
}

/// Whether `caller` may invoke a tool whose definition carries an
/// `allowed_callers` list. Tri-state, all fail-closed:
/// - field absent → `direct` callers only (the default posture);
/// - field present but EMPTY (`allowed_callers: []`) → also `direct` only —
///   a config typo writing `[]` where the field should be omitted quietly
///   locks out every non-direct caller with no diagnostic, so prefer
///   omitting the field entirely;
/// - non-empty → exactly the listed caller types.
pub fn caller_allowed_for_tool(caller: Option<&ToolCaller>, tool_def: Option<&Tool>) -> bool {
    let requested = caller_type_for_tool_use(caller);
    if let Some(def) = tool_def
        && let Some(allowed) = &def.allowed_callers
    {
        if allowed.is_empty() {
            return requested == "direct";
        }
        return allowed.iter().any(|item| item == requested);
    }
    requested == "direct"
}

pub fn format_tool_error(err: &ToolError, tool_name: &str) -> String {
    match err {
        ToolError::InvalidInput { message } => {
            format!("Invalid input for tool '{tool_name}': {message}")
        }
        ToolError::MissingField { field } => {
            format!("Tool '{tool_name}' is missing required field '{field}'")
        }
        // `PathEscape` delegates to its `Display` on purpose: that string is
        // the one place carrying the remedy (`/trust add <path>`), and every
        // live channel already prints it verbatim. Restating the advice here
        // is how the two renderings drift apart.
        ToolError::PathEscape { .. } => err.to_string(),
        ToolError::ExecutionFailed { message } => message.clone(),
        ToolError::Timeout { seconds } => format!(
            "Tool '{tool_name}' timed out after {seconds}s. Try a narrower scope or a longer timeout."
        ),
        ToolError::NotAvailable { message } => {
            let lower = message.to_ascii_lowercase();
            if lower.contains("current tool catalog") || lower.contains("did you mean:") {
                message.clone()
            } else {
                format!(
                    "Tool '{tool_name}' is not available: {message}. Check mode, feature flags, or tool name."
                )
            }
        }
        ToolError::PermissionDenied { message } => format!(
            "Tool '{tool_name}' was denied: {message}. Adjust approval mode or request permission."
        ),
        // Same rule as `PathEscape`: the rejection's message is the one place
        // carrying the remedy (read the file, then retry), so every channel
        // prints the same Display instead of restating it.
        ToolError::Rejected { .. } => err.to_string(),
    }
}

fn normalize_parallel_tool_name(raw: &str) -> String {
    let mut name = raw.trim();
    for prefix in ["functions.", "tools.", "tool."] {
        if let Some(stripped) = name.strip_prefix(prefix) {
            name = stripped;
            break;
        }
    }
    name.to_string()
}

pub fn parse_parallel_tool_calls(
    input: &serde_json::Value,
) -> Result<Vec<(String, serde_json::Value)>, ToolError> {
    let tool_uses = input
        .get("tool_uses")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ToolError::missing_field("tool_uses"))?;
    if tool_uses.is_empty() {
        return Err(ToolError::invalid_input(
            "multi_tool_use.parallel requires at least one tool call",
        ));
    }

    let mut calls = Vec::with_capacity(tool_uses.len());
    for item in tool_uses {
        let name = item
            .get("recipient_name")
            .or_else(|| item.get("tool_name"))
            .or_else(|| item.get("name"))
            .or_else(|| item.get("tool"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::missing_field("recipient_name"))?;
        let params = match item
            .get("parameters")
            .or_else(|| item.get("input"))
            .or_else(|| item.get("args"))
            .or_else(|| item.get("arguments"))
        {
            Some(params) => params.clone(),
            None => {
                // Distinguish "legitimately omitted for a zero-arg tool"
                // from an unrecognized shape (e.g. a typo'd `params` key):
                // an item that carries any other content but no recognized
                // argument key is almost certainly malformed — visible,
                // not silently executed with `{}`.
                if item.as_object().is_some_and(|object| object.len() > 1) {
                    tracing::warn!(
                        target: "codesmith::dispatch",
                        "multi_tool_use.parallel entry carries no recognized argument key \
                         (parameters/input/args/arguments); executing with no arguments: {item}"
                    );
                }
                json!({})
            }
        };
        calls.push((normalize_parallel_tool_name(name), params));
    }

    Ok(calls)
}

// === Dispatch policy ==================================================

/// How many calls may share one concurrent batch.
///
/// The same order of magnitude as the sub-agent cap (`[subagents].
/// max_concurrent`, default 10): a model that emits forty `grep_files` calls
/// in one turn gets four groups of ten rather than forty concurrent
/// processes. The cap only delays the tail — groups stay in declaration order
/// — and it is deliberately not a config knob: fan-out width is a resource
/// question the runtime can answer, and `[subagents].max_concurrent` is
/// already the dial that matters for the expensive case.
pub const MAX_PARALLEL_BATCH: usize = 10;

/// Whether `plan` could run concurrently *at all*, before looking at what else
/// is in the batch. Group-level conflicts (two calls naming one file) are
/// [`plan_tool_execution_batches`]'s decision, not this one.
///
/// Read-only is the default posture — every read tool fans out, whether or not
/// it declares `supports_parallel`; the declaration is what a tool that is
/// **not** read-only needs in order to be considered at all (the sub-agent
/// spawn family, which names no workspace path). A file writer is eligible
/// with known target paths on top of that.
pub fn tool_plan_is_parallel_safe(plan: &ToolExecutionPlan) -> bool {
    if plan.interactive || plan.destructive {
        return false;
    }
    // A host question is a prompt, not a read: it must never be asked from a
    // concurrent batch (the answer channel has one waiter and the asks would
    // interleave). The dispatcher's `is_interactive` covers the production
    // registry; the name check keeps embeds without a dispatcher correct too.
    if plan.name == crate::engine::tool_catalog::REQUEST_USER_INPUT_NAME {
        return false;
    }
    // A writer that does not name its targets is never eligible: `exec_shell`
    // can write anywhere, and an `apply_patch` whose diff failed to parse
    // resolves no path. Both stay serial rather than race.
    if plan.writes_files && plan.touched_paths.is_empty() {
        return false;
    }
    plan.read_only || plan.writes_files || plan.supports_parallel
}

pub fn should_parallelize_tool_batch(plans: &[ToolExecutionPlan]) -> bool {
    if plans.is_empty() || !plans.iter().all(tool_plan_is_parallel_safe) {
        return false;
    }
    // Same conflict rule the grouping applies: one file, one writer per batch.
    let mut claimed: HashSet<&PathBuf> = HashSet::new();
    plans
        .iter()
        .flat_map(|plan| plan.touched_paths.iter())
        .all(|path| claimed.insert(path))
}

/// Whether a batch stays parallel-safe after one member's declared paths are
/// recomputed — an extension rewrote that call's input after
/// [`plan_tool_execution_batches`] classified the batch on the original.
///
/// Same two rules as [`should_parallelize_tool_batch`], applied with the
/// member's paths substituted: a writer that no longer names its targets is
/// never eligible, and one file gets one claim per batch.
pub fn batch_stays_parallel_safe_with(
    plans: &[ToolExecutionPlan],
    index: usize,
    rewritten_paths: &[PathBuf],
) -> bool {
    let Some(target) = plans.iter().find(|plan| plan.index == index) else {
        return false;
    };
    if target.writes_files && rewritten_paths.is_empty() {
        return false;
    }
    let mut claimed: HashSet<&PathBuf> = HashSet::new();
    plans
        .iter()
        .flat_map(|plan| {
            if plan.index == index {
                rewritten_paths.iter()
            } else {
                plan.touched_paths.iter()
            }
        })
        .all(|path| claimed.insert(path))
}

/// A concurrent batch being assembled, plus the facts the next call's join has
/// to be checked against.
#[derive(Default)]
struct ParallelGroup {
    plans: Vec<ToolExecutionPlan>,
    /// Every path the members name.
    paths: HashSet<PathBuf>,
    /// A member with no path information — `grep_files` over a tree, a
    /// sub-agent spawn. Its reach is unbounded, so no writer may follow it.
    unbounded_reader: bool,
    /// A member that writes the workspace. An unbounded reader may not follow
    /// one, for the same reason.
    writer: bool,
}

impl ParallelGroup {
    fn accepts(&self, plan: &ToolExecutionPlan) -> bool {
        if !tool_plan_is_parallel_safe(plan) || self.plans.len() >= MAX_PARALLEL_BATCH {
            return false;
        }
        // Never pair a writer with a reader whose reach we cannot bound, in
        // either order: that reader may be looking at the very file the writer
        // is rewriting, and a half-written file reads as an empty one.
        if plan.writes_files && self.unbounded_reader {
            return false;
        }
        if plan.touched_paths.is_empty() && self.writer {
            return false;
        }
        // Two calls naming one file run in declaration order or not at all.
        !plan
            .touched_paths
            .iter()
            .any(|path| self.paths.contains(path))
    }

    fn push(&mut self, plan: ToolExecutionPlan) {
        self.unbounded_reader |= plan.touched_paths.is_empty();
        self.writer |= plan.writes_files;
        self.paths.extend(plan.touched_paths.iter().cloned());
        self.plans.push(plan);
    }

    fn take(&mut self) -> Vec<ToolExecutionPlan> {
        self.paths.clear();
        self.unbounded_reader = false;
        self.writer = false;
        std::mem::take(&mut self.plans)
    }
}

/// Group the turn's calls into batches that already encode their ordering.
///
/// A batch runs concurrently inside itself, and every batch finishes before
/// the next starts — so the grouping *is* the ordering guarantee, and
/// declaration order survives whatever the group split does. A call that is
/// parallel-eligible but conflicts with the group it would join starts a
/// fresh group instead of degrading to a barrier, which is what keeps two
/// `apply_patch` calls on one file in declaration order while two calls on
/// different files still overlap. Grouping is greedy over consecutive plans
/// and never reorders to pack a batch, so a conflict costs parallelism, never
/// ordering. Calls that are not eligible at all (interactive, destructive, an
/// unnamed writer) become one-call `Serial` batches: a barrier, exactly as
/// before.
pub fn plan_tool_execution_batches(plans: Vec<ToolExecutionPlan>) -> Vec<ToolExecutionBatch> {
    let mut batches = Vec::new();
    let mut group = ParallelGroup::default();

    for plan in plans {
        if group.accepts(&plan) {
            group.push(plan);
            continue;
        }
        if !group.plans.is_empty() {
            batches.push(ToolExecutionBatch::Parallel(group.take()));
        }
        if tool_plan_is_parallel_safe(&plan) {
            group.push(plan);
        } else {
            batches.push(ToolExecutionBatch::Serial(Box::new(plan)));
        }
    }

    if !group.plans.is_empty() {
        batches.push(ToolExecutionBatch::Parallel(group.take()));
    }

    batches
}

/// Whether an approved plan should end the turn.
///
/// A `Plan` turn's registry carries no write tools, so once the user approves
/// the plan the turn has nothing left it can do — and the toolset the restored
/// mode brings is built at the *next* turn's start
/// (`handle_send_message` → `build_turn_dispatcher`). Stopping here is the
/// handoff `prompts/modes/plan.md` promises. A refused call is an `Err`, so the
/// `[r] <feedback>` revise loop keeps the turn alive with plan mode intact, and
/// a model-initiated exit inside an Agent turn does not stop anything: that
/// turn already has its write tools.
///
/// Known gaps, beside the behaviour it owns:
///
/// - `update_plan` also claims a handback in `plan.md`, and this predicate does
///   not cover it — the tool is used mid-investigation too (base.md's
///   composition pattern), so stopping on every successful `update_plan` in
///   Plan mode would cut the turn short.
/// - A capacity-controller intervention (`VerifyAndReplan`) in the same step
///   replaces the batch outcome and drops the flag for that step, so the turn
///   continues — the pre-existing behaviour, narrowed to that rare pairing.
pub fn should_stop_after_plan_tool(
    mode: AppMode,
    tool_name: &str,
    result: &Result<ToolResult, ToolError>,
) -> bool {
    mode == AppMode::Plan && tool_name == "exit_plan_mode" && result.is_ok()
}

pub fn should_force_update_plan_first(mode: AppMode, content: &str) -> bool {
    if mode != AppMode::Plan {
        return false;
    }

    let lower = content.to_ascii_lowercase();
    let asks_for_direct_plan = [
        "quick plan",
        "short plan",
        "simple plan",
        "3-step plan",
        "3 step plan",
        "three-step plan",
        "three step plan",
        "high-level plan",
        "high level plan",
        "give me a plan",
        "make a plan",
        "outline a plan",
        "draft a plan",
    ]
    .iter()
    .any(|needle| lower.contains(needle));

    if !asks_for_direct_plan {
        return false;
    }

    let asks_for_repo_exploration = [
        "inspect the repo",
        "inspect the code",
        "explore the repo",
        "search the repo",
        "read the code",
        "review the code",
        "analyze the code",
        "investigate",
        "look through",
        "understand the current",
        "ground it in the codebase",
        "based on the codebase",
    ]
    .iter()
    .any(|needle| lower.contains(needle));

    !asks_for_repo_exploration
}

/// MCP tools that are safe to run in parallel AND read-only. One shared
/// list — the two predicates are consulted from different modules
/// (`tool_execution.rs` vs `capacity_flow.rs`), so duplicated literals
/// would drift silently. Known limitation: classification is by bare tool
/// name; anything surfaced under one of these names gets read-only +
/// parallel-safe semantics regardless of what it actually does.
const MCP_PARALLEL_SAFE_TOOLS: &[&str] = &[
    "list_mcp_resources",
    "list_mcp_resource_templates",
    "mcp_read_resource",
    "read_mcp_resource",
    "mcp_get_prompt",
];

pub fn mcp_tool_is_parallel_safe(name: &str) -> bool {
    MCP_PARALLEL_SAFE_TOOLS.contains(&name)
}

pub fn mcp_tool_is_read_only(name: &str) -> bool {
    MCP_PARALLEL_SAFE_TOOLS.contains(&name)
}
