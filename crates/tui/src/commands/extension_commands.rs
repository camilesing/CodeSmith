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

use crate::tui::app::{App, AppAction};

use super::CommandResult;
use codesmith_agent::extension::CommandOutput;

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

/// §F5d T2 — dispatch an extension-registered slash command (e.g. `/mycmd
/// args`) by calling [`ExtensionRunner::try_dispatch_command`].
///
/// `try_dispatch_command` is async (`CommandDefinition::run` is async) but
/// `commands::execute` is sync. Both production call sites of `execute`
/// (`tui/ui.rs:3763` + `tui/ui.rs:5901`) live inside `async fn`s running on
/// the TUI's multi-thread `#[tokio::main]` runtime → creating+dropping a
/// tokio runtime on this thread would panic on shutdown (tokio
/// blocking/shutdown.rs; the same lesson `populate_extension_runtime`
/// records at `core/engine.rs:418-426`). Mirror that pattern: spawn a plain
/// OS thread that owns the current-thread rt's lifetime + block via
/// `std::thread::scope`.
///
/// Returns `None` when no runner is bound or no command matches `name`, so
/// `execute` falls through to the static-match tier + built-in commands.
/// `CommandOutput` → `CommandResult`: `Message(s)`→display;
/// `SendMessage(s)`→agent send (mirrors `user_commands.rs:222`).
pub fn try_dispatch_extension_command(app: &App, name: &str, args: &str) -> Option<CommandResult> {
    let runner = app.extension_runner.clone()?;
    let out = std::thread::scope(|s| {
        s.spawn(move || -> Option<CommandOutput> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("extension command dispatch runtime");
            rt.block_on(runner.try_dispatch_command(name, args))
        })
        .join()
        .expect("extension command dispatch thread panicked")
    })?;
    Some(match out {
        CommandOutput::Message(s) => CommandResult::message(s),
        CommandOutput::SendMessage(s) => CommandResult::action(AppAction::SendMessage(s)),
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

    // === §F5d T2 — extension-registered slash command dispatch ============
    // The fixture dylib registers only a tool + handler (no command), so the
    // T2 dispatch test uses an in-process `CmdExt` that registers a static
    // `EchoCmd` (mirror of the fixture's tool-registration shape, but for
    // commands). The load+bind round-trip mirrors installer.rs:224-228.

    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use async_trait::async_trait;
    use codesmith_agent::extension::{
        CommandDefinition, CommandOutput, Extension, ExtensionApi, ExtensionCommandContext,
        ExtensionContext, ExtensionError, ExtensionMetadata, ExtensionMode,
    };
    use codesmith_extensions::ExtensionRunner;
    use tokio_util::sync::CancellationToken;
    use crate::config::Config;
    use crate::tui::app::TuiOptions;

    /// Minimal host context (mirrors the `Ctx` in `runner.rs` tests): impls
    /// `ExtensionContext` + the marker `ExtensionCommandContext` sub-trait so
    /// `bind_core` accepts it as `Arc<dyn ExtensionCommandContext>`.
    struct Ctx {
        generation: u64,
    }
    #[async_trait]
    impl ExtensionContext for Ctx {
        fn cwd(&self) -> &Path {
            Path::new(".")
        }
        fn mode(&self) -> ExtensionMode {
            ExtensionMode::Tui
        }
        fn is_idle(&self) -> bool {
            true
        }
        fn signal(&self) -> CancellationToken {
            CancellationToken::new()
        }
        fn generation(&self) -> u64 {
            self.generation
        }
    }
    impl ExtensionCommandContext for Ctx {}

    /// A contributed slash command: echoes its args back as a `Message`.
    /// Registered under name `fixture_cmd`.
    struct EchoCmd;
    #[async_trait]
    impl CommandDefinition for EchoCmd {
        fn name(&self) -> &str {
            "fixture_cmd"
        }
        fn description(&self) -> &str {
            "Echoes args back (T2 dispatch test)."
        }
        async fn run(
            &self,
            _ctx: &dyn ExtensionCommandContext,
            args: &str,
        ) -> Result<CommandOutput, ExtensionError> {
            Ok(CommandOutput::Message(format!("echo:{args}")))
        }
    }

    /// A contributed slash command returning `SendMessage` (the agent-send
    /// variant) — covers the `CommandOutput::SendMessage →
    /// CommandResult::action(AppAction::SendMessage)` arm of
    /// `try_dispatch_extension_command` (the `Message`-arm test below does
    /// not exercise it). Registered under `send_cmd`.
    struct SendCmd;
    #[async_trait]
    impl CommandDefinition for SendCmd {
        fn name(&self) -> &str {
            "send_cmd"
        }
        fn description(&self) -> &str {
            "Returns SendMessage (T2 SendMessage-arm test)."
        }
        async fn run(
            &self,
            _ctx: &dyn ExtensionCommandContext,
            args: &str,
        ) -> Result<CommandOutput, ExtensionError> {
            Ok(CommandOutput::SendMessage(format!("send:{args}")))
        }
    }

    /// Extension factory that registers `EchoCmd` + `SendCmd` via
    /// `api.register_command`. Mirrors the fixture dylib's `configure` (which
    /// registers a tool) but contributes commands instead — the symmetric T2
    /// fixture. `EchoCmd` exercises the `Message` arm; `SendCmd` the
    /// `SendMessage` arm.
    struct CmdExt;
    #[async_trait]
    impl Extension for CmdExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("cmd-ext");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.register_command(Box::new(EchoCmd))?;
            api.register_command(Box::new(SendCmd))?;
            Ok(())
        }
    }

    /// Minimal `App` mirroring `create_test_app` (`commands/mod.rs` tests).
    /// `App::new` leaves `extension_runner = None` — the base for both the
    /// no-runner fall-through test + `test_app_with_runner`.
    fn test_app() -> App {
        let options = TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace: PathBuf::from("."),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            use_alt_screen: true,
            use_mouse_capture: false,
            use_bracketed_paste: true,
            max_subagents: 1,
            skills_dir: PathBuf::from("."),
            memory_path: PathBuf::from("memory.md"),
            notes_path: PathBuf::from("notes.txt"),
            mcp_config_path: PathBuf::from("mcp.json"),
            use_memory: false,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        App::new(options, &Config::default())
    }

    /// `test_app()` with `extension_runner` bound — for dispatch tests that
    /// need a contributed command loaded. The helper's
    /// `app.extension_runner.clone()?` resolves only when this is set.
    fn test_app_with_runner(runner: Arc<ExtensionRunner>) -> App {
        let mut app = test_app();
        app.extension_runner = Some(runner);
        app
    }

    #[test]
    fn try_dispatch_extension_command_resolves_contributed_command() {
        // Load + bind a contributed command (mirror installer.rs:224-228
        // round-trip). `try_dispatch_command` is async; the dispatch helper
        // drives it on a spawned current-thread tokio rt (production
        // `execute()` runs on a tokio worker thread → can't create+drop a
        // runtime in-place; the helper mirrors `populate_extension_runtime`'s
        // `thread::scope` form).
        let runner = Arc::new(ExtensionRunner::new());
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&CmdExt)).expect("load CmdExt");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let app = test_app_with_runner(runner);

        let res = try_dispatch_extension_command(&app, "fixture_cmd", "hello");
        assert!(res.is_some(), "contributed command dispatched");
        let res = res.unwrap();
        assert!(!res.is_error, "command succeeded");
        assert!(
            res.message.as_deref().unwrap().contains("echo:hello"),
            "arg forwarded into command output: {:?}",
            res.message
        );
    }

    #[test]
    fn try_dispatch_extension_command_returns_none_when_no_runner() {
        // `App::new` leaves extension_runner = None → the helper's
        // `app.extension_runner.clone()?` short-circuits to None, so built-in
        // slash commands still run when no extension runner is bound.
        let app = test_app();
        assert!(
            try_dispatch_extension_command(&app, "fixture_cmd", "").is_none(),
            "no runner → None (fall through to built-ins)"
        );
    }

    #[test]
    fn try_dispatch_extension_command_returns_none_for_unknown_command() {
        // A bound runner with no matching command → try_dispatch_command
        // returns None → helper returns None → built-ins still run. Locks the
        // fall-through contract every built-in slash command depends on.
        let runner = Arc::new(ExtensionRunner::new());
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&CmdExt)).expect("load CmdExt");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let app = test_app_with_runner(runner);
        assert!(
            try_dispatch_extension_command(&app, "definitely_not_a_command", "").is_none(),
            "unknown command → None (fall through to built-ins)"
        );
    }

    #[test]
    fn try_dispatch_extension_command_maps_send_message_to_action() {
        // Covers the `CommandOutput::SendMessage(s) => CommandResult::action(
        // AppAction::SendMessage(s))` arm (mirrors user_commands.rs:222) —
        // the Message-arm test above does not exercise this branch.
        let runner = Arc::new(ExtensionRunner::new());
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&CmdExt)).expect("load CmdExt");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let app = test_app_with_runner(runner);

        let res = try_dispatch_extension_command(&app, "send_cmd", "ping");
        assert!(res.is_some(), "send_cmd dispatched");
        let res = res.unwrap();
        assert!(!res.is_error, "command succeeded");
        assert!(res.message.is_none(), "SendMessage maps to action, not message");
        match res.action {
            Some(AppAction::SendMessage(s)) => {
                assert_eq!(s.as_str(), "send:ping", "arg forwarded into SendMessage action");
            }
            other => panic!("expected AppAction::SendMessage, got {other:?}"),
        }
    }
}
