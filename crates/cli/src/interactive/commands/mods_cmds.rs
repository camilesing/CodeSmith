//! `/mods …` — the in-REPL script-mod manager (the §F port). Every action
//! maps 1:1 onto [`crate::mod_ops`], the single implementation shared with
//! the model-facing `manage_mods` tool; mutations follow the same protocol:
//! `mod_state_lock` → disk-fresh `ModStateStore` → mutate → reload note.

use colored::Colorize;

use super::ReplState;

/// `/mods [list|status|info|activate|enable|disable|remove|reload]`
pub async fn mods_command(state: &ReplState, arg: &str) {
    let mut parts = arg.split_whitespace();
    let sub = parts.next().unwrap_or("list").to_ascii_lowercase();
    let id = parts.next();
    match sub.as_str() {
        "list" | "ls" => match listing(state).await {
            Ok(text) => println!("{text}"),
            Err(err) => println!("{} {err}", "✗".red()),
        },
        "status" => status(state).await,
        "info" => match id {
            Some(id) => info(state, id).await,
            None => usage(),
        },
        "activate" => match id {
            Some(id) => mutate(state, id, Action::Activate).await,
            None => usage(),
        },
        "enable" => match id {
            Some(id) => mutate(state, id, Action::Enable).await,
            None => usage(),
        },
        "disable" => match id {
            Some(id) => mutate(state, id, Action::Disable).await,
            None => usage(),
        },
        "remove" => match id {
            Some(id) => mutate(state, id, Action::Remove).await,
            None => usage(),
        },
        "reload" => match reload_note(state).await {
            Ok(text) => println!("{text}"),
            Err(err) => println!("{} {err}", "✗".red()),
        },
        other => println!(
            "{} unknown subcommand `{other}` — list | status | info | activate | enable | disable | remove | reload",
            "✗".red()
        ),
    }
}

/// The mod listing both `/mods` (bare and `list`) and `/mods status` print,
/// read through the same blocking worker.
async fn listing(state: &ReplState) -> Result<String, String> {
    let workspace = state.workspace.clone();
    let runner = state.extension_runner.clone();
    tokio::task::spawn_blocking(move || {
        let store = crate::mod_state::ModStateStore::load_default()
            .map_err(|err| format!("load mod state: {err}"))?;
        Ok::<String, String>(crate::mod_ops::list_mods(
            &workspace,
            &store,
            runner.as_deref(),
        ))
    })
    .await
    .unwrap_or_else(|err| Err(format!("list worker failed: {err}")))
}

fn usage() {
    println!(
        "{}",
        "usage: /mods [list|status|info <id>|activate <id>|enable <id>|disable <id>|remove <id>|reload]".dimmed()
    );
}

async fn status(state: &ReplState) {
    match listing(state).await {
        Ok(text) => println!("{text}"),
        Err(err) => println!("{} {err}", "✗".red()),
    }
    if let Some(runner) = state.extension_runner.as_ref() {
        println!("extension runtime: generation {}", runner.generation());
    } else {
        println!(
            "{}",
            "no extension runner bound in this session (embed or pre-engine start)".dimmed()
        );
    }
    println!(
        "mods layer: {}",
        if state.config.mods_enabled() {
            "enabled"
        } else {
            "disabled ([mods] enabled = false)"
        }
    );
    if let Some(engine) = state.engine.as_ref() {
        if !engine.mods_pending.is_empty() {
            let ids: Vec<&str> = engine.mods_pending.iter().map(|p| p.id.as_str()).collect();
            println!("pending activation: {}", ids.join(", "));
        }
        let failed = crate::core::engine::failed_audit_lines(&engine.mods_audit);
        if !failed.is_empty() {
            println!("{} {}", "✗".red(), failed.join("; "));
        }
    }
}

async fn info(state: &ReplState, id: &str) {
    let workspace = state.workspace.clone();
    let id = id.to_string();
    let info = tokio::task::spawn_blocking(move || {
        let store = crate::mod_state::ModStateStore::load_default()
            .map_err(|err| format!("load mod state: {err}"))?;
        let mods = crate::mod_ops::discover_workspace_mods(&workspace);
        let Some(m) = mods.iter().find(|m| m.id == id) else {
            return Err(format!("no mod with id '{id}' — /mods list shows what exists"));
        };
        let status = match crate::mod_ops::classify_mod(&store, m) {
            crate::mod_ops::ModListStatus::Disabled => "disabled".to_string(),
            crate::mod_ops::ModListStatus::PendingActivation => {
                "pending activation".to_string()
            }
            crate::mod_ops::ModListStatus::Loaded => "activated".to_string(),
            crate::mod_ops::ModListStatus::PendingReconsent { reason } => {
                format!("pending re-activation ({reason})")
            }
        };
        Ok(format!(
            "mod {}:\n  name: {}\n  version: {}\n  scope: {}\n  dir: {}\n  entry: {}\n  status: {}\n  description: {}",
            m.id,
            m.name,
            m.version,
            if m.global { "global" } else { "project" },
            m.dir.display(),
            m.entry_path.display(),
            status,
            m.description.as_deref().unwrap_or("(none)"
            )
        ))
    })
    .await
    .unwrap_or_else(|err| Err(format!("info worker failed: {err}")));
    match info {
        Ok(text) => println!("{text}"),
        Err(err) => println!("{} {err}", "✗".red()),
    }
}

enum Action {
    Activate,
    Enable,
    Disable,
    Remove,
}

async fn mutate(state: &ReplState, id: &str, action: Action) {
    let workspace = state.workspace.clone();
    let id = id.to_string();
    let runner = state.extension_runner.clone();
    let mods_enabled = state.config.mods_enabled();
    // Computed before `action` moves into the persist closure below.
    let load_after_persist = matches!(action, Action::Activate | Action::Enable);
    // Clones for the persist closure; the originals stay for the per-mod
    // transition note after it.
    let ws_in = workspace.clone();
    let id_in = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Same protocol as the manage_mods tool: guard scopes to
        // load→mutate→persist only — the per-mod transition after it may
        // re-acquire the guard at the activation-hash backfill (std Mutex
        // is not reentrant).
        let _guard = crate::mod_ops::mod_state_lock();
        let mut store = crate::mod_state::ModStateStore::load_default()
            .map_err(|err| format!("load mod state: {err}"))?;
        match action {
            Action::Activate => crate::mod_ops::activate(&ws_in, &mut store, &id_in),
            Action::Enable => crate::mod_ops::set_enabled(&mut store, &id_in, true),
            Action::Disable => crate::mod_ops::set_enabled(&mut store, &id_in, false),
            Action::Remove => crate::mod_ops::remove(&ws_in, &mut store, &id_in),
        }
    })
    .await
    .unwrap_or_else(|err| Err(format!("worker failed: {err}")));
    match result {
        Ok(text) => {
            println!("{text}");
            // Single-mod transition instead of a whole-layer reload:
            // activate/enable compile + bind just this mod; disable/remove
            // evict it and replay the survivors from their retained
            // compiled forms.
            let note = match runner {
                Some(runner) => {
                    tokio::task::spawn_blocking(move || {
                        if load_after_persist {
                            crate::mod_ops::load_mod(&runner, &workspace, &id, mods_enabled)
                        } else {
                            crate::mod_ops::unload_mod(&runner, &id)
                        }
                    })
                    .await
                    .unwrap_or_else(|err| format!("mod transition failed: {err}"))
                }
                None => "No extension runner in this session; the change takes effect at the next session start."
                    .to_string(),
            };
            println!("{note}");
        }
        Err(err) => println!("{} {err}", "✗".red()),
    }
}

/// Reload the whole extension layer (compiled-in + dylib + mods) from
/// disk-fresh state — shared with `/extension reload`. Unavailable when no
/// runner is bound (embeds/tests). Reads the engine's captured reload ctx
/// (runner + token + `mods_enabled` + the session-mod registry) so the
/// REPL reload shares the tool path's gating and replays session mods.
pub(super) async fn reload_note(state: &ReplState) -> Result<String, String> {
    let Ok(engine) = state.engine() else {
        return Err("Reload unavailable in this session (engine not running).".to_string());
    };
    let Some(ctx) = engine.mod_reload.clone() else {
        return Err("Reload unavailable in this session (no bound extension runner); changes take effect on the next session start or /mods reload.".to_string());
    };
    let runner = ctx.runner.clone();
    let workspace = ctx.workspace.clone();
    let token = ctx.shared_cancel_token.clone();
    let mods_enabled = ctx.mods_enabled;
    let session_mods = ctx.session_mods.clone();
    let note = tokio::task::spawn_blocking(move || {
        crate::mod_ops::reload_mods(&runner, &workspace, token, mods_enabled, &session_mods)
    })
    .await
    .map_err(|err| format!("reload worker failed: {err}"))?;
    // `reload_mods` reports failures in-band ("Reload aborted: …"); route
    // them to the error marker at this boundary.
    if note.starts_with("Reload aborted") {
        Err(note)
    } else {
        Ok(note)
    }
}
