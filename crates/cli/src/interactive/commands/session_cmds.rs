//! Session lifecycle commands for the REPL — port of `commands/session.rs`
//! and `commands/rename.rs` against [`ReplState`]. Where the TUI returned
//! `AppAction`s for its event loop to apply, the REPL applies the engine
//! op directly.

use colored::Colorize;
use std::sync::Arc;

use super::ReplState;
use crate::models::{Message, SystemPrompt};
use codesmith_agent_runtime::ops::Op;
use codesmith_session::store::{
    FileSessionStore, create_saved_session_with_id_and_mode, create_saved_session_with_mode,
    truncate_id, update_session,
};

const MAX_TITLE_LEN: usize = 100;

/// Send a session op to the engine, warning when the op is rejected: a
/// dropped sync leaves the engine serving the previous session (id,
/// messages, model, workspace) while the REPL has already switched.
///
/// No engine attached is not a failure (engine-less REPL runs are a real
/// mode) and stays silent — only a failed send diverges.
async fn send_session_op(state: &ReplState, op: Op) {
    if let Ok(engine) = state.engine()
        && let Err(err) = engine.send(op).await
    {
        println!(
            "{}",
            format!("warning: the engine did not accept the session sync: {err}").yellow()
        );
    }
}

fn sync_cost_to_metadata(
    state: &ReplState,
    metadata: &mut codesmith_session::store::SessionMetadata,
) {
    metadata.cost.session_cost_usd = state.session.session_cost;
    metadata.cost.session_cost_cny = state.session.session_cost_cny;
    metadata.cost.subagent_cost_usd = state.session.subagent_cost;
    metadata.cost.subagent_cost_cny = state.session.subagent_cost_cny;
    metadata.cost.displayed_cost_high_water_usd = state.session.displayed_cost_high_water;
    metadata.cost.displayed_cost_high_water_cny = state.session.displayed_cost_high_water_cny;
    metadata.cumulative_turn_secs = state.cumulative_turn_duration.as_secs();
}

fn build_saved_session(state: &ReplState) -> codesmith_session::store::SavedSession {
    let mut session = create_saved_session_with_mode(
        &state.api_messages,
        &state.model,
        &state.workspace,
        state.session.total_tokens,
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
            let dir = codesmith_session::store::default_sessions_dir()
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
    match codesmith_agent_runtime::utils::write_atomic(&save_path, json.as_bytes()) {
        Ok(()) => {
            // Only a save into the managed store produces a resolvable id:
            // `load_session` resolves ids inside the sessions dir, so
            // adopting the fresh UUID for a caller-supplied path left
            // `/rename`, `/sessions`, and prefix resume pointing at nothing.
            if path.is_none() {
                state.current_session_id = Some(session.metadata.id.clone());
            }
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
    let Ok(manager) = FileSessionStore::default_location() else {
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
                state.session.total_tokens,
                state.system_prompt.as_ref(),
                Some(state.mode.label()),
            )
        },
        |existing| {
            update_session(
                existing,
                &state.api_messages,
                state.session.total_tokens,
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
    send_session_op(
        state,
        Op::SyncSession {
            session_id: Some(fork_id),
            messages: (*state.api_messages).clone(),
            system_prompt: state.system_prompt.clone(),
            system_prompt_override: super::system_prompt_override_active(state),
            model: state.model.clone(),
            workspace: state.workspace.clone(),
        },
    )
    .await;
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
    state.api_messages = Arc::new(Vec::new());
    state.system_prompt = None;
    state.session = Default::default();
    state.agent_progress.clear();
    state.subagent_cache.clear();
    // The on-screen transcript (and thus `/expand` replay) belongs to the
    // old session; the new session starts with an empty tool log.
    state.tool_log.clear();
    // Per-session bookkeeping that the next save would otherwise re-persist
    // into the fresh session's record: failed-attempt history and the
    // accumulated turn duration belong to the session that earned them.
    state.failed_attempts.clear();
    state.cumulative_turn_duration = Default::default();
    state.current_session_id = Some(new_id.clone());
    send_session_op(
        state,
        Op::SyncSession {
            session_id: Some(new_id.clone()),
            messages: Vec::new(),
            system_prompt: None,
            system_prompt_override: super::system_prompt_override_active(state),
            model: state.model.clone(),
            workspace: state.workspace.clone(),
        },
    )
    .await;
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
    let session: codesmith_session::store::SavedSession = match serde_json::from_str(&content) {
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
        Some(value) => match FileSessionStore::default_location()
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
    let manager = FileSessionStore::default_location().ok()?;
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
    session: codesmith_session::store::SavedSession,
) {
    state.api_messages = Arc::new(session.messages.clone());
    state.set_model_selection(session.metadata.model.clone());
    // A session file is user-named input (shared, shipped in a repo,
    // hand-edited): its stored workspace must exist AND pass the gate `/cd`
    // applies — the current workspace's own subtree, or a trusted directory.
    // Existence alone let any file re-root the session, with this
    // workspace's trust, shell and approval settings, into `$HOME` or `/`.
    let stored = &session.metadata.workspace;
    if !stored.is_dir() {
        println!(
            "{} session workspace {} does not exist — keeping the current workspace ({})",
            "✗".red(),
            stored.display(),
            state.workspace.display()
        );
    } else if !super::dir_cmds::cd_allowed(&state.workspace, stored) {
        println!(
            "{} session workspace {} is not trusted for this session — keeping the current \
             workspace ({}); run `/trust add {}` to move there",
            "✗".red(),
            stored.display(),
            state.workspace.display(),
            stored.display()
        );
    } else {
        state.workspace = stored.clone();
    }
    state.session.total_tokens = session.metadata.total_tokens;
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
    // The same per-session runtime state `/new` resets: after `/load`, the
    // previous session's on-screen transcript, agent progress, subagent
    // cache, and accumulated turn duration would otherwise ride along with
    // the loaded session's messages (and be re-persisted by the next save).
    state.tool_log.clear();
    state.agent_progress.clear();
    state.subagent_cache.clear();
    state.cumulative_turn_duration = Default::default();
    // Round-3 TODO-42 — adopted sessions keep their failed-attempt records,
    // so a `/load` + save cycle does not silently drop them.
    state.failed_attempts = session.failed_attempts.clone();
    send_session_op(
        state,
        Op::SyncSession {
            session_id: Some(session.metadata.id.clone()),
            messages: session.messages,
            system_prompt: state.system_prompt.clone(),
            system_prompt_override: super::system_prompt_override_active(state),
            model: state.model.clone(),
            workspace: state.workspace.clone(),
        },
    )
    .await;
    // The loaded session may carry a different model; its compaction trigger
    // is model-derived, so push the adopted threshold/window to the engine.
    super::config_cmds::apply_compaction(state).await;
}

/// `/compact [memory|from=N|up_to=N]`
pub async fn compact(state: &mut ReplState, arg: Option<&str>) {
    use codesmith_agent_runtime::ops::CompactMode;

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
        if let Err(err) = engine.send(op).await {
            println!(
                "{}",
                format!("warning: the engine did not accept the compaction: {err}").yellow()
            );
        } else {
            println!("context compaction ({label}) triggered…");
        }
    }
}

/// `/purge`
pub async fn purge(state: &mut ReplState) {
    if let Ok(engine) = state.engine() {
        if let Err(err) = engine.send(Op::PurgeContext).await {
            println!(
                "{}",
                format!("warning: the engine did not accept the purge: {err}").yellow()
            );
        } else {
            println!("agent context purge triggered…");
        }
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
    for message in state.api_messages.iter() {
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
        let Ok(manager) = FileSessionStore::default_location() else {
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
        // Forward the WHOLE remainder, not just the next word: forwarding
        // only the days word dropped `--yes`/`-y`, so a confirmed prune
        // silently stayed a dry run and never deleted anything.
        // Forward the WHOLE remainder, not just the next word: forwarding
        // only the days word dropped `--yes`/`-y`, so a confirmed prune
        // silently stayed a dry run and never deleted anything.
        "prune" => {
            let rest: Vec<&str> = parts.collect();
            let rest = rest.join(" ");
            prune(if rest.is_empty() { None } else { Some(&rest) });
        }
        other => println!(
            "{} unknown subcommand `{other}` — usage: /sessions [prune <days>]",
            "✗".red()
        ),
    }
}

fn prune(days_arg: Option<&str>) {
    let mut words = days_arg
        .map(str::split_whitespace)
        .map_or_else(Vec::new, Iterator::collect::<Vec<&str>>);
    let confirmed = words.iter().any(|w| *w == "--yes" || *w == "-y");
    words.retain(|w| !matches!(*w, "--yes" | "-y"));
    let Some(days_str) = words.first().copied() else {
        println!(
            "{} usage: /sessions prune <days> [--yes] (e.g. /sessions prune 30 --yes)",
            "✗".red()
        );
        return;
    };
    if words.len() > 1 {
        println!(
            "{} usage: /sessions prune <days> [--yes] (e.g. /sessions prune 30 --yes)",
            "✗".red()
        );
        return;
    }
    let Ok(days) = days_str.parse::<u64>() else {
        println!("{} expected a positive integer number of days", "✗".red());
        return;
    };
    if days == 0 {
        println!("{} expected a positive integer number of days", "✗".red());
        return;
    }
    let Ok(manager) = FileSessionStore::default_location() else {
        println!("{} could not open sessions directory", "✗".red());
        return;
    };
    let max_age = std::time::Duration::from_secs(days.saturating_mul(24 * 60 * 60));
    // Pruning is a permanent delete driven by age alone, so it previews
    // first: without `--yes` the command counts what the age window matches
    // (a typo'd large `days` would otherwise wipe the whole store) and the
    // user re-runs it to delete.
    if !confirmed {
        let candidates = manager
            .list_sessions()
            .map(|sessions| {
                sessions
                    .into_iter()
                    .filter(|meta| {
                        meta.updated_at
                            < chrono::Utc::now()
                                - chrono::Duration::from_std(max_age)
                                    .unwrap_or(chrono::Duration::days(365 * 10))
                    })
                    .count()
            })
            .unwrap_or(0);
        println!(
            "{}",
            format!(
                "dry run: {candidates} session(s) older than {days}d would be pruned — \
                 re-run `/sessions prune {days} --yes` to delete them"
            )
            .dimmed()
        );
        return;
    }
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
    let Ok(manager) = FileSessionStore::default_location() else {
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
        state.session.total_tokens,
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
    use codesmith_agent_runtime::test_support::EnvVarGuard;

    fn test_repl_state() -> ReplState {
        let options = crate::app_state::InteractiveOptions {
            load_project_presets: false,
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

    /// Redirect HOME (session-workspace and import-source resolution),
    /// CODESMITH_HOME (managed session store, `~/.codesmith/sessions`), and
    /// CODESMITH_CONFIG_PATH (settings) into a scratch dir. Every test that
    /// can reach a persist path must hold the env lock for the whole test.
    struct ScratchEnv {
        _home: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
        _home_guard: EnvVarGuard,
        _smith_home_guard: EnvVarGuard,
        _config_guard: EnvVarGuard,
    }

    fn scratch_env() -> ScratchEnv {
        let lock = codesmith_agent_runtime::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("tempdir");
        let codesmith_home = home.path().join(".codesmith");
        std::fs::create_dir_all(&codesmith_home).expect("create codesmith home");
        let home_guard = EnvVarGuard::set("HOME", home.path());
        let smith_home_guard = EnvVarGuard::set("CODESMITH_HOME", &codesmith_home);
        let config_guard =
            EnvVarGuard::set("CODESMITH_CONFIG_PATH", codesmith_home.join("config.toml"));
        ScratchEnv {
            _home: home,
            _lock: lock,
            _home_guard: home_guard,
            _smith_home_guard: smith_home_guard,
            _config_guard: config_guard,
        }
    }

    fn user_message(text: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![crate::models::ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn saved_with_failed_attempt() -> codesmith_session::store::SavedSession {
        let mut session = codesmith_session::store::SavedSession {
            schema_version: 1,
            metadata: codesmith_session::store::SessionMetadata {
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
            approvals: Vec::new(),
            journal_bytes: None,
        };
        session.failed_attempts = vec![codesmith_session::store::AttemptRecord {
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
        let _lock = codesmith_agent_runtime::test_support::lock_test_env();
        let mut state = test_repl_state();
        adopt_loaded_session(&mut state, saved_with_failed_attempt()).await;
        assert_eq!(
            state.failed_attempts.len(),
            1,
            "loading a session must adopt its failed-attempt records"
        );
        assert_eq!(state.failed_attempts[0].turn_id, "t-9");
    }

    /// `/save` with no path writes into the managed store and adopts the
    /// fresh id; the persisted record carries the live cost/token counters
    /// (`sync_cost_to_metadata`) and the failed-attempt history.
    #[test]
    fn save_without_a_path_writes_the_managed_store_and_adopts_the_id() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![user_message("hello save")]);
        state.session.session_cost = 1.25;
        state.session.subagent_cost_cny = 0.5;
        state.cumulative_turn_duration = std::time::Duration::from_secs(9);
        state.failed_attempts = saved_with_failed_attempt().failed_attempts.clone();

        save(&mut state, None);

        let adopted = state
            .current_session_id
            .clone()
            .expect("a managed save adopts the fresh session id");
        let manager = FileSessionStore::default_location().expect("session store");
        let files: Vec<_> = std::fs::read_dir(manager.sessions_dir())
            .expect("sessions dir")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        assert_eq!(files.len(), 1, "exactly one managed session file");
        let saved: codesmith_session::store::SavedSession =
            serde_json::from_str(&std::fs::read_to_string(&files[0]).expect("read session file"))
                .expect("parse session file");
        assert_eq!(saved.metadata.id, adopted);
        assert_eq!(saved.metadata.message_count, 1);
        assert_eq!(saved.metadata.model, "test-model");
        assert_eq!(saved.metadata.cost.session_cost_usd, 1.25);
        assert_eq!(saved.metadata.cost.subagent_cost_cny, 0.5);
        assert_eq!(saved.metadata.cumulative_turn_secs, 9);
        assert_eq!(saved.failed_attempts.len(), 1, "failed attempts persist");
    }

    /// A caller-supplied path is honored (parents created) and deliberately
    /// does NOT adopt the generated id — `load_session` resolves ids inside
    /// the sessions dir, so adopting it would point at nothing.
    #[test]
    fn save_with_an_explicit_path_writes_it_without_adopting_the_id() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![user_message("explicit")]);
        let out = tempfile::tempdir().expect("tempdir");
        let save_path = out.path().join("nested").join("session.json");

        save(&mut state, Some(save_path.to_str().unwrap()));

        assert!(
            save_path.is_file(),
            "the explicit save path is written, parents included"
        );
        assert_eq!(
            state.current_session_id, None,
            "an explicit path never adopts a managed-store id"
        );
        let manager = FileSessionStore::default_location().expect("session store");
        assert!(
            manager.list_sessions().expect("list").is_empty(),
            "the managed store is untouched by an explicit-path save"
        );
    }

    /// The write-failure branch reports and returns without adopting an id.
    #[test]
    fn save_onto_a_directory_reports_failure() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![user_message("doomed")]);
        let out = tempfile::tempdir().expect("tempdir");

        save(&mut state, Some(out.path().to_str().unwrap()));

        assert_eq!(
            state.current_session_id, None,
            "a failed save must not adopt an id"
        );
    }

    /// `/fork` persists the branch point and the copy; the copy records its
    /// lineage (`parent_session_id` + message count) and becomes the live
    /// session.
    #[tokio::test]
    async fn fork_saves_the_parent_and_a_lineage_stamped_copy() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![user_message("hello world")]);

        fork(&mut state).await;

        let fork_id = state
            .current_session_id
            .clone()
            .expect("fork switches to the copy");
        let manager = FileSessionStore::default_location().expect("session store");
        let mut listed = manager.list_sessions().expect("list sessions");
        listed.sort_by_key(|meta| meta.id.clone());
        assert_eq!(
            listed.len(),
            2,
            "the branch point and the copy both persist"
        );

        let forked = manager.load_session(&fork_id).expect("load fork");
        let parent_id = listed
            .iter()
            .map(|meta| meta.id.as_str())
            .find(|id| *id != fork_id)
            .expect("the other row is the parent")
            .to_string();
        assert_eq!(
            forked.metadata.parent_session_id.as_deref(),
            Some(parent_id.as_str())
        );
        assert_eq!(forked.metadata.forked_from_message_count, Some(1));
        let parent = manager.load_session(&parent_id).expect("load parent");
        assert_eq!(parent.metadata.message_count, 1);
    }

    /// `/branch <name>` titles the copy before saving so `/resume <name>`
    /// finds the branch rather than the branch point.
    #[tokio::test]
    async fn branch_with_a_name_titles_the_copy() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![user_message("to branch from")]);

        branch(&mut state, Some("side-quest")).await;

        let manager = FileSessionStore::default_location().expect("session store");
        let forked = manager
            .load_session(
                state
                    .current_session_id
                    .as_deref()
                    .expect("branch switches to the copy"),
            )
            .expect("load fork");
        assert_eq!(forked.metadata.title, "side-quest");
    }

    /// The over-long-name guard and the empty-conversation guard both leave
    /// the store untouched.
    #[tokio::test]
    async fn branch_refuses_overlong_names_and_empty_conversations() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![user_message("content")]);

        let long_name: String = "x".repeat(MAX_TITLE_LEN + 1);
        branch(&mut state, Some(&long_name)).await;
        assert_eq!(state.current_session_id, None, "no branch was made");
        let manager = FileSessionStore::default_location().expect("session store");
        assert!(
            manager.list_sessions().expect("list").is_empty(),
            "an over-long name saves nothing"
        );

        state.api_messages = Arc::new(Vec::new());
        branch(&mut state, None).await;
        assert!(
            manager.list_sessions().expect("list").is_empty(),
            "nothing to fork saves nothing"
        );
    }

    /// Branching twice in a row keeps the first branch's title on disk —
    /// rebuilding the parent metadata from state instead used to silently
    /// unname it.
    #[tokio::test]
    async fn branching_twice_keeps_the_first_branch_title() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![user_message("hello world")]);

        branch(&mut state, None).await;
        let first_fork = state.current_session_id.clone().expect("first fork");
        branch(&mut state, Some("second-name")).await;
        let second_fork = state.current_session_id.clone().expect("second fork");
        assert_ne!(first_fork, second_fork);

        let manager = FileSessionStore::default_location().expect("session store");
        assert_eq!(
            manager
                .load_session(&first_fork)
                .expect("first fork")
                .metadata
                .title,
            "hello world",
            "the first branch keeps its derived title"
        );
        assert_eq!(
            manager
                .load_session(&second_fork)
                .expect("second fork")
                .metadata
                .title,
            "second-name"
        );
    }

    /// `/new --force` discards in-flight work and resets every per-session
    /// counter the next save would otherwise re-persist.
    #[tokio::test]
    async fn new_session_with_force_resets_per_session_state() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![user_message("stale turn")]);
        state.session.session_cost = 2.5;
        state.failed_attempts = saved_with_failed_attempt().failed_attempts.clone();
        state.cumulative_turn_duration = std::time::Duration::from_secs(11);
        state.current_session_id = Some("old-id".to_string());
        state
            .agent_progress
            .insert("a1".to_string(), "working".to_string());
        state.tool_log.push(crate::tool_summary::ToolCallRecord {
            seq: 1,
            turn: 1,
            name: "bash".to_string(),
            args_summary: None,
            output: None,
            error: None,
            replayed: false,
        });
        state.is_loading = true; // force must push past the blocker

        new_session(&mut state, Some("--force")).await;

        assert!(state.api_messages.is_empty());
        assert_eq!(state.session.session_cost, 0.0);
        assert!(state.failed_attempts.is_empty());
        assert_eq!(state.cumulative_turn_duration, std::time::Duration::ZERO);
        assert!(state.agent_progress.is_empty());
        assert!(state.tool_log.is_empty());
        assert_ne!(
            state.current_session_id.as_deref(),
            Some("old-id"),
            "a fresh id replaces the old session"
        );
    }

    /// Without `--force` in-flight work blocks `/new`, and an unknown
    /// argument is refused — both leave the session untouched.
    #[tokio::test]
    async fn new_session_blocks_without_force_and_rejects_unknown_args() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![user_message("in flight")]);
        state.current_session_id = Some("keep-me".to_string());
        state.is_loading = true;
        state.is_compacting = true;

        new_session(&mut state, None).await;
        assert_eq!(
            state.current_session_id.as_deref(),
            Some("keep-me"),
            "the blocker refused the reset"
        );
        assert_eq!(state.api_messages.len(), 1);

        new_session(&mut state, Some("junk")).await;
        assert_eq!(
            state.current_session_id.as_deref(),
            Some("keep-me"),
            "an unknown argument changes nothing"
        );
    }

    /// `/load <relative-name>` resolves against the workspace and adopts
    /// the record: messages, model, tokens, system prompt, and (when it
    /// exists and passes the `/cd` trust gate — here a directory inside the
    /// current workspace) the stored workspace.
    #[tokio::test]
    async fn load_adopts_a_session_file_from_the_workspace() {
        let _env = scratch_env();
        let ws = tempfile::tempdir().expect("tempdir");
        let saved_ws = ws.path().join("subdir");
        std::fs::create_dir_all(&saved_ws).expect("mkdir saved workspace");
        let mut session = saved_with_failed_attempt();
        session.messages = vec![user_message("loaded message")];
        session.metadata.message_count = 1;
        session.metadata.total_tokens = 42;
        session.metadata.model = "loaded-model".to_string();
        session.metadata.workspace = saved_ws.clone();
        session.system_prompt = Some("custom prompt".to_string());
        std::fs::write(
            ws.path().join("sess.json"),
            serde_json::to_string_pretty(&session).expect("serialize"),
        )
        .expect("write session file");

        let mut state = test_repl_state();
        state.workspace = ws.path().to_path_buf();
        state.session.session_cost = 9.9;
        load(&mut state, Some("sess.json")).await;

        assert_eq!(state.api_messages.len(), 1);
        assert_eq!(state.current_session_id.as_deref(), Some("sess-load"));
        assert_eq!(state.model, "loaded-model");
        assert_eq!(state.workspace, saved_ws);
        assert_eq!(state.session.total_tokens, 42);
        assert_eq!(state.session.total_conversation_tokens, 42);
        assert_eq!(state.session.session_cost, 0.0, "runtime cost resets");
        assert_eq!(
            state.system_prompt,
            Some(SystemPrompt::Text("custom prompt".to_string()))
        );
    }

    /// A stored workspace that does not exist is refused — a tampered file
    /// must not redirect relative-path resolution.
    #[tokio::test]
    async fn load_keeps_the_workspace_when_the_stored_one_is_missing() {
        let _env = scratch_env();
        let ws = tempfile::tempdir().expect("tempdir");
        let mut session = saved_with_failed_attempt();
        session.messages = vec![user_message("adopted anyway")];
        session.metadata.workspace = ws.path().join("definitely-not-here");
        let file = ws.path().join("elsewhere.json");
        std::fs::write(&file, serde_json::to_string_pretty(&session).unwrap()).unwrap();

        let mut state = test_repl_state();
        state.workspace = ws.path().to_path_buf();
        load(&mut state, Some(file.to_str().unwrap())).await;

        assert_eq!(state.workspace, ws.path(), "the bogus workspace is refused");
        assert_eq!(state.api_messages.len(), 1, "the messages still load");
    }

    /// A stored workspace that exists but is outside the current workspace
    /// and not trusted — a session file shared by someone else — must not
    /// re-root the session with this workspace's trust and shell settings.
    #[tokio::test]
    async fn load_keeps_the_workspace_when_the_stored_one_is_untrusted() {
        let _env = scratch_env();
        let ws = tempfile::tempdir().expect("tempdir");
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let mut session = saved_with_failed_attempt();
        session.messages = vec![user_message("adopted anyway")];
        session.metadata.workspace = elsewhere.path().to_path_buf();
        let file = ws.path().join("shared.json");
        std::fs::write(&file, serde_json::to_string_pretty(&session).unwrap()).unwrap();

        let mut state = test_repl_state();
        state.workspace = ws.path().to_path_buf();
        load(&mut state, Some(file.to_str().unwrap())).await;

        assert_eq!(
            state.workspace,
            ws.path(),
            "an untrusted workspace is refused"
        );
        assert_eq!(state.api_messages.len(), 1, "the messages still load");
    }

    /// Usage, missing-file, and parse-error branches leave state untouched.
    #[tokio::test]
    async fn load_rejects_missing_arguments_files_and_bad_json() {
        let _env = scratch_env();
        let ws = tempfile::tempdir().expect("tempdir");
        let bad = ws.path().join("bad.json");
        std::fs::write(&bad, "definitely not json").unwrap();

        let mut state = test_repl_state();
        state.workspace = ws.path().to_path_buf();
        load(&mut state, None).await;
        assert!(state.api_messages.is_empty(), "usage error loads nothing");
        load(&mut state, Some("missing.json")).await;
        assert!(state.api_messages.is_empty(), "missing file loads nothing");
        load(&mut state, Some("bad.json")).await;
        assert!(state.api_messages.is_empty(), "parse error loads nothing");
        assert_eq!(state.current_session_id, None);
    }

    /// `/resume <prefix>` resolves inside the managed store and adopts the
    /// session (messages, id, failed-attempt records) even engine-less —
    /// the engine sync is the only step that needs one.
    #[tokio::test]
    async fn resume_by_prefix_adopts_the_saved_session() {
        let _env = scratch_env();
        let ws = tempfile::tempdir().expect("tempdir");
        let mut session = saved_with_failed_attempt();
        session.metadata.id = "resumable-1".to_string();
        session.metadata.title = "prefix resume".to_string();
        session.metadata.message_count = 1;
        session.metadata.workspace = ws.path().to_path_buf();
        session.messages = vec![user_message("resumed conversation")];
        FileSessionStore::default_location()
            .expect("store")
            .save_session(&session)
            .expect("save");

        let mut state = test_repl_state();
        state.workspace = ws.path().to_path_buf();
        resume(&mut state, Some("resum")).await;

        assert_eq!(state.current_session_id.as_deref(), Some("resumable-1"));
        assert_eq!(state.api_messages.len(), 1);
        assert_eq!(
            state.failed_attempts.len(),
            1,
            "resume re-adopts failed-attempt records"
        );
    }

    /// A name that matches no id prefix resolves to the newest session
    /// whose title contains it.
    #[tokio::test]
    async fn resume_by_name_matches_a_titled_session() {
        let _env = scratch_env();
        let ws = tempfile::tempdir().expect("tempdir");
        let mut session = saved_with_failed_attempt();
        session.metadata.id = "byname-9".to_string();
        session.metadata.title = "alpha beta gamma".to_string();
        session.metadata.message_count = 1;
        session.metadata.workspace = ws.path().to_path_buf();
        session.messages = vec![user_message("named")];
        FileSessionStore::default_location()
            .expect("store")
            .save_session(&session)
            .expect("save");

        let mut state = test_repl_state();
        state.workspace = ws.path().to_path_buf();
        resume(&mut state, Some("beta")).await;

        assert_eq!(
            state.current_session_id.as_deref(),
            Some("byname-9"),
            "the title match wins when no id prefix matches"
        );
    }

    /// `/resume` with no argument resumes the newest non-empty session for
    /// this workspace.
    #[tokio::test]
    async fn resume_latest_picks_the_newest_workspace_session() {
        let _env = scratch_env();
        let ws = tempfile::tempdir().expect("tempdir");
        let manager = FileSessionStore::default_location().expect("store");
        for (id, age_secs) in [("older-1", 86_400_u64), ("newer-1", 0)] {
            let mut session = saved_with_failed_attempt();
            session.metadata.id = id.to_string();
            session.metadata.title = format!("{id} titled");
            session.metadata.message_count = 1;
            session.metadata.workspace = ws.path().to_path_buf();
            session.metadata.updated_at =
                chrono::Utc::now() - chrono::Duration::seconds(age_secs as i64);
            session.messages = vec![user_message(id)];
            manager.save_session(&session).expect("save");
        }

        let mut state = test_repl_state();
        state.workspace = ws.path().to_path_buf();
        resume(&mut state, None).await;

        assert_eq!(state.current_session_id.as_deref(), Some("newer-1"));
    }

    #[test]
    fn find_session_by_name_prefers_the_newest_match() {
        let _env = scratch_env();
        let manager = FileSessionStore::default_location().expect("store");
        for (id, title, age_secs) in [
            ("title-old-1", "needle here", 86_400_u64),
            ("title-new-1", "another needle entry", 0),
        ] {
            let mut session = saved_with_failed_attempt();
            session.metadata.id = id.to_string();
            session.metadata.title = title.to_string();
            session.metadata.updated_at =
                chrono::Utc::now() - chrono::Duration::seconds(age_secs as i64);
            manager.save_session(&session).expect("save");
        }
        assert_eq!(
            find_session_by_name("needle").as_deref(),
            Some("title-new-1"),
            "the newest title match wins"
        );
        assert_eq!(find_session_by_name("no-such-title"), None);
    }

    #[test]
    fn derive_title_uses_the_first_user_line() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![
            Message {
                role: "assistant".to_string(),
                content: vec![crate::models::ContentBlock::Text {
                    text: "not a user line".to_string(),
                    cache_control: None,
                }],
            },
            user_message("  first line here  \nsecond line"),
        ]);
        assert_eq!(derive_title(&state).as_deref(), Some("first line here"));
    }

    #[test]
    fn derive_title_truncates_long_lines_at_a_word_boundary() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        // 21 four-letter words: 104 chars — over the 100-char cap, and the
        // cut lands on the space after the 20th word ("tttt").
        let words = [
            "aaaa", "bbbb", "cccc", "dddd", "eeee", "ffff", "gggg", "hhhh", "iiii", "jjjj", "kkkk",
            "llll", "mmmm", "nnnn", "oooo", "pppp", "qqqq", "rrrr", "ssss", "tttt", "uuuu",
        ];
        state.api_messages = Arc::new(vec![user_message(&words.join(" "))]);
        let title = derive_title(&state).expect("a title is derived");
        assert!(
            title.chars().count() <= MAX_TITLE_LEN,
            "{title} exceeds the cap"
        );
        assert!(!title.ends_with(' '), "the cut is at a word boundary");
        assert!(title.contains("tttt"));
        assert!(!title.contains("uuuu"), "the overflow word is dropped");
    }

    #[test]
    fn derive_title_returns_none_without_usable_user_text() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        assert_eq!(derive_title(&state), None, "no messages at all");

        state.api_messages = Arc::new(vec![Message {
            role: "assistant".to_string(),
            content: vec![crate::models::ContentBlock::Text {
                text: "only assistant".to_string(),
                cache_control: None,
            }],
        }]);
        assert_eq!(derive_title(&state), None, "assistant-only conversation");

        state.api_messages = Arc::new(vec![user_message("   \nlater line")]);
        assert_eq!(
            derive_title(&state),
            None,
            "a blank first line derives none"
        );
    }

    /// `/rename` persists an explicit title (and the current messages) onto
    /// the stored record. The store is seeded the way the engine's
    /// turn-boundary autosave writes it (`<id>.json` via `save_session`).
    #[test]
    fn rename_persists_an_explicit_title() {
        let _env = scratch_env();
        let manager = FileSessionStore::default_location().expect("store");
        let mut stored = saved_with_failed_attempt();
        stored.metadata.id = "rename-1".to_string();
        stored.metadata.title = "original".to_string();
        manager.save_session(&stored).expect("seed");

        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![user_message("to be named")]);
        state.current_session_id = Some("rename-1".to_string());

        rename(&mut state, Some("renamed title"));

        let reloaded = manager.load_session("rename-1").expect("reload");
        assert_eq!(reloaded.metadata.title, "renamed title");
        assert_eq!(reloaded.metadata.message_count, 1);
    }

    /// `/rename` with no argument derives the title from the first user
    /// message instead of calling the model.
    #[test]
    fn rename_without_an_argument_derives_the_title() {
        let _env = scratch_env();
        let manager = FileSessionStore::default_location().expect("store");
        let mut stored = saved_with_failed_attempt();
        stored.metadata.id = "rename-2".to_string();
        stored.metadata.title = "original".to_string();
        manager.save_session(&stored).expect("seed");

        let mut state = test_repl_state();
        state.api_messages = Arc::new(vec![user_message("derive me please")]);
        state.current_session_id = Some("rename-2".to_string());

        rename(&mut state, None);

        assert_eq!(
            manager
                .load_session("rename-2")
                .expect("reload")
                .metadata
                .title,
            "derive me please"
        );
    }

    /// The usage error (no session, nothing derivable) and the length cap
    /// leave the stored title unchanged.
    #[test]
    fn rename_refuses_missing_sessions_long_titles_and_nothing_derivable() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        rename(&mut state, Some("orphan"));
        assert_eq!(
            state.current_session_id, None,
            "no active session means no rename happened"
        );

        let manager = FileSessionStore::default_location().expect("store");
        let mut stored = saved_with_failed_attempt();
        stored.metadata.id = "rename-3".to_string();
        stored.metadata.title = "original".to_string();
        manager.save_session(&stored).expect("seed");
        state.current_session_id = Some("rename-3".to_string());

        let long: String = "y".repeat(MAX_TITLE_LEN + 1);
        rename(&mut state, Some(&long));
        assert_eq!(
            manager.load_session("rename-3").unwrap().metadata.title,
            "original",
            "an over-long title is refused"
        );

        state.api_messages = Arc::new(Vec::new());
        rename(&mut state, None);
        assert_eq!(
            manager.load_session("rename-3").unwrap().metadata.title,
            "original",
            "nothing derivable leaves the title alone"
        );
    }

    /// `/sessions` lists (bare and via both aliases), refuses unknown
    /// subcommands, and the prune argument surface rejects everything but
    /// one positive integer. Every form here is a preview or a refusal —
    /// nothing is deleted.
    #[test]
    fn sessions_lists_and_validates_the_prune_arguments() {
        let _env = scratch_env();
        let manager = FileSessionStore::default_location().expect("store");
        for id in ["prune-old-1", "prune-new-1"] {
            let mut session = saved_with_failed_attempt();
            session.metadata.id = id.to_string();
            session.metadata.title = format!("{id} titled");
            manager.save_session(&session).expect("save");
        }
        let state = test_repl_state();

        sessions(&state, None);
        sessions(&state, Some(""));
        sessions(&state, Some("show"));
        sessions(&state, Some("list"));
        sessions(&state, Some("bogus-subcommand"));
        sessions(&state, Some("prune"));
        sessions(&state, Some("prune abc"));
        sessions(&state, Some("prune 0"));
        sessions(&state, Some("prune 30")); // dry run: preview only
        assert_eq!(
            manager.list_sessions().expect("list").len(),
            2,
            "every rejected or dry-run form deletes nothing"
        );
    }

    /// Regression: `/sessions prune <days> --yes` used to forward only the
    /// days word to `prune`, dropping `--yes` — a confirmed prune silently
    /// stayed a dry run. The confirmed form must delete exactly the
    /// expired sessions.
    #[test]
    fn sessions_prune_confirmed_deletes_only_expired_sessions() {
        let _env = scratch_env();
        let manager = FileSessionStore::default_location().expect("store");
        let mut expired = saved_with_failed_attempt();
        expired.metadata.id = "prune-expired".to_string();
        expired.metadata.title = "long gone".to_string();
        expired.metadata.updated_at = chrono::Utc::now() - chrono::Duration::days(90);
        manager.save_session(&expired).expect("save expired");
        let mut fresh = saved_with_failed_attempt();
        fresh.metadata.id = "prune-fresh".to_string();
        fresh.metadata.title = "just now".to_string();
        manager.save_session(&fresh).expect("save fresh");

        let state = test_repl_state();
        sessions(&state, Some("prune 30 --yes"));

        let ids: Vec<String> = manager
            .list_sessions()
            .expect("list")
            .into_iter()
            .map(|meta| meta.id)
            .collect();
        assert!(
            ids.contains(&"prune-fresh".to_string()),
            "fresh session survives: {ids:?}"
        );
        assert!(
            !ids.contains(&"prune-expired".to_string()),
            "the confirmed prune must actually delete: {ids:?}"
        );
    }

    /// `/compact`'s argument surface parses every mode word and refuses the
    /// invalid ones before any engine contact (none here — engine-less is a
    /// real REPL mode and stays silent).
    #[tokio::test]
    async fn compact_parses_modes_and_rejects_invalid_indexes() {
        let _env = scratch_env();
        let mut state = test_repl_state();

        compact(&mut state, None).await;
        compact(&mut state, Some("memory")).await;
        compact(&mut state, Some("kod")).await;
        compact(&mut state, Some("from=3")).await;
        compact(&mut state, Some("up_to=4")).await;
        compact(&mut state, Some("from=0")).await;
        compact(&mut state, Some("up_to=0")).await;
        compact(&mut state, Some("from=x")).await;
        compact(&mut state, Some("up_to=y")).await;
        compact(&mut state, Some("banana")).await;

        assert!(!state.is_compacting, "validation never flips the flag");
    }

    /// `/purge` engine-less is silent (no engine means no context to purge).
    #[tokio::test]
    async fn purge_without_an_engine_is_silent() {
        let _env = scratch_env();
        let mut state = test_repl_state();
        purge(&mut state).await;
    }

    /// `/export <path>` renders every block kind into markdown.
    #[test]
    fn export_renders_every_block_kind() {
        let _env = scratch_env();
        let ws = tempfile::tempdir().expect("tempdir");
        let mut state = test_repl_state();
        state.workspace = ws.path().to_path_buf();
        state.api_messages = Arc::new(vec![
            user_message("hello export"),
            Message {
                role: "assistant".to_string(),
                content: vec![
                    crate::models::ContentBlock::Thinking {
                        thinking: "pondering".to_string(),
                    },
                    crate::models::ContentBlock::ToolUse {
                        id: "t1".to_string(),
                        name: "bash".to_string(),
                        input: serde_json::json!({}),
                        caller: None,
                    },
                    crate::models::ContentBlock::Text {
                        text: "done".to_string(),
                        cache_control: None,
                    },
                ],
            },
            Message {
                role: "user".to_string(),
                content: vec![
                    crate::models::ContentBlock::Image {
                        source: codesmith_agent::models::ImageSource::File {
                            path: std::path::PathBuf::from("/tmp/pic.png"),
                            media_type: None,
                        },
                    },
                    crate::models::ContentBlock::ToolResult {
                        tool_use_id: "t1".to_string(),
                        content: "ok".to_string(),
                        is_error: None,
                        content_blocks: None,
                    },
                ],
            },
        ]);
        let out = ws.path().join("exported.md");

        export(&state, Some(out.to_str().unwrap()));

        let body = std::fs::read_to_string(&out).expect("export file");
        assert!(body.starts_with("# Chat Export"));
        assert!(body.contains("**Model:** test-model"));
        assert!(body.contains(&format!("**Workspace:** {}", ws.path().display())));
        assert!(body.contains("**You:**"));
        assert!(body.contains("hello export"));
        assert!(body.contains("**Assistant:**"));
        assert!(body.contains("[thinking] pondering"));
        assert!(body.contains("[tool call: bash]"));
        assert!(body.contains("image at /tmp/pic.png"));
        assert!(
            body.contains("ToolResult"),
            "fallback renders the block kind"
        );
    }

    /// `/export` with no path writes a timestamped file into the CWD; the
    /// test deletes exactly the exports it created (crate dir during cargo
    /// test) so no repository artifacts remain.
    #[test]
    fn export_without_a_path_writes_into_the_cwd_and_cleans_up() {
        let _env = scratch_env();
        let state = test_repl_state();
        let before: Vec<std::path::PathBuf> = std::fs::read_dir(".")
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_chat_export(path))
            .collect();

        export(&state, None);

        let created: Vec<std::path::PathBuf> = std::fs::read_dir(".")
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_chat_export(path) && !before.contains(path))
            .collect();
        assert_eq!(created.len(), 1, "one timestamped export per call");
        let body = std::fs::read_to_string(&created[0]).expect("read export");
        assert!(body.starts_with("# Chat Export"));
        for path in created {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The write-failure branch (target is a directory) reports cleanly.
    #[test]
    fn export_onto_a_directory_reports_failure() {
        let _env = scratch_env();
        let out = tempfile::tempdir().expect("tempdir");
        let state = test_repl_state();
        export(&state, Some(out.path().to_str().unwrap()));
        assert!(out.path().is_dir(), "the target is untouched");
    }

    fn is_chat_export(path: &std::path::Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("chat_export_") && name.ends_with(".md"))
    }
}
