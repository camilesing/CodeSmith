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
    let rest: Vec<&str> = parts.collect();
    match sub.as_str() {
        "" | "list" | "ls" => {
            let workspace = state.workspace.clone();
            let listing = tokio::task::spawn_blocking(move || {
                let store = crate::mod_state::ModStateStore::load_default()
                    .map_err(|err| format!("load mod state: {err}"))?;
                Ok::<String, String>(crate::mod_ops::list_mods(&workspace, &store))
            })
            .await
            .unwrap_or_else(|err| Err(format!("list worker failed: {err}")));
            match listing {
                Ok(text) => println!("{text}"),
                Err(err) => println!("{} {err}", "✗".red()),
            }
        }
        "status" => status(state).await,
        "info" => match rest.first() {
            Some(id) => info(state, id).await,
            None => usage(),
        },
        "activate" => match rest.first() {
            Some(id) => mutate(state, id, Action::Activate).await,
            None => usage(),
        },
        "enable" => match rest.first() {
            Some(id) => mutate(state, id, Action::Enable).await,
            None => usage(),
        },
        "disable" => match rest.first() {
            Some(id) => mutate(state, id, Action::Disable).await,
            None => usage(),
        },
        "remove" => match rest.first() {
            Some(id) => mutate(state, id, Action::Remove).await,
            None => usage(),
        },
        "reload" => println!("{}", reload_note(state).await),
        other => println!(
            "{} unknown subcommand `{other}` — list | status | info | activate | enable | disable | remove | reload",
            "✗".red()
        ),
    }
}

fn usage() {
    println!(
        "{}",
        "usage: /mods [list|status|info <id>|activate <id>|enable <id>|disable <id>|remove <id>|reload]".dimmed()
    );
}

async fn status(state: &ReplState) {
    let workspace = state.workspace.clone();
    let listing = tokio::task::spawn_blocking(move || {
        let store = crate::mod_state::ModStateStore::load_default()
            .map_err(|err| format!("load mod state: {err}"))?;
        Ok::<String, String>(crate::mod_ops::list_mods(&workspace, &store))
    })
    .await
    .unwrap_or_else(|err| Err(format!("list worker failed: {err}")));
    match listing {
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
    let result = tokio::task::spawn_blocking(move || {
        // Same protocol as the manage_mods tool: guard scopes to
        // load→mutate→persist only — reload re-acquires the guard at the
        // activation-hash backfill (std Mutex is not reentrant).
        let _guard = crate::mod_ops::mod_state_lock();
        let mut store = crate::mod_state::ModStateStore::load_default()
            .map_err(|err| format!("load mod state: {err}"))?;
        match action {
            Action::Activate => crate::mod_ops::activate(&workspace, &mut store, &id),
            Action::Enable => crate::mod_ops::set_enabled(&mut store, &id, true),
            Action::Disable => crate::mod_ops::set_enabled(&mut store, &id, false),
            Action::Remove => crate::mod_ops::remove(&workspace, &mut store, &id),
        }
    })
    .await
    .unwrap_or_else(|err| Err(format!("worker failed: {err}")));
    match result {
        Ok(text) => {
            println!("{text}");
            println!("{}", reload_note(state).await);
        }
        Err(err) => println!("{} {err}", "✗".red()),
    }
}

/// Reload the whole extension layer (compiled-in + dylib + mods) from
/// disk-fresh state — shared with `/extension reload`. Unavailable when no
/// runner is bound (embeds/tests).
pub(super) async fn reload_note(state: &ReplState) -> String {
    let Some(runner) = state.extension_runner.clone() else {
        return "Reload unavailable in this session (no bound extension runner); changes take effect on the next session start or /mods reload.".to_string();
    };
    let Ok(engine) = state.engine() else {
        return "Reload unavailable in this session (engine not running).".to_string();
    };
    let token = engine.cancel_token.clone();
    let workspace = state.workspace.clone();
    let mods_enabled = state.config.mods_enabled();
    tokio::task::spawn_blocking(move || {
        crate::mod_ops::reload_mods(&runner, &workspace, token, mods_enabled)
    })
    .await
    .unwrap_or_else(|err| format!("reload worker failed: {err}"))
}
