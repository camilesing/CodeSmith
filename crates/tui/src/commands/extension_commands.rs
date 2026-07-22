//! `/extension` command group (spec §6.3). Dispatched via the
//! `extension_commands::try_dispatch` runtime lookup wired into
//! `execute()` between user-defined and the static `match` (Task 8.3).
//!
//! Slice 1 (phase 1, static): `list` / `info` / `enable` / `disable` /
//! `status` / `reload` work for compiled-in extensions. `install` /
//! `uninstall` stub "requires dylib loader (phase 2)" (§F5).
//!
//! Extension-registered slash commands (`registerCommand` wrap, spec §5.1.2)
//! are dispatched separately via `ExtensionRunner::try_dispatch_command` —
//! §F2 wires that tier; slice 1 ships only the `/extension` meta-commands.

use crate::tui::app::App;

use super::CommandResult;

/// Runtime lookup mirror of `user_commands::try_dispatch_user_command`
/// (`crates/tui/src/commands/user_commands.rs:193`). Called from `execute()`
/// AFTER user-defined commands, BEFORE the static `match`. Returns `None`
/// when the command isn't an `/extension` invocation so `execute` falls
/// through to the static arms.
pub fn try_dispatch(app: &mut App, input: &str) -> Option<CommandResult> {
    let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
    let command = parts[0].to_lowercase();
    let command = command.strip_prefix('/').unwrap_or(&command);
    if command != "extension" && command != "ext" {
        return None;
    }
    let sub = parts
        .get(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or("");
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
    Some(match sub {
        "list" | "ls" => list(app),
        "info" => info(app, arg),
        "enable" => enable(app, arg),
        "disable" => disable(app, arg),
        "status" => status(app),
        "reload" => reload(app),
        "install" => install_stub(arg),
        "uninstall" => uninstall_stub(arg),
        _ => CommandResult::error(format!(
            "Unsupported /extension subcommand: {sub:?}. Try: list, info, enable, disable, status, reload"
        )),
    })
}

fn runner(app: &App) -> Option<&std::sync::Arc<codesmith_extensions::ExtensionRunner>> {
    // The runner is bound to the engine host in `build_extension_runtime`
    // (Task 9) and surfaced on `App::extension_runner`. `None` until the
    // engine builds (embeds/tests) — `status`/`reload` report "not bound".
    app.extension_runner.as_ref()
}

fn list(_app: &App) -> CommandResult {
    // Slice 1: list compiled-in extensions via `discover_static()`.
    let discovered = codesmith_extensions::discover_static();
    if discovered.is_empty() {
        return CommandResult::message("No extensions discovered.");
    }
    let mut out = String::from("Compiled-in extensions:\n");
    for reg in discovered {
        out.push_str(&format!("  {} (v{})\n", reg.metadata.id, reg.metadata.version));
    }
    CommandResult::message(out)
}

fn info(_app: &App, arg: &str) -> CommandResult {
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /extension info <id>");
    }
    let discovered = codesmith_extensions::discover_static();
    let Some(reg) = discovered.iter().find(|r| r.metadata.id == id) else {
        return CommandResult::error(format!("No extension with id '{id}'."));
    };
    CommandResult::message(format!(
        "id: {}\nversion: {}\ncontributions: (slice 1: see /extension status)\n",
        reg.metadata.id, reg.metadata.version
    ))
}

fn enable(app: &mut App, arg: &str) -> CommandResult {
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /extension enable <id>");
    }
    match app.extension_state.set_enabled(id, true) {
        Ok(()) => CommandResult::message(format!(
            "Enabled extension '{id}' (takes effect on next /extension reload)."
        )),
        Err(e) => CommandResult::error(format!("Failed to enable: {e}")),
    }
}

fn disable(app: &mut App, arg: &str) -> CommandResult {
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /extension disable <id>");
    }
    match app.extension_state.set_enabled(id, false) {
        Ok(()) => CommandResult::message(format!(
            "Disabled extension '{id}' (takes effect on next /extension reload)."
        )),
        Err(e) => CommandResult::error(format!("Failed to disable: {e}")),
    }
}

fn status(app: &App) -> CommandResult {
    let Some(runner) = runner(app) else {
        return CommandResult::message("Extension runner not bound (no engine).");
    };
    CommandResult::message(format!(
        "Extension runner: generation={}, commands=[{}], tools={}\n\
         (slice 1: handler list + dispatch stats are §F2)",
        runner.generation(),
        runner.bound_command_names().join(", "),
        runner.bound_tools().len()
    ))
}

fn reload(app: &mut App) -> CommandResult {
    let Some(runner) = app.extension_runner.clone() else {
        return CommandResult::error("Extension runner not bound.");
    };
    let gen_before = runner.generation();
    // §F2b T7 — live reload on the SHARED runner Arc: clear → invalidate →
    // re-discover → re-load → re-bind. The Engine's per-turn
    // `HostAgentExecutor` holds the same Arc, so the next turn sees the new
    // handlers without an engine rebuild. `cancel_token` is fresh (no §F2b
    // handler reads the ctx signal yet; sharing the engine's token is §F2c).
    crate::core::engine::reload_extension_runtime(
        &runner,
        &app.workspace,
        &app.extension_state,
        tokio_util::sync::CancellationToken::new(),
    );
    CommandResult::message(format!(
        "Extension runner reloaded (generation {} → {}). Re-discovered + re-loaded compiled-in extensions on the shared runner (live for the next turn).",
        gen_before,
        runner.generation()
    ))
}

fn install_stub(arg: &str) -> CommandResult {
    CommandResult::error(format!(
        "/extension install {arg} requires the dylib loader (phase 2, §F5). Slice 1 supports compiled-in extensions only."
    ))
}

fn uninstall_stub(arg: &str) -> CommandResult {
    CommandResult::error(format!(
        "/extension uninstall {arg} requires the dylib loader (phase 2, §F5)."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_dispatch_prefix_guard_rejects_non_extension_command() {
        // Mirrors the prefix-guard logic in `try_dispatch` without
        // constructing an `App` (the full App-based dispatch is exercised by
        // the `every_registered_command_dispatches_to_a_handler` smoke test
        // in `commands/mod.rs`, which now includes `/extension`).
        let input = "/skills list";
        let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
        let cmd = parts[0].to_lowercase();
        let cmd = cmd.strip_prefix('/').unwrap_or(&cmd);
        assert_ne!(cmd, "extension");
        assert_ne!(cmd, "ext");
    }

    #[test]
    fn install_stub_is_phase_2_message() {
        let r = install_stub("git:foo/bar");
        assert!(r.is_error);
        let msg = r.message.expect("error has a message");
        assert!(msg.contains("phase 2"), "got: {msg}");
    }

    #[test]
    fn uninstall_stub_is_phase_2_message() {
        let r = uninstall_stub("my-ext");
        assert!(r.is_error);
        assert!(r.message.unwrap().contains("phase 2"));
    }
}
