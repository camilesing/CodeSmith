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
        "install" => install(app, arg),
        "uninstall" => uninstall(app, arg),
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

fn list(app: &App) -> CommandResult {
    let mut out = String::new();
    let mut ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    // Compiled-in (phase 1).
    let discovered = codesmith_extensions::discover_static();
    let mut compiled = String::new();
    for reg in &discovered {
        ids.insert(reg.metadata.id.to_string());
        compiled.push_str(&format!(
            "  {} (v{}) [compiled]\n",
            reg.metadata.id, reg.metadata.version
        ));
    }
    if !discovered.is_empty() {
        out.push_str("Compiled-in extensions:\n");
        out.push_str(&compiled);
    }

    // §F5b — dylib-discovered (global + project; configured paths → §F5c).
    let global_dir = crate::config::effective_home_dir()
        .map(|home| home.join(".codesmith").join("extensions"));
    let project_dir = app.workspace.join(".codesmith").join("extensions");
    let global_roots: Vec<std::path::PathBuf> = global_dir.into_iter().collect();
    let project_roots = vec![project_dir];
    let dylibs = codesmith_extensions::discover_dylib(&global_roots, &project_roots);
    if !dylibs.is_empty() {
        out.push_str("Dylib extensions:\n");
        for d in &dylibs {
            // Skip ids already shown as compiled-in (dedup by id).
            if ids.insert(d.id.clone()) {
                out.push_str(&format!(
                    "  {} (v{}) [dylib, {}]\n",
                    d.id,
                    d.version,
                    if d.global { "global" } else { "project" },
                ));
            }
        }
    }

    if out.is_empty() {
        return CommandResult::message("No extensions discovered.");
    }
    CommandResult::message(out)
}

fn info(app: &App, arg: &str) -> CommandResult {
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /extension info <id>");
    }
    // Compiled-in lookup.
    let discovered = codesmith_extensions::discover_static();
    if let Some(reg) = discovered.iter().find(|r| r.metadata.id == id) {
        return CommandResult::message(format!(
            "id: {}\nversion: {}\nsource: compiled-in\ncontributions: (see /extension status)\n",
            reg.metadata.id, reg.metadata.version
        ));
    }
    // §F5b — dylib lookup.
    let global_dir = crate::config::effective_home_dir()
        .map(|home| home.join(".codesmith").join("extensions"));
    let project_dir = app.workspace.join(".codesmith").join("extensions");
    let global_roots: Vec<std::path::PathBuf> = global_dir.into_iter().collect();
    let project_roots = vec![project_dir];
    let dylibs = codesmith_extensions::discover_dylib(&global_roots, &project_roots);
    if let Some(d) = dylibs.into_iter().find(|d| d.id == id) {
        return CommandResult::message(format!(
            "id: {}\nversion: {}\nsource: dylib ({})\npath: {}\ncontributions: (see /extension status)\n",
            d.id,
            d.version,
            if d.global { "global" } else { "project" },
            d.dylib_path.display(),
        ));
    }
    CommandResult::error(format!("No extension with id '{id}'."))
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
    // handlers without an engine rebuild. §F2c Layer 2: pass the engine's
    // **shared** cancel-token `Arc` (not a fresh token) so a handler's
    // `ctx.signal()` reflects the engine's per-turn `reset_cancel_token`.
    let Some(shared_cancel_token) = app.extension_shared_cancel_token.clone() else {
        return CommandResult::error("Extension runner not bound.");
    };
    crate::core::engine::reload_extension_runtime(
        &runner,
        &app.workspace,
        &app.extension_state,
        shared_cancel_token,
    );
    CommandResult::message(format!(
        "Extension runner reloaded (generation {} → {}). Re-discovered + re-loaded compiled-in extensions on the shared runner (live for the next turn).",
        gen_before,
        runner.generation()
    ))
}

/// Pre-App validation for `/extension install`: parse + crate/prebuilt guard
/// (§F5c R4). Returns `Some(error)` for bad args / not-yet-implemented kinds;
/// `None` to proceed with the `App`. No `App` access needed → unit-testable.
fn install_precheck(arg: &str) -> Option<CommandResult> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Some(CommandResult::error(
            "Usage: /extension install <kind>:<body>[@<ref>] [--global]  (kinds: git, path)",
        ));
    }
    let spec = match codesmith_extensions::SourceSpec::parse(arg) {
        Ok(s) => s,
        Err(e) => return Some(CommandResult::error(format!("Invalid source spec: {e}"))),
    };
    if matches!(
        spec.kind,
        codesmith_extensions::SourceKind::CratesIo | codesmith_extensions::SourceKind::Prebuilt
    ) {
        return Some(CommandResult::error(format!(
            "§F5c-later: {:?} source not yet implemented (this slice supports git/path only)",
            spec.kind
        )));
    }
    None
}

/// Extensions root for a scope (§F5c). Global =
/// `~/.codesmith/extensions` (falls back to project if no home dir); Project
/// = `<workspace>/.codesmith/extensions`.
fn extensions_root_for(
    scope: codesmith_extensions::InstallScope,
    workspace: &std::path::Path,
) -> std::path::PathBuf {
    match scope {
        codesmith_extensions::InstallScope::Global => crate::config::effective_home_dir()
            .map(|h| h.join(".codesmith").join("extensions"))
            .unwrap_or_else(|| workspace.join(".codesmith").join("extensions")),
        codesmith_extensions::InstallScope::Project => {
            workspace.join(".codesmith").join("extensions")
        }
    }
}

fn install(app: &mut App, arg: &str) -> CommandResult {
    if let Some(err) = install_precheck(arg) {
        return err;
    }
    // Precheck passed → spec is valid + git/path.
    let spec = codesmith_extensions::SourceSpec::parse(arg).expect("precheck validated");
    let root = extensions_root_for(spec.scope, &app.workspace);
    let source: Box<dyn codesmith_extensions::ExtensionSource> = match spec.kind {
        codesmith_extensions::SourceKind::Git => Box::new(
            codesmith_extensions::GitSource::new(spec.body.clone(), spec.ref_.clone()),
        ),
        codesmith_extensions::SourceKind::Path => Box::new(
            codesmith_extensions::LocalPathSource::new(spec.body.clone()),
        ),
        _ => unreachable!("install_precheck rejected crate/prebuilt"),
    };
    let build_target = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => return CommandResult::error(format!("tempdir for build: {e}")),
    };
    let builder = codesmith_extensions::CargoBuilder::new(build_target.path().to_path_buf());
    let installer =
        codesmith_extensions::Installer::new(source.as_ref(), &builder, root.clone());
    let report = match installer.install(&spec) {
        Ok(r) => r,
        Err(e) => return CommandResult::error(format!("install failed: {e}")),
    };
    // Record provenance (tui-side state mutator; R1).
    if let Err(e) = app.extension_state.add_installed(&report.id, &report.provenance) {
        return CommandResult::error(format!("installed but state write failed: {e}"));
    }
    // Trust-warn (R1: install is trust-agnostic; warn if project + untrusted).
    let will_load = match spec.scope {
        codesmith_extensions::InstallScope::Global => true,
        codesmith_extensions::InstallScope::Project => {
            crate::config::is_workspace_trusted(&app.workspace)
        }
    };
    let trust_note = if will_load {
        String::new()
    } else {
        "\n⚠ won't load until the workspace is trusted (accept the trust prompt or /trust, then /extension reload)."
            .to_string()
    };
    CommandResult::message(format!(
        "Installed extension '{}' (v{}) to {}.\nprovenance: {}\nRun /extension reload to load it.{}",
        report.id,
        report.version,
        report.path.display(),
        report.provenance,
        trust_note,
    ))
}

fn uninstall(app: &mut App, arg: &str) -> CommandResult {
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /extension uninstall <id>");
    }
    // Search both roots (state doesn't record scope; locate by convention).
    let project_root = app.workspace.join(".codesmith").join("extensions");
    let mut roots = vec![project_root];
    if let Some(h) = crate::config::effective_home_dir() {
        roots.push(h.join(".codesmith").join("extensions"));
    }
    let report = match codesmith_extensions::Installer::uninstall_files(id, &roots) {
        Ok(r) => r,
        Err(e) => return CommandResult::error(format!("uninstall failed: {e}")),
    };
    if let Err(e) = app.extension_state.remove_installed(id) {
        return CommandResult::error(format!("files removed but state write failed: {e}"));
    }
    if report.removed {
        CommandResult::message(format!(
            "Uninstalled extension '{id}'.\n⚠ tools/commands remain bound until process restart (bounded retention, §F5b Q1); handlers clear on next /extension reload."
        ))
    } else {
        CommandResult::message(format!(
            "No installed extension '{id}' found on disk (state cleared)."
        ))
    }
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
    fn install_precheck_missing_arg_is_usage_error() {
        let r = install_precheck("");
        assert!(r.is_some());
        let r = r.unwrap();
        assert!(r.is_error);
        assert!(r.message.as_deref().unwrap().contains("Usage"));
    }

    #[test]
    fn install_precheck_crate_kind_is_not_yet_implemented() {
        let r = install_precheck("crate:my-ext");
        assert!(r.is_some());
        let r = r.unwrap();
        assert!(r.is_error);
        assert!(
            r.message.as_deref().unwrap().contains("§F5c-later"),
            "got: {:?}",
            r.message
        );
    }

    #[test]
    fn install_precheck_prebuilt_kind_is_not_yet_implemented() {
        let r = install_precheck("prebuilt:https://x/y.dylib");
        assert!(r.is_some());
        assert!(r.unwrap().is_error);
    }

    #[test]
    fn install_precheck_git_path_proceeds_none() {
        assert!(install_precheck("git:github.com/foo/bar").is_none());
        assert!(install_precheck("path:/abs/dir").is_none());
    }
}
