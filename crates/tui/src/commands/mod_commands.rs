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
    let sub = parts
        .get(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or("");
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
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

fn list(app: &App) -> CommandResult {
    if let Some(err) = guard_enabled(app) {
        return err;
    }
    CommandResult::message(mod_ops::list_mods(&app.workspace, &app.mod_state))
}

fn status(app: &App) -> CommandResult {
    if let Some(err) = guard_enabled(app) {
        return err;
    }
    let mut out = mod_ops::mod_status(&app.workspace, &app.mod_state);
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
    match mod_ops::mod_info(&app.workspace, &app.mod_state, id) {
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
    match mod_ops::activate(&app.workspace, &mut app.mod_state, id) {
        Ok(msg) => {
            let reload_note = match reload_runner(app) {
                Some(note) => note,
                None => "Runner not bound; the mod loads at the next engine build.".to_string(),
            };
            CommandResult::message(format!("{msg}\n{reload_note}"))
        }
        Err(e) => CommandResult::error(e),
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
    match mod_ops::set_enabled(&mut app.mod_state, id, true) {
        Ok(msg) => CommandResult::message(msg),
        Err(e) => CommandResult::error(e),
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
    match mod_ops::set_enabled(&mut app.mod_state, id, false) {
        Ok(msg) => CommandResult::message(msg),
        Err(e) => CommandResult::error(e),
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
    match mod_ops::remove(&app.workspace, &mut app.mod_state, id) {
        Ok(msg) => {
            let reload_note = match reload_runner(app) {
                Some(note) => note,
                None => "Runner not bound; state is cleared regardless.".to_string(),
            };
            CommandResult::message(format!("{msg}\n{reload_note}"))
        }
        Err(e) => CommandResult::error(e),
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
    #[test]
    fn try_dispatch_prefix_guard_rejects_non_mods_command() {
        let input = "/extension list";
        let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
        let cmd = parts[0].to_lowercase();
        let cmd = cmd.strip_prefix('/').unwrap_or(&cmd);
        assert_ne!(cmd, "mods");
    }
}
