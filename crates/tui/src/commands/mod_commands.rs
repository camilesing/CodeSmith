//! `/mods` command group (§F script mod layer Phase B8). Thin veneer over
//! the shared [`crate::mod_ops`] layer — the model-visible `manage_mods`
//! tool drives the same operations, so the consent/enablement semantics
//! cannot drift between the two entry points.

use crate::mod_ops;
use crate::tui::app::App;

use super::CommandResult;

/// Runtime lookup mirror of `extension_commands::try_dispatch`. Called from
/// `execute()` after `/extension`, before the static `match`. Returns `None`
/// when the command isn't a `/mods` invocation so `execute` falls through.
pub fn try_dispatch(app: &mut App, input: &str) -> Option<CommandResult> {
    let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
    let command = parts[0].to_lowercase();
    let command = command.strip_prefix('/').unwrap_or(&command);
    if command != "mods" {
        return None;
    }
    // Split the remainder ONCE into subcommand + argument: taking `sub` as
    // the first word but `arg` as the whole remainder (the round-3 review
    // bug) made every id-taking subcommand look up "activate mymod" as the
    // id and always fail.
    let rest = parts.get(1).map(|s| s.trim()).unwrap_or("");
    let (sub, arg) = match rest.split_once(char::is_whitespace) {
        Some((s, a)) => (s, a.trim()),
        None => (rest, ""),
    };
    Some(match sub {
        "list" | "ls" => list(app),
        "status" => status(app),
        "info" => info(app, arg),
        "activate" => activate(app, arg),
        "enable" => enable(app, arg),
        "disable" => disable(app, arg),
        "remove" | "uninstall" => remove(app, arg),
        "reload" => reload(app),
        _ => CommandResult::error(format!(
            "Unsupported /mods subcommand: {sub:?}. Try: list, status, info <id>, activate <id>, enable <id>, disable <id>, remove <id>, reload"
        )),
    })
}

fn guard_enabled(app: &App) -> Option<CommandResult> {
    if app.mods_enabled {
        None
    } else {
        Some(CommandResult::error(
            "The mods layer is disabled ([mods] enabled = false in config.toml).",
        ))
    }
}

/// Disk-fresh read for the read-only subcommands: the `manage_mods` tool
/// mutates the store behind the App copy's back, so the App copy is only a
/// startup snapshot — never the truth for display. `Err` is the display
/// message.
fn fresh_state() -> Result<crate::mod_state::ModStateStore, String> {
    crate::mod_state::ModStateStore::load_default()
        .map_err(|e| format!("Mod state unreadable ({e}); /mods reload after fixing it."))
}

/// Serialized load→mutate→persist shared with the `manage_mods` tool (each
/// persist rewrites the whole file — unlocked racing mutations are
/// last-writer-wins). The fresh store is written back to the App on a
/// successful mutation so subsequent reads agree with what just happened.
/// Outer `Err` = state unreadable (message), inner = the operation's own
/// result.
fn mutate_state(
    app: &mut App,
    f: impl FnOnce(&mut crate::mod_state::ModStateStore) -> Result<String, String>,
) -> Result<Result<String, String>, String> {
    let _guard = mod_ops::mod_state_lock();
    let mut state = fresh_state()?;
    let inner = f(&mut state);
    if inner.is_ok() {
        app.mod_state = state;
    }
    Ok(inner)
}

fn list(app: &App) -> CommandResult {
    if let Some(err) = guard_enabled(app) {
        return err;
    }
    let state = match fresh_state() {
        Ok(s) => s,
        Err(e) => return CommandResult::error(e),
    };
    CommandResult::message(mod_ops::list_mods(&app.workspace, &state))
}

fn status(app: &App) -> CommandResult {
    if let Some(err) = guard_enabled(app) {
        return err;
    }
    let state = match fresh_state() {
        Ok(s) => s,
        Err(e) => return CommandResult::error(e),
    };
    let mut out = mod_ops::mod_status(&app.workspace, &state);
    if let Some(runner) = app.extension_runner.as_ref() {
        out.push_str(&format!(
            "\nrunner: generation={}, tools={}, commands={}",
            runner.generation(),
            runner.bound_tools().len(),
            runner.bound_command_names().len()
        ));
    }
    CommandResult::message(out)
}

fn info(app: &App, arg: &str) -> CommandResult {
    if let Some(err) = guard_enabled(app) {
        return err;
    }
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /mods info <id>");
    }
    let state = match fresh_state() {
        Ok(s) => s,
        Err(e) => return CommandResult::error(e),
    };
    match mod_ops::mod_info(&app.workspace, &state, id) {
        Some(text) => CommandResult::message(text),
        None => CommandResult::error(format!("No mod with id '{id}'.")),
    }
}

fn activate(app: &mut App, arg: &str) -> CommandResult {
    if let Some(err) = guard_enabled(app) {
        return err;
    }
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /mods activate <id>");
    }
    let outcome: Result<Result<String, String>, String> = {
        let ws = app.workspace.clone();
        mutate_state(app, |state| mod_ops::activate(&ws, state, id))
    };
    match outcome {
        Err(e) => CommandResult::error(e),
        Ok(Err(e)) => CommandResult::error(e),
        Ok(Ok(msg)) => {
            let reload_note = match reload_runner(app) {
                Some(note) => note,
                None => "Runner not bound; the mod loads at the next engine build.".to_string(),
            };
            CommandResult::message(format!("{msg}\n{reload_note}"))
        }
    }
}

fn enable(app: &mut App, arg: &str) -> CommandResult {
    if let Some(err) = guard_enabled(app) {
        return err;
    }
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /mods enable <id>");
    }
    match mutate_state(app, |state| mod_ops::set_enabled(state, id, true)) {
        Err(e) => CommandResult::error(e),
        Ok(Err(e)) => CommandResult::error(e),
        Ok(Ok(msg)) => {
            // Reload immediately, matching activate/remove here AND the
            // manage_mods tool: the runner is built per generation, so a
            // deferred enablement would have no effect until an unrelated
            // reload.
            let reload_note = match reload_runner(app) {
                Some(note) => note,
                None => "Runner not bound; takes effect at the next engine build.".to_string(),
            };
            CommandResult::message(format!("{msg}\n{reload_note}"))
        }
    }
}

fn disable(app: &mut App, arg: &str) -> CommandResult {
    if let Some(err) = guard_enabled(app) {
        return err;
    }
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /mods disable <id>");
    }
    match mutate_state(app, |state| mod_ops::set_enabled(state, id, false)) {
        Err(e) => CommandResult::error(e),
        Ok(Err(e)) => CommandResult::error(e),
        Ok(Ok(msg)) => {
            let reload_note = match reload_runner(app) {
                Some(note) => note,
                None => "Runner not bound; takes effect at the next engine build.".to_string(),
            };
            CommandResult::message(format!("{msg}\n{reload_note}"))
        }
    }
}

fn remove(app: &mut App, arg: &str) -> CommandResult {
    if let Some(err) = guard_enabled(app) {
        return err;
    }
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /mods remove <id>");
    }
    let outcome: Result<Result<String, String>, String> = {
        let ws = app.workspace.clone();
        mutate_state(app, |state| mod_ops::remove(&ws, state, id))
    };
    match outcome {
        Err(e) => CommandResult::error(e),
        Ok(Err(e)) => CommandResult::error(e),
        Ok(Ok(msg)) => {
            let reload_note = match reload_runner(app) {
                Some(note) => note,
                None => "Runner not bound; state is cleared regardless.".to_string(),
            };
            CommandResult::message(format!("{msg}\n{reload_note}"))
        }
    }
}

fn reload_runner(app: &App) -> Option<String> {
    let runner = app.extension_runner.clone()?;
    let cancel = app.extension_shared_cancel_token.clone()?;
    Some(mod_ops::reload_mods(
        &runner,
        &app.workspace,
        cancel,
        app.mods_enabled,
    ))
}

fn reload(app: &mut App) -> CommandResult {
    if let Some(err) = guard_enabled(app) {
        return err;
    }
    match reload_runner(app) {
        Some(note) => CommandResult::message(note),
        None => CommandResult::error("Extension runner not bound (no engine)."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tui::app::{App, TuiOptions};
    use tempfile::TempDir;

    fn create_test_app_with_tmpdir(tmpdir: &TempDir) -> App {
        let options = TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace: tmpdir.path().to_path_buf(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            use_alt_screen: true,
            use_mouse_capture: false,
            use_bracketed_paste: true,
            max_subagents: 1,
            skills_dir: tmpdir.path().join("skills"),
            memory_path: tmpdir.path().join("memory.md"),
            notes_path: tmpdir.path().join("notes.txt"),
            mcp_config_path: tmpdir.path().join("mcp.json"),
            use_memory: false,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn try_dispatch_prefix_guard_rejects_non_mods_command() {
        // Assert on the real dispatch path — a locally re-implemented
        // prefix parse would pass even if `try_dispatch` matched every
        // command.
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        assert!(
            try_dispatch(&mut app, "/extension list").is_none(),
            "non-/mods input must fall through to the static match"
        );
    }

    /// Regression (review round 3): `arg` used to include the subcommand
    /// token, so `/mods activate mymod` looked up the id "activate mymod"
    /// and every id-taking subcommand always failed.
    #[test]
    fn dispatch_passes_only_the_argument_to_id_taking_subcommands() {
        // Hermetic: this drives the real dispatch chain (try_dispatch →
        // mutate_state → ModStateStore::load_default), which otherwise
        // resolves/creates the real user state dir and discovery scans the
        // real ~/.codesmith/mods.
        let _env = crate::test_support::lock_test_env();
        let tmpdir = TempDir::new().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmpdir.path());
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let result = try_dispatch(&mut app, "/mods activate mymod").expect("a /mods command");
        assert!(result.is_error);
        let msg = result.message.unwrap_or_default();
        assert!(
            msg.contains("No mod with id 'mymod'"),
            "id-taking subcommand got the subcommand token too: {msg}"
        );
    }
}
