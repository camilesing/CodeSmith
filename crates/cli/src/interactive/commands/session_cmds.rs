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
    let mut parent = create_saved_session_with_id_and_mode(
        parent_id,
        &state.api_messages,
        &state.model,
        &state.workspace,
        u64::from(state.session.total_tokens),
        state.system_prompt.as_ref(),
        Some(state.mode.label()),
    );
    sync_cost_to_metadata(state, &mut parent.metadata);
    if let Err(err) = manager.save_session(&parent) {
        println!("{} failed to save parent session: {err}", "✗".red());
        return;
    }
    let mut forked = build_saved_session(state);
    forked.metadata.copy_cost_from(&parent.metadata);
    forked.metadata.mark_forked_from(&parent.metadata);
    if let Err(err) = manager.save_session(&forked) {
        println!("{} failed to save forked session: {err}", "✗".red());
        return;
    }
    state.current_session_id = Some(forked.metadata.id.clone());
    let fork_id = forked.metadata.id.clone();
    println!(
        "forked session {} -> {}",
        truncate_id(&parent.metadata.id),
        truncate_id(&fork_id)
    );
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
    let id = match arg.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => id.to_string(),
        None => "latest".to_string(),
    };
    let _ = super::super::state::resume_session(state, &id).await;
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
    let Some(new_title) = arg.map(str::trim).filter(|s| !s.is_empty()) else {
        println!("{} usage: /rename <new title>", "✗".red());
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
