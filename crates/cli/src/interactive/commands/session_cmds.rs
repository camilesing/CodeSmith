//! Session lifecycle commands for the REPL — port of `commands/session.rs`
//! and `commands/rename.rs` against [`ReplState`]. Where the TUI returned
//! `AppAction`s for its event loop to apply, the REPL applies the engine
//! op directly.

use colored::Colorize;

use super::ReplState;
use crate::core::ops::Op;
use crate::models::{Message, SystemPrompt};
use crate::session_manager::{
    SessionManager, create_saved_session_with_id_and_mode, create_saved_session_with_mode,
    truncate_id, update_session,
};

const MAX_TITLE_LEN: usize = 100;

fn sync_cost_to_metadata(
    state: &ReplState,
    metadata: &mut crate::session_manager::SessionMetadata,
) {
    metadata.cost.session_cost_usd = state.session.session_cost;
    metadata.cost.session_cost_cny = state.session.session_cost_cny;
    metadata.cost.subagent_cost_usd = state.session.subagent_cost;
    metadata.cost.subagent_cost_cny = state.session.subagent_cost_cny;
    metadata.cost.displayed_cost_high_water_usd = state.session.displayed_cost_high_water;
    metadata.cost.displayed_cost_high_water_cny = state.session.displayed_cost_high_water_cny;
    metadata.cumulative_turn_secs = state.cumulative_turn_duration.as_secs();
}

fn build_saved_session(state: &ReplState) -> crate::session_manager::SavedSession {
    let mut session = create_saved_session_with_mode(
        &state.api_messages,
        &state.model,
        &state.workspace,
        u64::from(state.session.total_tokens),
        state.system_prompt.as_ref(),
        Some(state.mode.label()),
    );
    sync_cost_to_metadata(state, &mut session.metadata);
    // Round-3 TODO-42 — failed-attempt bookkeeping rides the explicit save
    // too, so `/save` and the turn-boundary autosave persist the same record.
    session.failed_attempts = state.failed_attempts.clone();
    session
}

/// `/save [path]` — save the session to a file (managed dir by default).
pub fn save(state: &mut ReplState, path: Option<&str>) {
    let save_path = match path {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let dir = crate::session_manager::default_sessions_dir()
                .unwrap_or_else(|_| state.workspace.clone());
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            dir.join(format!("session_{timestamp}.json"))
        }
    };
    let session = build_saved_session(state);
    let sessions_dir = save_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| state.workspace.clone(), std::path::Path::to_path_buf);

    if let Err(e) = std::fs::create_dir_all(&sessions_dir) {
        println!("{} failed to create directory: {e}", "✗".red());
        return;
    }
    let json = match serde_json::to_string_pretty(&session) {
        Ok(j) => j,
        Err(e) => {
            println!("{} failed to serialize session: {e}", "✗".red());
            return;
        }
    };
    match std::fs::write(&save_path, json) {
        Ok(()) => {
            state.current_session_id = Some(session.metadata.id.clone());
            println!(
                "session saved to {} (ID: {})",
                save_path.display(),
                truncate_id(&session.metadata.id)
            );
        }
        Err(e) => println!("{} failed to save session: {e}", "✗".red()),
    }
}

/// `/fork` — save the current conversation and fork it into a new sibling.
pub async fn fork(state: &mut ReplState) {
    branch(state, None).await;
}

/// `/branch [name]` — branch the conversation here and switch to the copy.
///
/// Claude Code's `/branch`: the session as it stands is saved, a copy is made
/// and becomes the live session, and the original stays on disk for
/// `/resume`. `/fork` is the same operation under the name this REPL had
/// first; Claude Code's `/fork` — running the copy as a *detached background
/// session* — needs a session-IPC substrate this REPL does not have.
///
/// A name is applied to the copy before it is saved, so `/resume <name>`
/// finds the branch rather than the branch point.
///
/// Known limitations (recorded beside the behaviour it owns):
///
/// - The copy is a conversation branch, not a `git` branch: nothing in the
///   working tree is copied, stashed, or checked out.
/// - The branch point is the whole conversation, not a message index: there
///   is no "branch from that earlier message" (use `/rewind`'s snapshot list
///   for the file side, or `/sessions` to resume the original).
pub async fn branch(state: &mut ReplState, name: Option<&str>) {
    let name = name.map(str::trim).filter(|s| !s.is_empty());
    if let Some(name) = name
        && name.chars().count() > MAX_TITLE_LEN
    {
        println!(
            "{} branch name too long (max {MAX_TITLE_LEN} characters)",
            "✗".red()
        );
        return;
    }
    if state.api_messages.is_empty() {
        println!(
            "{} nothing to fork — send or load a message first",
            "✗".red()
        );
        return;
    }
    let Ok(manager) = SessionManager::default_location() else {
        println!("{} could not open sessions directory", "✗".red());
        return;
    };
    let parent_id = state
        .current_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    // The branch point keeps whatever the stored session already holds — its
    // title (from `/rename` or an earlier `/branch <name>`), its `created_at`,
    // and its own `parent_session_id`. Rebuilding the metadata from state
    // instead wrote those fields out: branching twice in a row silently
    // unnamed the first branch.
    let mut parent = manager.load_session(&parent_id).map_or_else(
        |_| {
            create_saved_session_with_id_and_mode(
                parent_id,
                &state.api_messages,
                &state.model,
                &state.workspace,
                u64::from(state.session.total_tokens),
                state.system_prompt.as_ref(),
                Some(state.mode.label()),
            )
        },
        |existing| {
            update_session(
                existing,
                &state.api_messages,
                u64::from(state.session.total_tokens),
                state.system_prompt.as_ref(),
            )
        },
    );
    sync_cost_to_metadata(state, &mut parent.metadata);
    if let Err(err) = manager.save_session(&parent) {
        println!("{} failed to save parent session: {err}", "✗".red());
        return;
    }
    let mut forked = build_saved_session(state);
    forked.metadata.copy_cost_from(&parent.metadata);
    forked.metadata.mark_forked_from(&parent.metadata);
    if let Some(name) = name {
        forked.metadata.title = name.to_string();
    }
    if let Err(err) = manager.save_session(&forked) {
        println!("{} failed to save forked session: {err}", "✗".red());
        return;
    }
    state.current_session_id = Some(forked.metadata.id.clone());
    let fork_id = forked.metadata.id.clone();
    match name {
        Some(name) => println!(
            "branched session {} -> {fork_id} ({name}) — the original stays for /resume",
            truncate_id(&parent.metadata.id),
        ),
        None => println!(
            "forked session {} -> {} — the original stays for /resume",
            truncate_id(&parent.metadata.id),
            truncate_id(&fork_id)
        ),
    }
    if let Ok(engine) = state.engine() {
        let _ = engine
            .send(Op::SyncSession {
                session_id: Some(fork_id),
                messages: state.api_messages.clone(),
                system_prompt: state.system_prompt.clone(),
                system_prompt_override: false,
                model: state.model.clone(),
                workspace: state.workspace.clone(),
            })
            .await;
    }
}

/// `/new [--force]` — start a fresh session.
pub async fn new_session(state: &mut ReplState, arg: Option<&str>) {
    let force = match arg.map(str::trim).filter(|s| !s.is_empty()) {
        None => false,
        Some("--force" | "force") => true,
        Some(other) => {
            println!("{}/new [--force] — unknown argument: {other}", "✗".red());
            return;
        }
    };
    let mut blockers = Vec::new();
    if state.is_loading {
        blockers.push("a turn is in progress");
    }
    if state.is_compacting {
        blockers.push("context compaction is running");
    }
    if !force && !blockers.is_empty() {
        println!(
            "{} cannot start a new session while {}. Run /new --force to discard pending work.",
            "✗".red(),
            blockers.join(", ")
        );
        return;
    }

    let new_id = uuid::Uuid::new_v4().to_string();
    state.api_messages.clear();
    state.system_prompt = None;
    state.session = Default::default();
    state.agent_progress.clear();
    state.subagent_cache.clear();
    // The on-screen transcript (and thus `/expand` replay) belongs to the
    // old session; the new session starts with an empty tool log.
    state.tool_log.clear();
    state.current_session_id = Some(new_id.clone());
    if let Ok(engine) = state.engine() {
        let _ = engine
            .send(Op::SyncSession {
                session_id: Some(new_id.clone()),
                messages: Vec::new(),
                system_prompt: None,
                system_prompt_override: false,
                model: state.model.clone(),
                workspace: state.workspace.clone(),
            })
            .await;
    }
    println!(
        "started new session {} — previous sessions remain available via /resume",
        truncate_id(&new_id)
    );
}

/// `/load <path>` — load a session file.
pub async fn load(state: &mut ReplState, path: Option<&str>) {
    let Some(p) = path else {
        println!("{} usage: /load <path>", "✗".red());
        return;
    };
    let load_path = if p.contains('/') || p.contains('\\') {
        std::path::PathBuf::from(p)
    } else {
        state.workspace.join(p)
    };
    let content = match std::fs::read_to_string(&load_path) {
        Ok(c) => c,
        Err(e) => {
            println!("{} failed to read session file: {e}", "✗".red());
            return;
        }
    };
    let session: crate::session_manager::SavedSession = match serde_json::from_str(&content) {
        Ok(s) => s,
        Err(e) => {
            println!("{} failed to parse session file: {e}", "✗".red());
            return;
        }
    };
    adopt_loaded_session(state, session).await;
    println!(
        "session loaded from {} ({} messages)",
        load_path.display(),
        state.api_messages.len(),
    );
}

/// `/resume <id|prefix|latest>` — resume a saved session in place.
pub async fn resume(state: &mut ReplState, arg: Option<&str>) {
    let requested = arg.map(str::trim).filter(|s| !s.is_empty());
    // Claude Code's `/resume [session]` takes an id *or a name*; this
    // resolves a name only when nothing matches as an id prefix, so an id
    // that happens to look like a title still wins.
    let id = match requested {
        None => "latest".to_string(),
        Some(value) => match SessionManager::default_location()
            .ok()
            .and_then(|manager| manager.load_session_by_prefix(value).ok())
        {
            Some(_) => value.to_string(),
            None => match find_session_by_name(value) {
                Some(id) => {
                    println!("{}", format!("resuming {id} (matched by name)").dimmed());
                    id
                }
                None => value.to_string(),
            },
        },
    };
    let _ = super::super::state::resume_session(state, &id).await;
}

/// The newest session whose title contains `needle`, if any.
fn find_session_by_name(needle: &str) -> Option<String> {
    let manager = SessionManager::default_location().ok()?;
    let sessions = manager.list_sessions().ok()?;
    let needle = needle.to_ascii_lowercase();
    sessions
        .into_iter()
        .filter(|meta| !meta.title.is_empty() && meta.title.to_ascii_lowercase().contains(&needle))
        .max_by_key(|meta| meta.updated_at)
        .map(|meta| meta.id)
}

/// A title for this session, from its first user message.
///
/// Local and synchronous by design (see [`rename`]): the first line of the
/// first user message, trimmed to the title cap at a word boundary.
fn derive_title(state: &ReplState) -> Option<String> {
    let first = state
        .api_messages
        .iter()
        .find(|message| message.role == "user")
        .and_then(|message| {
            message.content.iter().find_map(|block| match block {
                crate::models::ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
        })?;
    let line = first.lines().next().unwrap_or(first).trim();
    if line.is_empty() {
        return None;
    }
    if line.chars().count() <= MAX_TITLE_LEN {
        return Some(line.to_string());
    }
    let mut title: String = line.chars().take(MAX_TITLE_LEN).collect();
    if let Some(cut) = title.rfind(char::is_whitespace) {
        title.truncate(cut);
    }
    Some(title)
}

/// Shared adoption path for `/load`.
async fn adopt_loaded_session(
    state: &mut ReplState,
    session: crate::session_manager::SavedSession,
) {
    state.api_messages = session.messages.clone();
    state.set_model_selection(session.metadata.model.clone());
    state.workspace = session.metadata.workspace.clone();
    state.session.total_tokens = u32::try_from(session.metadata.total_tokens).unwrap_or(u32::MAX);
    state.session.total_conversation_tokens = state.session.total_tokens;
    // Accumulated token breakdown is per-runtime-session; zero on load.
    state.session.total_input_tokens = 0;
    state.session.total_cache_hit_tokens = 0;
    state.session.total_cache_miss_tokens = 0;
    state.session.total_output_tokens = 0;
    state.session.session_cost = 0.0;
    state.session.session_cost_cny = 0.0;
    state.session.subagent_cost = 0.0;
    state.session.subagent_cost_cny = 0.0;
    state.session.displayed_cost_high_water = 0.0;
    state.session.displayed_cost_high_water_cny = 0.0;
    state.session.last_prompt_tokens = None;
    state.session.last_completion_tokens = None;
    state.session.last_prompt_cache_hit_tokens = None;
    state.session.last_prompt_cache_miss_tokens = None;
    state.session.last_reasoning_replay_tokens = None;
    state.current_session_id = Some(session.metadata.id.clone());
    state.system_prompt = session.system_prompt.map(SystemPrompt::Text);
    // Round-3 TODO-42 — adopted sessions keep their failed-attempt records,
    // so a `/load` + save cycle does not silently drop them.
    state.failed_attempts = session.failed_attempts.clone();
    if let Ok(engine) = state.engine() {
        let _ = engine
            .send(Op::SyncSession {
                session_id: Some(session.metadata.id.clone()),
                messages: session.messages,
                system_prompt: state.system_prompt.clone(),
                system_prompt_override: false,
                model: state.model.clone(),
                workspace: state.workspace.clone(),
            })
            .await;
    }
}

/// `/compact [memory|from=N|up_to=N]`
pub async fn compact(state: &mut ReplState, arg: Option<&str>) {
    use crate::core::ops::CompactMode;

    let mode = match arg.map(str::trim) {
        None => CompactMode::Full,
        Some("memory") | Some("kod") => CompactMode::Memory,
        Some(s) if s.starts_with("from=") => {
            let idx: usize = s.strip_prefix("from=").unwrap_or(s).parse().unwrap_or(0);
            if idx == 0 {
                println!("{} invalid from index — usage: /compact from=N", "✗".red());
                return;
            }
            CompactMode::From { pivot_index: idx }
        }
        Some(s) if s.starts_with("up_to=") => {
            let idx: usize = s.strip_prefix("up_to=").unwrap_or(s).parse().unwrap_or(0);
            if idx == 0 {
                println!(
                    "{} invalid up_to index — usage: /compact up_to=N",
                    "✗".red()
                );
                return;
            }
            CompactMode::UpTo { pivot_index: idx }
        }
        Some(other) => {
            println!(
                "{} unknown compact mode '{other}' — usage: /compact [memory|from=N|up_to=N]",
                "✗".red()
            );
            return;
        }
    };
    let label = match &mode {
        CompactMode::Full => "full".to_string(),
        CompactMode::From { pivot_index } => format!("from={pivot_index}"),
        CompactMode::UpTo { pivot_index } => format!("up_to={pivot_index}"),
        CompactMode::Memory => "memory".to_string(),
    };
    if let Ok(engine) = state.engine() {
        let op = match mode {
            CompactMode::Full => Op::CompactContext,
            other => Op::CompactContextWithMode { mode: other },
        };
        let _ = engine.send(op).await;
        println!("context compaction ({label}) triggered…");
    }
}

/// `/purge`
pub async fn purge(state: &mut ReplState) {
    if let Ok(engine) = state.engine() {
        let _ = engine.send(Op::PurgeContext).await;
        println!("agent context purge triggered…");
    }
}

/// `/export [path]` — markdown export of the conversation.
pub fn export(state: &ReplState, path: Option<&str>) {
    use std::fmt::Write as _;

    let export_path = path.map_or_else(
        || {
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            std::path::PathBuf::from(format!("chat_export_{timestamp}.md"))
        },
        std::path::PathBuf::from,
    );
    let mut content = String::new();
    content.push_str("# Chat Export\n\n");
    let _ = write!(
        content,
        "**Model:** {}\n**Workspace:** {}\n**Date:** {}\n\n---\n\n",
        state.model,
        state.workspace.display(),
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    for message in &state.api_messages {
        let role = match message.role.as_str() {
            "user" => "**You:**",
            "assistant" => "**Assistant:**",
            "system" => "*System:*",
            other => other,
        };
        let body = render_message_text(message);
        let _ = write!(content, "{role}\n\n{}\n\n---\n\n", body.trim());
    }
    match std::fs::write(&export_path, content) {
        Ok(()) => println!("exported to {}", export_path.display()),
        Err(e) => println!("{} failed to export: {e}", "✗".red()),
    }
}

fn render_message_text(message: &Message) -> String {
    use crate::models::ContentBlock;

    let mut out = String::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text, .. } => out.push_str(text),
            ContentBlock::Image { source } => out.push_str(&source.summary()),
            ContentBlock::Thinking { thinking } => {
                out.push_str(&format!("[thinking] {thinking}"));
            }
            ContentBlock::ToolUse { name, .. } => {
                out.push_str(&format!("[tool call: {name}]"));
            }
            other => out.push_str(&format!("[{other:?}]")),
        }
    }
    out
}

/// `/sessions [prune <days>]` — list sessions (line-based) or prune.
pub fn sessions(state: &ReplState, arg: Option<&str>) {
    let trimmed = arg.unwrap_or("").trim();
    if trimmed.is_empty() || matches!(trimmed, "show" | "list") {
        let Ok(manager) = SessionManager::default_location() else {
            println!("{} could not open the session store", "✗".red());
            return;
        };
        let Ok(mut sessions) = manager.list_sessions() else {
            println!("{} could not list sessions", "✗".red());
            return;
        };
        sessions.truncate(20);
        if sessions.is_empty() {
            println!("no saved sessions");
            return;
        }
        let current = state.current_session_id.as_deref();
        println!("{}", "saved sessions (newest first):".bold());
        for meta in sessions {
            let marker = if Some(meta.id.as_str()) == current {
                "← current"
            } else {
                ""
            };
            println!(
                "  {}  {}  {}  {}",
                truncate_id(&meta.id),
                meta.updated_at.format("%Y-%m-%d %H:%M"),
                if meta.title.is_empty() {
                    "(untitled)"
                } else {
                    meta.title.as_str()
                },
                marker,
            );
        }
        println!("{}", "resume with /resume <id-prefix>".dimmed());
        return;
    }
    let mut parts = trimmed.split_whitespace();
    let action = parts.next().unwrap_or_default().to_ascii_lowercase();
    match action.as_str() {
        "prune" => prune(parts.next()),
        other => println!(
            "{} unknown subcommand `{other}` — usage: /sessions [prune <days>]",
            "✗".red()
        ),
    }
}

fn prune(days_arg: Option<&str>) {
    let Some(days_str) = days_arg else {
        println!(
            "{} usage: /sessions prune <days> (e.g. /sessions prune 30)",
            "✗".red()
        );
        return;
    };
    let Ok(days) = days_str.parse::<u64>() else {
        println!("{} expected a positive integer number of days", "✗".red());
        return;
    };
    if days == 0 {
        println!("{} expected a positive integer number of days", "✗".red());
        return;
    }
    let Ok(manager) = SessionManager::default_location() else {
        println!("{} could not open sessions directory", "✗".red());
        return;
    };
    let max_age = std::time::Duration::from_secs(days.saturating_mul(24 * 60 * 60));
    match manager.prune_sessions_older_than(max_age) {
        Ok(0) => println!("no sessions older than {days}d to prune"),
        Ok(n) => println!("pruned {n} session(s) older than {days}d"),
        Err(err) => println!("{} prune failed: {err}", "✗".red()),
    }
}

/// `/rename <title>`
pub fn rename(state: &mut ReplState, arg: Option<&str>) {
    // Claude Code auto-generates the name when none is given. This derives
    // one from the conversation's first user message instead of asking the
    // model: a rename is a UI affordance, and a network call for it would
    // be a surprise cost.
    let derived = arg
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| derive_title(state));
    let Some(new_title) = derived.as_deref() else {
        println!(
            "{}",
            "usage: /rename <title> — or /rename with no argument after a message to name the session from it"
                .red()
        );
        return;
    };
    if new_title.chars().count() > MAX_TITLE_LEN {
        println!(
            "{} title too long (max {MAX_TITLE_LEN} characters)",
            "✗".red()
        );
        return;
    }
    let Some(session_id) = state.current_session_id.clone() else {
        println!("{} no active session — send a message first", "✗".red());
        return;
    };
    let Ok(manager) = SessionManager::default_location() else {
        println!("{} could not open sessions directory", "✗".red());
        return;
    };
    let mut session = match manager.load_session(&session_id) {
        Ok(s) => s,
        Err(e) => {
            println!("{} could not load session: {e}", "✗".red());
            return;
        }
    };
    session = update_session(
        session,
        &state.api_messages,
        u64::from(state.session.total_tokens),
        state.system_prompt.as_ref(),
    );
    sync_cost_to_metadata(state, &mut session.metadata);
    session.metadata.title = new_title.to_string();
    match manager.save_session(&session) {
        Ok(_) => println!("session renamed to \"{new_title}\""),
        Err(e) => println!("{} could not save session: {e}", "✗".red()),
    }
}

#[cfg(test)]
mod tests {
    // The env lock spans the async body by the same convention as the
    // engine tests (core/engine/tests.rs header): the guard is deliberately
    // held across await points on the current-thread runtime.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    fn test_repl_state() -> ReplState {
        let options = crate::app_state::InteractiveOptions {
            model: "test-model".to_string(),
            workspace: std::env::temp_dir(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            max_subagents: 2,
            skills_dir: std::env::temp_dir(),
            memory_path: std::env::temp_dir().join("memory.md"),
            mcp_config_path: std::env::temp_dir().join("mcp.json"),
            use_memory: false,
            start_in_agent_mode: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        ReplState::new(options, &crate::config::Config::default())
    }

    fn saved_with_failed_attempt() -> crate::session_manager::SavedSession {
        let mut session = crate::session_manager::SavedSession {
            schema_version: 1,
            metadata: codesmith_agent_runtime::session_store::SessionMetadata {
                id: "sess-load".to_string(),
                title: "load adoption".to_string(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                message_count: 0,
                total_tokens: 0,
                model: "test-model".to_string(),
                reported_model: None,
                workspace: std::env::temp_dir(),
                mode: None,
                cost: Default::default(),
                parent_session_id: None,
                forked_from_message_count: None,
                cumulative_turn_secs: 0,
            },
            messages: Vec::new(),
            system_prompt: None,
            last_tool_catalog: None,
            last_base_url: None,
            fact_ledger: None,
            context_references: Vec::new(),
            artifacts: Vec::new(),
            failed_attempts: Vec::new(),
        };
        session.failed_attempts = vec![codesmith_agent_runtime::session_store::AttemptRecord {
            turn_id: "t-9".to_string(),
            step: 3,
            error_category: "network".to_string(),
            error_message: "connection reset by peer".to_string(),
            transparent_retries: 3,
            partial_content: false,
            duration_ms: 880,
        }];
        session
    }

    /// Round-3 TODO-42 — `/load` adopts the saved session's failed-attempt
    /// records, so a load + save cycle keeps them (the same one-line
    /// adoption `resume_session` performs at startup).
    #[tokio::test]
    async fn load_adopts_failed_attempt_records() {
        let _lock = crate::test_support::lock_test_env();
        let mut state = test_repl_state();
        adopt_loaded_session(&mut state, saved_with_failed_attempt()).await;
        assert_eq!(
            state.failed_attempts.len(),
            1,
            "loading a session must adopt its failed-attempt records"
        );
        assert_eq!(state.failed_attempts[0].turn_id, "t-9");
    }
}
