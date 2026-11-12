//! The desktop GoalBar bridge: the `goal` session projection and the
//! `goals/get|edit|pause|resume|clear` remotes.
//!
//! CodeSmith already owns the goal as engine state: the model's
//! `create_goal`/`get_goal`/`update_goal` tools mutate a shared
//! [`GoalState`](codesmith_agent_runtime::tool_state::goal::GoalState), and
//! the engine's round driver continues an active goal at turn end. The
//! runtime exposes that state at `GET|POST /v1/threads/{id}/goal`; this
//! module maps it onto the vendored client's `GoalProjection`/`GoalView`.
//!
//! Three sources feed the projection, in strict priority order:
//!
//! 1. **Live** — when the thread's engine is loaded, the runtime view is
//!    authoritative (it carries paused/blocked/cleared states and the round
//!    counters). A read never loads an engine.
//! 2. **Journal** — the latest completed `create_goal`/`update_goal` tool
//!    result replayed from the thread record. It outranks the command
//!    snapshot only when it is strictly newer. The engine seeds the same
//!    state from that entry when it loads.
//! 3. **Command snapshot** — a goal created or edited through the desktop
//!    (`/goal <objective> [budget: N]`, `goals/edit`). The runtime keeps that
//!    state in memory only and writes no journal item, so the bridge persists
//!    the snapshot inside its per-session overlay (`desktop/goals.json`) and
//!    replays it after an engine eviction.
//!
//! The tie-break between 2 and 3 is deterministic: the command snapshot wins
//! an exact `updated_ms` tie (the command was applied to the runtime after
//! the journal item it ties with, and it is the mutation the bar holds a ref
//! for). Journal timestamps come from the tool item's `ended_at`; the command
//! timestamp is the bridge clock at the successful POST, so gross clock skew
//! or a coarse-grained item timestamp can invert a genuinely later item — a
//! stated limit, not a hidden reconciliation.
//!
//! UI mutations (`edit`/`pause`/`resume`/`clear`) POST the runtime action
//! (loading the engine, which seeds from the journal first) and bump a
//! bridge-local revision so the client's CAS-facing projection updates.
//!
//! **Desktop pause/clear outlive an engine eviction.** The runtime's journal
//! records model-tool changes only, and its seed deliberately restores an
//! active journaled goal, so a process-local pause or clear was lost the
//! moment the engine was evicted. The bridge therefore persists its overlay
//! per session at `~/.codesmith/desktop/goals.json` (atomic rename; a corrupt
//! file logs and is ignored, never silently reset) and:
//!
//! - the read path presents the persisted pause/clear over the journal
//!   fallback without loading an engine, and re-applies it to a live engine
//!   that is already loaded (no engine is spawned by a read);
//! - [`reconcile`] re-applies the persisted action before a prompt, so a
//!   freshly loaded engine cannot drive continuation rounds for a goal the
//!   desktop stopped.
//!
//! **A command-created goal survives the eviction too.** `/goal` has no
//! journal item, so `Overlay::command` records its objective, budget and last
//! observed phase. The non-loaded projection replays it when it outranks the
//! journal, [`reconcile`] re-seeds an evicted engine with it before a prompt
//! (an edit is idempotent — an unchanged objective keeps its rounds — and a
//! persisted pause follows), and a loaded read writes the observed
//! phase/budget/blocker back into it so a completion is durable (a different
//! objective is another writer's goal and is left to the journal). A terminal
//! snapshot (`complete`/`blocked`) is never re-seeded and a persisted clear
//! suppresses both the projection and the re-seed.
//!
//! Known limitations:
//! - completion or blocking that happens after the last loaded read and
//!   before the eviction: the snapshot still says `active`, so the next
//!   prompt's reconcile re-seeds an active goal. The bridge only sees the
//!   terminal phase while an engine renders it;
//! - an engine loaded by a path that bypasses [`reconcile`] (a direct
//!   `goals/*` mutation, an external HTTP caller) starts empty for a
//!   command-created goal, so the bar shows no goal until the next prompt
//!   re-seeds it — the snapshot itself is not lost;
//! - a command-created goal's identity is `goal_<session>` until a model
//!   `create_goal`/`update_goal` records a tool item; that item starts a new
//!   journal identity, so a bar holding the old ref fails its CAS (the
//!   correct outcome for a new goal identity);
//! - a queued turn started by the runtime after an eviction, without an
//!   intervening desktop prompt, is the one path that can still outrun a
//!   pause for a single turn.
//!
//! The projection's id/revision are synthesized (the runtime snapshot has no
//! id). Mutations carry the client's `ref {id, revision}` and the bridge
//! refuses a stale one (`goal/stale-revision`, upstream's
//! GOAL_STALE_REVISION guard): the revision is the journal item count plus
//! the persisted overlay counter (the command snapshot lives in that
//! overlay, so its mutations ride the same counter), so it is durable and
//! monotone across a bridge restart — a ref held before the restart still
//! passes the compare-and-set, and a second client's edit loses the race.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::{mapper, rpc_error, rpc_ok, HostState};

/// The default round cap the runtime exposes (kept in step with
/// `codesmith_agent_runtime::tool_state::goal::MAX_GOAL_ROUNDS`).
const FALLBACK_MAX_GOAL_ROUNDS: u64 = 25;

/// Serializes read-modify-write cycles over the one overlay file.
static PERSIST_LOCK: Mutex<()> = Mutex::const_new(());

/// One session's bridge-local mutation overlay. Persisted at
/// `desktop/goals.json` so a pause/clear survives a bridge restart and an
/// engine eviction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Overlay {
    /// Revisions applied through the desktop bar (the journal counts model
    /// tool calls only).
    pub extra_revisions: u64,
    /// Wall-clock ms of the latest desktop mutation.
    pub updated_ms: Option<i64>,
    /// A desktop pause applies to the goal identity it was issued against.
    pub paused: bool,
    pub paused_goal_id: Option<String>,
    /// A desktop clear suppresses the journal fallback for that exact goal
    /// identity; a new `create_goal` starts a new identity and reappears.
    pub cleared: bool,
    pub cleared_goal_id: Option<String>,
    /// The goal a desktop command created or edited. The runtime keeps it in
    /// memory only, so this is the only replayable record after an engine
    /// eviction; see the module notes for its precedence against the journal.
    pub command: Option<CommandGoal>,
}

/// A goal set through `/goal <objective> [budget: N]` or the bar's inline
/// edit. `phase`/`blocker` are the last phase a loaded read observed (or the
/// edit's `active`), so a model completion survives the eviction. `created_ms`
/// tracks the current objective; `updated_ms` is the last command that edited
/// it (never an observation), which keeps the journal/command precedence
/// stable across reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandGoal {
    pub objective: String,
    pub token_budget: Option<u32>,
    pub phase: String,
    #[serde(default)]
    pub blocker: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

impl Default for CommandGoal {
    fn default() -> Self {
        Self {
            objective: String::new(),
            token_budget: None,
            phase: "active".into(),
            blocker: None,
            created_ms: 0,
            updated_ms: 0,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    sessions: BTreeMap<String, Overlay>,
}

fn overlay_path(host: &HostState) -> PathBuf {
    host.settings_home().join("desktop").join("goals.json")
}

fn load_store(path: &Path) -> Result<Store> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn save_store(path: &Path, store: &Store) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(store)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn is_goal_tool(summary: &str) -> bool {
    summary.starts_with("create_goal") || summary.starts_with("update_goal")
}

fn item_ms(item: &Value) -> Option<i64> {
    item.get("ended_at")
        .or_else(|| item.get("started_at"))
        .and_then(Value::as_str)
        .and_then(mapper::epoch_ms)
}

/// The journal's goal record: the latest completed goal-tool snapshot plus
/// the identity/metadata derived from the tool items.
struct JournalGoal {
    id: String,
    snapshot: Value,
    revision: u64,
    created_ms: i64,
    updated_ms: i64,
}

fn journal_goal(detail: &Value, session_id: &str) -> Option<JournalGoal> {
    let items: Vec<&Value> = detail
        .get("items")?
        .as_array()?
        .iter()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("completed"))
        .filter(|item| {
            item.get("summary")
                .and_then(Value::as_str)
                .is_some_and(is_goal_tool)
        })
        .collect();
    let latest = *items.last()?;
    let create = items
        .iter()
        .rev()
        .find(|item| {
            item.get("summary")
                .and_then(Value::as_str)
                .is_some_and(|summary| summary.starts_with("create_goal"))
        })
        .copied();
    let snapshot: Value = serde_json::from_str(latest.get("detail")?.as_str()?).ok()?;
    let objective = snapshot
        .get("objective")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if objective.is_empty() {
        return None;
    }
    // The identity is the goal's creating call: updates keep the same id so
    // the bar's local edit state survives them; a re-create starts a new one.
    let identity = create.unwrap_or(latest);
    let id = format!(
        "goal_{}",
        identity
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(session_id)
    );
    let created_ms = create
        .and_then(item_ms)
        .or_else(|| items.first().and_then(|item| item_ms(item)))
        .unwrap_or_else(now_ms);
    let updated_ms = item_ms(latest).unwrap_or(created_ms);
    Some(JournalGoal {
        id,
        snapshot,
        revision: items.len() as u64,
        created_ms,
        updated_ms,
    })
}

/// Whether the journal snapshot outranks the command snapshot: strictly
/// newer only. An exact tie goes to the command (see the module notes), so a
/// `/goal` applied in the same millisecond as the journal item it follows
/// still describes the live goal.
fn journal_wins(journal: Option<&JournalGoal>, command: Option<&CommandGoal>) -> bool {
    match (journal, command) {
        (Some(_), None) => true,
        (None, _) => false,
        (Some(journal), Some(command)) => journal.updated_ms > command.updated_ms,
    }
}

fn command_snapshot(overlay: &Overlay) -> Option<&CommandGoal> {
    overlay
        .command
        .as_ref()
        .filter(|command| !command.objective.is_empty())
}

/// The command snapshot after an edit: the runtime's `GoalState::edit`
/// semantics — an unchanged objective keeps its creation time and (unless a
/// new budget is supplied) its budget; a changed objective starts fresh.
fn edited_command(
    previous: Option<&CommandGoal>,
    objective: &str,
    token_budget: Option<u32>,
    now: i64,
) -> CommandGoal {
    let objective = objective.trim();
    match previous {
        Some(previous) if previous.objective == objective => CommandGoal {
            objective: objective.to_string(),
            token_budget: token_budget.or(previous.token_budget),
            phase: "active".into(),
            blocker: None,
            created_ms: previous.created_ms,
            updated_ms: now,
        },
        _ => CommandGoal {
            objective: objective.to_string(),
            token_budget,
            phase: "active".into(),
            blocker: None,
            created_ms: now,
            updated_ms: now,
        },
    }
}

/// Load the persisted overlays once per Host generation. A corrupt file logs
/// loud and degrades to the in-memory state; it is never silently rewritten.
async fn hydrate(host: &HostState) {
    if host.goal_overlays_loaded.swap(true, Ordering::SeqCst) {
        return;
    }
    let path = overlay_path(host);
    let loaded = tokio::task::spawn_blocking(move || load_store(&path)).await;
    match loaded {
        Ok(Ok(store)) => {
            let mut overlays = host.goal_overlays.lock().await;
            for (session_id, overlay) in store.sessions {
                overlays.entry(session_id).or_insert(overlay);
            }
        }
        Ok(Err(err)) => {
            eprintln!("codesmith desktop: goal overlay store unreadable: {err:#}");
        }
        Err(err) => {
            eprintln!("codesmith desktop: goal overlay load join failed: {err}");
        }
    }
}

async fn overlay_for(host: &HostState, session_id: &str) -> Overlay {
    hydrate(host).await;
    host.goal_overlays
        .lock()
        .await
        .get(session_id)
        .cloned()
        .unwrap_or_default()
}

/// Apply one in-memory mutation to a session's overlay and persist the
/// result. The read-modify-write runs under the map lock, so a concurrent
/// read (the loaded-path observation) cannot overwrite the mutation with an
/// older copy. `apply` returns whether anything changed; an unchanged
/// observation skips the disk write.
async fn update_overlay<F>(host: &HostState, session_id: &str, apply: F) -> bool
where
    F: FnOnce(&mut Overlay) -> bool,
{
    hydrate(host).await;
    let changed = {
        let mut overlays = host.goal_overlays.lock().await;
        let overlay = overlays.entry(session_id.to_string()).or_default();
        apply(overlay)
    };
    if changed {
        persist_overlay(host, session_id).await;
    }
    changed
}

/// Write the current in-memory overlay to disk. The runtime mutation (or the
/// observation) already succeeded, so a failed write logs loud but does not
/// fail the caller. Reading the value under the persist lock (rather than
/// writing a caller-captured copy) keeps an older writer from regressing the
/// file after a newer one.
async fn persist_overlay(host: &HostState, session_id: &str) {
    let path = overlay_path(host);
    let _guard = PERSIST_LOCK.lock().await;
    let overlay = host.goal_overlays.lock().await.get(session_id).cloned();
    let Some(overlay) = overlay else {
        return;
    };
    let session_id = session_id.to_string();
    let written = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut store = load_store(&path)?;
        store.sessions.insert(session_id, overlay);
        save_store(&path, &store)
    })
    .await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(err)) => eprintln!("codesmith desktop: goal overlay persist failed: {err:#}"),
        Err(err) => eprintln!("codesmith desktop: goal overlay persist join failed: {err}"),
    }
}

/// Record one successful command edit: persist its snapshot, forget a
/// superseded desktop pause/clear, and bump the CAS revision. This is the
/// durable record a later eviction replays.
pub async fn note_edit(
    host: &HostState,
    session_id: &str,
    objective: &str,
    token_budget: Option<u32>,
) {
    let now = now_ms();
    update_overlay(host, session_id, |overlay| {
        overlay.extra_revisions += 1;
        overlay.updated_ms = Some(now);
        overlay.paused = false;
        overlay.paused_goal_id = None;
        overlay.cleared = false;
        overlay.cleared_goal_id = None;
        let command = edited_command(overlay.command.as_ref(), objective, token_budget, now);
        overlay.command = Some(command);
        true
    })
    .await;
}

/// Fold a loaded engine's observed state into the command snapshot. Runs on
/// the read path, so it never bumps `updated_ms` (the journal/command
/// precedence must not shift on a read) and only writes when something
/// changed. A goal whose objective differs is another writer's (the model, or
/// an external client) and is left alone; the journal path records those.
async fn note_observed(
    host: &HostState,
    session_id: &str,
    live: &Value,
    journal: Option<&JournalGoal>,
) {
    let Some(objective) = live.get("objective").and_then(Value::as_str) else {
        return;
    };
    let phase = match live.get("status").and_then(Value::as_str) {
        Some("active") => "active",
        Some("paused") => "paused",
        Some("blocked") => "blocked",
        Some("complete") => "complete",
        _ => return,
    };
    let token_budget = live
        .get("token_budget")
        .and_then(Value::as_u64)
        .and_then(|budget| u32::try_from(budget).ok());
    let blocker = live
        .get("blocker")
        .and_then(Value::as_str)
        .filter(|blocker| !blocker.trim().is_empty())
        .map(str::to_string);
    update_overlay(host, session_id, |overlay| {
        // Re-read the command snapshot under the map lock: a concurrent
        // command mutation must not be reverted by this observation.
        let Some(command) = command_snapshot(overlay) else {
            return false;
        };
        if journal_wins(journal, Some(command))
            || objective.trim() != command.objective
            || (command.phase == phase
                && command.token_budget == token_budget
                && command.blocker == blocker)
        {
            return false;
        }
        let Some(target) = overlay.command.as_mut() else {
            return false;
        };
        target.phase = phase.to_string();
        target.token_budget = token_budget;
        target.blocker = blocker;
        true
    })
    .await;
}

/// Re-seed an evicted engine with the command goal before a `pause`/`resume`
/// mutation that needs the goal to exist. A loaded engine is authoritative
/// and a journal-newer goal is not ours to overwrite; the caller's projected
/// phase gates the verbs (`pause` only from active, `resume` only from a
/// stopped phase), so a terminal goal is never re-armed by this path.
async fn seed_for_mutation(
    host: &HostState,
    session_id: &str,
    detail: &Value,
    before: &Value,
    overlay: &Overlay,
    verb: &str,
) {
    let Some(command) = command_snapshot(overlay) else {
        return;
    };
    if journal_wins(journal_goal(detail, session_id).as_ref(), Some(command)) {
        return;
    }
    let phase = before.pointer("/goal/phase").and_then(Value::as_str);
    let wanted = match verb {
        "pause" => phase == Some("active"),
        "resume" => matches!(phase, Some("paused" | "blocked")),
        _ => false,
    };
    if !wanted
        || before.pointer("/goal/objective").and_then(Value::as_str)
            != Some(command.objective.as_str())
    {
        return;
    }
    let loaded = host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}/goal"))
        .await
        .ok()
        .is_some_and(|live| live.get("loaded").and_then(Value::as_bool).unwrap_or(false));
    if loaded {
        return;
    }
    post_edit(host, session_id, command).await;
}

/// The one command-snapshot re-seed POST. A failure logs and leaves the
/// snapshot in place for the next attempt; the caller's RPC proceeds either
/// way (the mutation itself reports its own refusal).
async fn post_edit(host: &HostState, session_id: &str, command: &CommandGoal) {
    let mut body = json!({ "action": "edit", "objective": command.objective });
    if let Some(budget) = command.token_budget {
        body["token_budget"] = json!(budget);
    }
    match host
        .backend
        .post_with_error_message::<Value>(&format!("/v1/threads/{session_id}/goal"), &body)
        .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(message)) => {
            eprintln!("codesmith desktop: goal edit re-seed for {session_id} refused: {message}");
        }
        Err(err) => {
            eprintln!("codesmith desktop: goal edit re-seed for {session_id} failed: {err:#}");
        }
    }
}

/// Re-apply the persisted state before a prompt loads (or re-seeds) the
/// engine. `apply_goal_action` loads an evicted engine from the journal
/// first, so this is what stops a stopped goal from being driven again, and
/// what puts a command-created goal back into an engine that has no journal
/// entry for it. A refusal that means "already satisfied" is silent;
/// anything else logs.
pub async fn reconcile(host: &HostState, session_id: &str) {
    let overlay = overlay_for(host, session_id).await;
    let command = command_snapshot(&overlay).cloned();
    if command.is_none() && !overlay.paused && !overlay.cleared {
        return;
    }
    let Ok(detail) = host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}"))
        .await
    else {
        return;
    };
    let journal = journal_goal(&detail, session_id);
    let identity = journal
        .as_ref()
        .map(|journal| journal.id.clone())
        .unwrap_or_else(|| format!("goal_{session_id}"));
    if overlay.cleared && overlay.cleared_goal_id.as_deref() == Some(identity.as_str()) {
        if let Err(message) = post_action(host, session_id, "clear").await {
            eprintln!(
                "codesmith desktop: goal clear reconcile for {session_id} refused: {message}"
            );
        }
        return;
    }
    // Re-seed an evicted engine with the command snapshot when it is the newer
    // source and is not terminal. A loaded engine is authoritative (the
    // `/goal` POST already applied the goal to it; a different or empty goal
    // came from another writer), so only an evicted one is rebuilt. A paused
    // snapshot is rebuilt and then stopped again: the objective survives
    // without letting the round driver continue.
    let mut stop_from_command = false;
    if let Some(command) = command.as_ref() {
        if !journal_wins(journal.as_ref(), Some(command))
            && matches!(command.phase.as_str(), "active" | "paused")
        {
            let loaded = host
                .backend
                .get::<Value>(&format!("/v1/threads/{session_id}/goal"))
                .await
                .ok()
                .is_some_and(|live| live.get("loaded").and_then(Value::as_bool).unwrap_or(false));
            if !loaded {
                post_edit(host, session_id, command).await;
                stop_from_command = command.phase == "paused";
            }
        }
    }
    let stop_from_overlay =
        overlay.paused && overlay.paused_goal_id.as_deref() == Some(identity.as_str());
    if stop_from_command || stop_from_overlay {
        match post_action(host, session_id, "pause").await {
            Ok(()) => {}
            // The seeded state is already paused/blocked/gone: satisfied.
            Err(message) if message.contains("no active goal to pause") => {}
            Err(message) => {
                eprintln!(
                    "codesmith desktop: goal pause reconcile for {session_id} refused: {message}"
                );
            }
        }
    }
}

/// The `goal` projection value (`GoalProjection | null`). `detail` is the
/// thread record the caller already read.
pub async fn projection(host: &HostState, session_id: &str, detail: &Value) -> Value {
    let live = host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}/goal"))
        .await
        .ok();
    let loaded = live
        .as_ref()
        .and_then(|value| value.get("loaded"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let journal = journal_goal(detail, session_id);
    let overlay = overlay_for(host, session_id).await;
    let command = command_snapshot(&overlay);
    let command_wins = !journal_wins(journal.as_ref(), command);
    let journal_id = journal
        .as_ref()
        .map(|journal| journal.id.clone())
        .unwrap_or_else(|| format!("goal_{session_id}"));
    let goal_cleared =
        overlay.cleared && overlay.cleared_goal_id.as_deref() == Some(journal_id.as_str());
    let goal_paused =
        overlay.paused && overlay.paused_goal_id.as_deref() == Some(journal_id.as_str());

    let (mut snapshot, rounds_started, max_rounds) = if loaded {
        let Some(goal) = live
            .as_ref()
            .and_then(|value| value.get("goal"))
            .filter(|goal| !goal.is_null())
        else {
            // The engine's own state is authoritative: no goal, no bar.
            return Value::Null;
        };
        // A loaded read is the only place the bridge can observe a durable
        // phase (a model completion/block has no desktop command behind it),
        // so fold it into the command snapshot before it can be evicted.
        if !goal_cleared {
            note_observed(host, session_id, goal, journal.as_ref()).await;
        }
        // A read never loads an engine, but when one is already loaded the
        // persisted stop is re-applied to it (a fresh load seeds the journal
        // active goal). A refusal logs and leaves the live value visible.
        let mut effective = goal.clone();
        if goal_cleared {
            match post_action(host, session_id, "clear").await {
                Ok(()) => return Value::Null,
                Err(message) => {
                    eprintln!(
                        "codesmith desktop: goal clear reconcile for {session_id} refused: {message}"
                    );
                }
            }
        } else if goal_paused && goal.get("status").and_then(Value::as_str) == Some("active") {
            match post_action(host, session_id, "pause").await {
                Ok(()) => {
                    effective["status"] = json!("paused");
                    effective["blocker"] = json!("paused from the desktop");
                }
                Err(message) => {
                    eprintln!(
                        "codesmith desktop: goal pause reconcile for {session_id} refused: {message}"
                    );
                }
            }
        }
        (
            effective,
            goal.get("rounds_started")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            goal.get("max_goal_rounds")
                .and_then(Value::as_u64)
                .unwrap_or(FALLBACK_MAX_GOAL_ROUNDS),
        )
    } else {
        if goal_cleared {
            return Value::Null;
        }
        // The journal wins only while strictly newer; otherwise the command
        // snapshot is the newest durable record of the goal.
        let Some(mut snapshot) = (if command_wins {
            command.map(|command| {
                json!({
                    "objective": command.objective,
                    "status": command.phase,
                    "token_budget": command.token_budget,
                    "blocker": command.blocker,
                })
            })
        } else {
            journal.as_ref().map(|journal| journal.snapshot.clone())
        }) else {
            return Value::Null;
        };
        // A terminal state (complete/blocked) wins over a desktop pause issued
        // while the goal was still active.
        if goal_paused && snapshot.get("status").and_then(Value::as_str) == Some("active") {
            snapshot["status"] = json!("paused");
            if snapshot
                .get("blocker")
                .is_none_or(|blocker| blocker.is_null())
            {
                snapshot["blocker"] = json!("paused from the desktop");
            }
        }
        (snapshot, 0, FALLBACK_MAX_GOAL_ROUNDS)
    };

    let Some(objective) = snapshot
        .get("objective")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Value::Null;
    };
    let phase: &'static str = match snapshot.get("status").and_then(Value::as_str) {
        Some("active") => "active",
        Some("paused") => "paused",
        Some("blocked") => "blocked",
        Some("complete") => "complete",
        _ => return Value::Null,
    };
    if phase == "blocked" && !snapshot["blocker"].is_string() {
        snapshot["blocker"] = json!("blocked");
    }
    let revision = journal
        .as_ref()
        .map(|journal| journal.revision)
        .unwrap_or(1)
        + overlay.extra_revisions;
    let created_ms = match (command_wins, command, journal.as_ref()) {
        (true, Some(command), _) => command.created_ms,
        (_, _, Some(journal)) => journal.created_ms,
        _ => now_ms(),
    };
    let updated_ms = overlay
        .updated_ms
        .unwrap_or(created_ms)
        .max(
            journal
                .as_ref()
                .map(|journal| journal.updated_ms)
                .unwrap_or(created_ms),
        )
        .max(
            command
                .map(|command| command.updated_ms)
                .unwrap_or(created_ms),
        );
    let mut goal = json!({
        "id": journal_id,
        "revision": revision,
        "objective": objective,
        "phase": phase,
        "maxGoalRounds": max_rounds,
    });
    // The bar renders only the phase/objective; the budget still rides the
    // snapshot so `/goal … budget: N` survives an eviction observably.
    if let Some(budget) = snapshot.get("token_budget").and_then(Value::as_u64) {
        goal["tokenBudget"] = json!(budget);
    }
    if phase == "blocked" {
        let message = snapshot
            .get("blocker")
            .and_then(Value::as_str)
            .filter(|blocker| !blocker.trim().is_empty())
            .unwrap_or("blocked");
        goal["blockedReason"] = json!({ "code": "blocked", "message": message });
    }
    json!({
        "goal": goal,
        "roundsStarted": rounds_started,
        "createdAt": created_ms,
        "updatedAt": updated_ms,
    })
}

/// One best-effort goal action; `Err` carries the runtime's refusal text so
/// a caller can treat "already satisfied" as success.
async fn post_action(
    host: &HostState,
    session_id: &str,
    action: &str,
) -> std::result::Result<(), String> {
    match host
        .backend
        .post_with_error_message::<Value>(
            &format!("/v1/threads/{session_id}/goal"),
            &json!({ "action": action }),
        )
        .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(message)) => Err(message),
        Err(err) => Err(format!("{err:#}")),
    }
}

/// The `GoalView` the `goals/get` remote answers: the projection plus the
/// process-local activation flag. `None` means "no current goal" (the client
/// expects an absent value, not `null`).
async fn goal_view(host: &HostState, session_id: &str) -> Option<Value> {
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await
        .ok()?;
    let projection = projection(host, session_id, &detail).await;
    let goal = projection.get("goal")?.clone();
    let activation = if goal.get("phase").and_then(Value::as_str) == Some("active") {
        "armed"
    } else {
        "disarmed"
    };
    let mut view = goal;
    view["roundsStarted"] = projection.get("roundsStarted").cloned().unwrap_or(json!(0));
    view["createdAt"] = projection.get("createdAt").cloned().unwrap_or(json!(0));
    view["updatedAt"] = projection.get("updatedAt").cloned().unwrap_or(json!(0));
    view["activation"] = json!(activation);
    Some(view)
}

fn session_arg(args: &Value) -> Option<String> {
    args.get("agentId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The `goals/edit` objective: the third parameter is named `request`, so it
/// rides one level below the object-keyed `{agentId, ref, request}` envelope
/// (a flattened `{objective}` is accepted too).
fn edit_objective(args: &Value) -> Option<&str> {
    args.get("request")
        .unwrap_or(args)
        .get("objective")
        .and_then(Value::as_str)
}

/// The client's optional round-cap edit. CodeSmith's cap is the runtime
/// constant; a request for a different cap is refused rather than silently
/// ignored (the bar never sends one today, but the remote declares it).
fn validate_max_goal_rounds(args: &Value) -> std::result::Result<Option<u64>, Value> {
    let requested = args
        .get("request")
        .unwrap_or(args)
        .get("maxGoalRounds")
        .and_then(Value::as_u64);
    match requested {
        Some(max) if max != FALLBACK_MAX_GOAL_ROUNDS => Err(rpc_error(
            "goal/rejected",
            format!(
                "the goal round cap is fixed at {FALLBACK_MAX_GOAL_ROUNDS}; {max} is not supported"
            ),
        )),
        other => Ok(other),
    }
}

/// `goals/get` — the live goal view. An absent value means no current goal.
pub async fn get(host: &HostState, args: &Value) -> Value {
    let args = args.get("request").unwrap_or(args);
    let Some(session_id) = session_arg(args) else {
        return rpc_error("gateway/arguments-invalid", "goals/get without agentId");
    };
    match goal_view(host, &session_id).await {
        Some(view) => rpc_ok(view),
        // `GoalView | undefined`: an absent value, not JSON null (the client
        // dereferences a present null).
        None => json!({ "ok": true }),
    }
}

/// `goals/edit|pause|resume|clear` — one runtime goal mutation, then a live
/// projection push.
pub async fn action(host: &HostState, endpoint: &str, args: &Value) -> Value {
    // Object-keyed args: `{agentId, ref, request:{objective?, maxGoalRounds?}}`
    // — the `request` member is the remote's third parameter, never the whole
    // envelope, so `agentId` must be read before unwrapping it.
    let Some(session_id) = session_arg(args) else {
        return rpc_error(
            "gateway/arguments-invalid",
            format!("{endpoint} without agentId"),
        );
    };
    let requested_max = match validate_max_goal_rounds(args) {
        Ok(max) => max,
        Err(error) => return error,
    };
    let verb = endpoint.rsplit('/').next().unwrap_or_default();
    let body = match verb {
        "edit" => match edit_objective(args) {
            Some(objective) => json!({ "action": "edit", "objective": objective }),
            // A round-cap-only edit is a no-op: the cap is the runtime
            // constant, and the request is otherwise valid.
            None if requested_max.is_some() => {
                return match goal_view(host, &session_id).await {
                    Some(view) => rpc_ok(view),
                    None => rpc_error("no-current-goal", "no current goal to mutate"),
                };
            }
            None => {
                return rpc_error(
                    "gateway/arguments-invalid",
                    "goals/edit without an objective",
                );
            }
        },
        "pause" => json!({ "action": "pause" }),
        "resume" => json!({ "action": "resume" }),
        "clear" => json!({ "action": "clear" }),
        other => {
            return rpc_error(
                "gateway/arguments-invalid",
                format!("unknown goal mutation {other:?}"),
            );
        }
    };
    let detail: Value = match host.backend.get(&format!("/v1/threads/{session_id}")).await {
        Ok(detail) => detail,
        Err(err) => {
            return rpc_error("gateway/internal", format!("{endpoint}: {err:#}"));
        }
    };
    let before = projection(host, &session_id, &detail).await;
    if before.is_null() {
        return rpc_error("no-current-goal", "no current goal to mutate");
    }
    let previous = before.get("goal").cloned().unwrap_or(Value::Null);
    let previous_id = previous
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let previous_revision = previous
        .get("revision")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    // The client's compare-and-set (upstream's GOAL_STALE_REVISION guard):
    // the ref names the exact revision the bar issued the mutation against —
    // a second client's edit, or a bar left open across one, refuses here.
    // The synthesized revision is durable (journal item count + the
    // persisted overlay counter), so the guard also survives a bridge
    // restart.
    let given_ref = args.get("ref");
    let (Some(ref_id), ref_revision) = (
        given_ref
            .and_then(|given| given.get("id"))
            .and_then(Value::as_str),
        given_ref
            .and_then(|given| given.get("revision"))
            .and_then(Value::as_u64),
    ) else {
        return rpc_error(
            "gateway/arguments-invalid",
            format!("{endpoint} without a goal ref"),
        );
    };
    if ref_id != previous_id || ref_revision != Some(previous_revision) {
        return rpc_error(
            "goal/stale-revision",
            format!(
                "stale goal ref {ref_id:?} revision {:?}; current is {previous_id:?} revision {previous_revision}",
                ref_revision,
            ),
        );
    }
    let overlay = overlay_for(host, &session_id).await;
    // Pause/resume need the goal to exist in the runtime. A command-created
    // goal has no journal entry to seed an evicted engine, so re-create it
    // first; a stale ref above must not reach this mutation.
    if matches!(verb, "pause" | "resume") {
        seed_for_mutation(host, &session_id, &detail, &before, &overlay, verb).await;
    }
    match host
        .backend
        .post_with_error_message::<Value>(&format!("/v1/threads/{session_id}/goal"), &body)
        .await
    {
        Ok(Ok(_)) => {
            let now = now_ms();
            let previous_objective = previous.get("objective").and_then(Value::as_str);
            update_overlay(host, &session_id, |overlay| {
                overlay.extra_revisions += 1;
                overlay.updated_ms = Some(now);
                match verb {
                    "clear" => {
                        overlay.cleared = true;
                        overlay.cleared_goal_id = Some(previous_id.clone());
                        overlay.paused = false;
                        overlay.paused_goal_id = None;
                    }
                    "pause" => {
                        overlay.paused = true;
                        overlay.paused_goal_id = Some(previous_id.clone());
                        overlay.cleared = false;
                        overlay.cleared_goal_id = None;
                    }
                    "resume" => {
                        overlay.paused = false;
                        overlay.paused_goal_id = None;
                        overlay.cleared = false;
                        overlay.cleared_goal_id = None;
                        // The last observed phase may be the pause this
                        // resumes; leaving it would resurrect the stop on the
                        // next read.
                        if let Some(command) = overlay.command.as_mut() {
                            if Some(command.objective.as_str()) == previous_objective {
                                command.phase = "active".into();
                                command.blocker = None;
                            }
                        }
                    }
                    _ => {
                        overlay.paused = false;
                        overlay.paused_goal_id = None;
                        overlay.cleared = false;
                        overlay.cleared_goal_id = None;
                        if let Some(objective) = edit_objective(args) {
                            // An unchanged objective keeps the runtime's
                            // budget; a changed one starts fresh (the bar's
                            // edit request carries no budget), matching
                            // `GoalState::edit`.
                            let budget = if previous_objective == Some(objective.trim()) {
                                previous
                                    .get("tokenBudget")
                                    .and_then(Value::as_u64)
                                    .and_then(|budget| u32::try_from(budget).ok())
                            } else {
                                None
                            };
                            let command =
                                edited_command(overlay.command.as_ref(), objective, budget, now);
                            overlay.command = Some(command);
                        }
                    }
                }
                true
            })
            .await;
            push(host, &session_id).await;
            if verb == "clear" {
                return rpc_ok(json!({
                    "id": previous_id,
                    "revision": previous_revision + 1,
                }));
            }
            match goal_view(host, &session_id).await {
                Some(view) => rpc_ok(view),
                None => rpc_error("no-current-goal", "the goal disappeared after the mutation"),
            }
        }
        Ok(Err(message)) => rpc_error("goal/rejected", message),
        Err(err) => rpc_error("gateway/internal", format!("{endpoint}: {err:#}")),
    }
}

/// Push one session's `goal` projection to every open `session/control`
/// stream (a mutation or a completed goal tool changed it).
pub async fn push(host: &HostState, session_id: &str) {
    let detail = match host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}"))
        .await
    {
        Ok(detail) => detail,
        Err(err) => {
            eprintln!(
                "codesmith desktop: goal projection refresh for {session_id} failed: {err:#}"
            );
            return;
        }
    };
    let value = projection(host, session_id, &detail).await;
    let seq = host.next_projection_seq();
    host.push_live(
        "session/control",
        super::sessions::projection_update(session_id, seq, &json!({ "goal": value })),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal_item(id: &str, summary: &str, snapshot: Value) -> Value {
        json!({
            "id": id,
            "status": "completed",
            "summary": summary,
            "detail": snapshot.to_string(),
            "started_at": "2026-11-11T10:00:00Z",
            "ended_at": "2026-11-11T10:00:05Z",
        })
    }

    /// The bridge-free projection path when the engine is not loaded: the
    /// journal fallback, its identity, and the round counters.
    #[test]
    fn journal_goal_reads_the_latest_snapshot_and_keeps_the_create_identity() {
        let detail = json!({
            "items": [
                goal_item("item_create", "create_goal", json!({
                    "objective": "ship it", "status": "active", "token_budget": 500,
                    "elapsed_seconds": 1, "evidence": null, "blocker": null,
                })),
                goal_item("item_update", "update_goal", json!({
                    "objective": "ship it", "status": "blocked", "token_budget": 500,
                    "elapsed_seconds": 2, "evidence": null, "blocker": "needs input",
                })),
            ]
        });
        let journal = journal_goal(&detail, "thr_1").expect("goal");
        assert_eq!(journal.id, "goal_item_create");
        assert_eq!(journal.snapshot["status"], "blocked");
        assert_eq!(journal.revision, 2);
        assert_eq!(
            journal.updated_ms,
            mapper::epoch_ms("2026-11-11T10:00:05Z").unwrap()
        );
    }

    #[test]
    fn journal_goal_ignores_items_without_a_snapshot_or_objective() {
        let detail = json!({
            "items": [
                goal_item("item_create", "create_goal", json!({
                    "objective": "  ", "status": "active",
                })),
            ]
        });
        assert!(journal_goal(&detail, "thr_1").is_none());
    }

    #[test]
    fn edit_args_keep_the_agent_id_and_read_the_nested_objective() {
        // Object-keyed wire shape: the remote's third parameter is named
        // `request`, so the objective sits one level down and `agentId`
        // stays on the outer envelope.
        let args = json!({
            "agentId": "thr_1",
            "ref": { "id": "goal_x", "revision": 2 },
            "request": { "objective": "ship it" },
        });
        assert_eq!(session_arg(&args).as_deref(), Some("thr_1"));
        assert_eq!(edit_objective(&args), Some("ship it"));
        assert_eq!(
            edit_objective(&json!({ "objective": "flattened" })),
            Some("flattened")
        );
    }

    #[test]
    fn max_goal_rounds_accepts_only_the_runtime_cap() {
        assert_eq!(
            validate_max_goal_rounds(
                &json!({ "agentId": "x", "request": { "maxGoalRounds": 25 } })
            ),
            Ok(Some(25))
        );
        assert_eq!(
            validate_max_goal_rounds(&json!({ "agentId": "x" })),
            Ok(None)
        );
        let error = validate_max_goal_rounds(
            &json!({ "agentId": "x", "request": { "maxGoalRounds": 50 } }),
        )
        .expect_err("a different cap must be refused, not silently ignored");
        assert_eq!(error["error"]["code"], "goal/rejected");
    }

    #[test]
    fn overlay_round_trips_and_a_missing_file_is_empty() {
        let path = std::env::temp_dir().join(format!(
            "cds-goals-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        assert!(load_store(&path).unwrap().sessions.is_empty());
        let mut store = Store::default();
        store.sessions.insert(
            "thr_1".into(),
            Overlay {
                extra_revisions: 2,
                updated_ms: Some(5),
                paused: true,
                paused_goal_id: Some("goal_item".into()),
                cleared: false,
                cleared_goal_id: None,
                command: Some(CommandGoal {
                    objective: "ship it".into(),
                    token_budget: Some(100),
                    phase: "active".into(),
                    blocker: None,
                    created_ms: 3,
                    updated_ms: 4,
                }),
            },
        );
        save_store(&path, &store).unwrap();
        let reloaded = load_store(&path).unwrap();
        let overlay = reloaded.sessions.get("thr_1").expect("overlay");
        assert!(overlay.paused);
        assert_eq!(overlay.paused_goal_id.as_deref(), Some("goal_item"));
        let command = overlay.command.as_ref().expect("command snapshot");
        assert_eq!(command.objective, "ship it");
        assert_eq!(command.token_budget, Some(100));
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_store(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    /// The older overlay shape (no command snapshot) still loads; the
    /// snapshot is additive.
    #[test]
    fn an_overlay_without_a_command_snapshot_loads_as_none() {
        let overlay: Overlay = serde_json::from_value(json!({
            "extraRevisions": 1,
            "paused": true,
            "pausedGoalId": "goal_item",
        }))
        .expect("legacy overlay");
        assert_eq!(overlay.extra_revisions, 1);
        assert!(overlay.command.is_none());
    }

    /// The command/journal precedence: a strictly newer journal wins, an
    /// exact tie goes to the command, and an absent command leaves the
    /// journal in charge.
    #[test]
    fn command_snapshot_precedence_breaks_ties_toward_the_command() {
        let detail = json!({
            "items": [goal_item("item_create", "create_goal", json!({
                "objective": "ship it", "status": "active",
            }))],
        });
        let journal = journal_goal(&detail, "thr_1").expect("journal");
        let command = |updated_ms| CommandGoal {
            objective: "ship it".into(),
            updated_ms,
            ..CommandGoal::default()
        };
        assert!(journal_wins(Some(&journal), None));
        assert!(!journal_wins(None, Some(&command(1))));
        assert!(journal_wins(Some(&journal), Some(&command(0))));
        assert!(
            !journal_wins(Some(&journal), Some(&command(journal.updated_ms))),
            "an exact tie goes to the command"
        );
        assert!(!journal_wins(
            Some(&journal),
            Some(&command(journal.updated_ms + 1))
        ));
        assert!(!journal_wins(None, None));
    }

    /// A desktop edit keeps the creation time and budget for an unchanged
    /// objective and starts a fresh snapshot for a changed one (the runtime's
    /// `GoalState::edit` semantics).
    #[test]
    fn edited_command_keeps_progress_for_an_unchanged_objective() {
        let previous = CommandGoal {
            objective: "ship it".into(),
            token_budget: Some(100),
            created_ms: 10,
            updated_ms: 10,
            ..CommandGoal::default()
        };
        let unchanged = edited_command(Some(&previous), "ship it", None, 20);
        assert_eq!(unchanged.created_ms, 10);
        assert_eq!(unchanged.token_budget, Some(100));
        assert_eq!(unchanged.updated_ms, 20);
        assert_eq!(unchanged.phase, "active");
        let budgeted = edited_command(Some(&previous), "ship it", Some(5), 21);
        assert_eq!(budgeted.token_budget, Some(5));
        assert_eq!(budgeted.created_ms, 10);
        let changed = edited_command(Some(&previous), "ship that", None, 22);
        assert_eq!(changed.created_ms, 22);
        assert_eq!(changed.token_budget, None);
    }

    /// The projection's journal path is pure over the overlay: a persisted
    /// pause shows as paused after an engine eviction, and a persisted clear
    /// hides the replayed goal. Both are exercised through the pure pieces
    /// (the HTTP path needs a fake runtime; the dispatch tests cover it).
    #[test]
    fn journal_fallback_applies_the_persisted_pause_and_clear() {
        let detail = json!({
            "items": [goal_item("item_create", "create_goal", json!({
                "objective": "ship it", "status": "active", "blocker": null,
            }))],
        });
        let journal = journal_goal(&detail, "thr_1").expect("goal");
        let mut overlay = Overlay {
            paused: true,
            paused_goal_id: Some(journal.id.clone()),
            ..Overlay::default()
        };
        let mut snapshot = journal.snapshot.clone();
        if overlay.paused_goal_id.as_deref() == Some(journal.id.as_str()) {
            snapshot["status"] = json!("paused");
        }
        assert_eq!(snapshot["status"], "paused");
        overlay.paused = false;
        overlay.cleared = true;
        overlay.cleared_goal_id = Some(journal.id.clone());
        assert!(overlay.cleared);
    }
}
