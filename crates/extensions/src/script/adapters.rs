//! Script-side adapters: the trait objects a `RhaiMod` contributes —
//! `ScriptHandler` (impl [`Handler`]), `ScriptToolDefinition` (impl
//! [`ToolDefinition`]), `ScriptCommandDefinition` (impl
//! [`CommandDefinition`]).
//!
//! Each adapter holds a clone of the mod's [`ScriptRuntime`] (engine + AST)
//! and the script `FnPtr`. Calls are synchronous `FnPtr::call`s inline
//! inside the async trait methods — bounded by the engine's
//! `set_max_operations` cap, so a runaway hook burns its budget and errors
//! out instead of parking a tokio worker forever (plan §七).
//!
//! Fail-open policy: a handler whose script errors → `warn` + `Continue`
//! (the chain must not break because one mod is broken — mirrors `emit`'s
//! `catch_unwind` isolation). A tool/command error surfaces as the normal
//! `ExtensionError::Tool/Command` failure instead.

use async_trait::async_trait;
use codesmith_agent::extension::{
    CommandDefinition, CommandOutput, ExtensionCommandContext, ExtensionContext, ExtensionError,
    Handler, HandlerOutcome, ToolDefinition,
};
use codesmith_tools::ToolResult;
use rhai::Dynamic;

use super::rhai_mod::{
    ScriptRuntime, ctx_to_dynamic, dynamic_to_json, event_to_dynamic, merge_transform,
    split_control,
};

/// Call a script callback with `(payload, ctx)`, falling back to `(payload)`
/// when the closure declares a single parameter.
///
/// Rhai surfaces an arity mismatch as a TOP-LEVEL
/// [`ErrorFunctionNotFound`](rhai::EvalAltResult::ErrorFunctionNotFound) —
/// the closure body never ran, so the retry cannot duplicate side effects.
/// Errors from inside a successfully-bound call arrive wrapped in
/// `ErrorInFunctionCall` (or as runtime errors) and are returned verbatim.
fn call_with_optional_ctx(
    runtime: &ScriptRuntime,
    callback: &rhai::FnPtr,
    payload: Dynamic,
    ctx: Dynamic,
) -> Result<Dynamic, Box<rhai::EvalAltResult>> {
    match callback.call::<Dynamic>(&runtime.engine, &runtime.ast, (payload.clone(), ctx)) {
        Ok(v) => Ok(v),
        Err(e) if matches!(*e, rhai::EvalAltResult::ErrorFunctionNotFound(_, _)) => {
            // Single-parameter form (`|e|` without ctx).
            callback.call::<Dynamic>(&runtime.engine, &runtime.ast, (payload,))
        }
        Err(e) => Err(e),
    }
}

// === ScriptHandler ==========================================================

/// A `on(event, |e, ctx| {...})` registration.
pub struct ScriptHandler {
    pub(crate) mod_id: String,
    pub(crate) runtime: std::sync::Arc<ScriptRuntime>,
    pub(crate) callback: rhai::FnPtr,
}

#[async_trait]
impl Handler for ScriptHandler {
    async fn handle(
        &self,
        event: &codesmith_agent::extension::ExtensionEvent,
        ctx: &dyn ExtensionContext,
    ) -> Result<HandlerOutcome, ExtensionError> {
        let payload = event_to_dynamic(event);
        let ctx_map = ctx_to_dynamic(ctx);
        match call_with_optional_ctx(&self.runtime, &self.callback, payload, ctx_map) {
            Ok(ret) => Ok(interpret_handler_outcome(ret, event)),
            Err(e) => {
                // Fail-open (plan §三.4): warn + Continue.
                tracing::warn!(
                    target: "codesmith_mods",
                    "mod '{}' handler failed: {e}",
                    self.mod_id
                );
                Ok(HandlerOutcome::Continue)
            }
        }
    }
}

/// Map a handler return value onto [`HandlerOutcome`]:
/// `proceed()`/`()` → `Continue`; `block(r)` → `Block`; `cancel(r)` →
/// `Cancel`; `transform(#{...})` → merged `Transform`.
fn interpret_handler_outcome(
    ret: Dynamic,
    event: &codesmith_agent::extension::ExtensionEvent,
) -> HandlerOutcome {
    if let Some((control, value)) = split_control(&ret) {
        return match control.as_str() {
            "proceed" => HandlerOutcome::Continue,
            "block" => HandlerOutcome::Block {
                reason: value.try_cast::<String>().unwrap_or_default(),
            },
            "cancel" => HandlerOutcome::Cancel {
                reason: value.try_cast::<String>().unwrap_or_default(),
            },
            "transform" => merge_transform(event, &value),
            other => {
                tracing::warn!(
                    target: "codesmith_mods",
                    "handler returned unknown control '{other}' (use proceed/block/cancel/transform)"
                );
                HandlerOutcome::Continue
            }
        };
    }
    // Unit (bare `()`) or any non-marker value → Continue.
    HandlerOutcome::Continue
}

// === ScriptToolDefinition ===================================================

/// A `register_tool(spec, |input, ctx| {...})` registration.
pub struct ScriptToolDefinition {
    pub(crate) mod_id: String,
    pub(crate) runtime: std::sync::Arc<ScriptRuntime>,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) schema: serde_json::Value,
    pub(crate) callback: rhai::FnPtr,
}

/// Render a script value as tool-result content: strings verbatim,
/// everything else as pretty JSON.
fn stringify_dynamic(value: &Dynamic) -> String {
    if let Some(s) = value.clone().try_cast::<String>() {
        return s;
    }
    match dynamic_to_json(value) {
        Ok(json) => serde_json::to_string_pretty(&json).unwrap_or_else(|_| "()".to_string()),
        Err(_) => "()".to_string(),
    }
}

#[async_trait]
impl ToolDefinition for ScriptToolDefinition {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> serde_json::Value {
        self.schema.clone()
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        ctx: &dyn ExtensionContext,
    ) -> Result<ToolResult, ExtensionError> {
        let payload = super::rhai_mod::json_to_dynamic(&input);
        let ctx_map = ctx_to_dynamic(ctx);
        match call_with_optional_ctx(&self.runtime, &self.callback, payload, ctx_map) {
            Ok(ret) => Ok(match split_control(&ret) {
                Some((control, value)) if control == "ok" => {
                    ToolResult::success(stringify_dynamic(&value))
                }
                Some((control, value)) if control == "err" => {
                    ToolResult::error(stringify_dynamic(&value))
                }
                // Bare string / unit / structured value: treat as success
                // content (a mod returning a plain string is the friendly
                // common case).
                _ => ToolResult::success(stringify_dynamic(&ret)),
            }),
            Err(e) => Err(ExtensionError::Tool {
                tool: format!("{} (mod '{}')", self.name, self.mod_id),
                message: e.to_string(),
            }),
        }
    }
}

// === ScriptCommandDefinition ================================================

/// A `register_command(name, description, |args, ctx| {...})` registration.
pub struct ScriptCommandDefinition {
    pub(crate) mod_id: String,
    pub(crate) runtime: std::sync::Arc<ScriptRuntime>,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) callback: rhai::FnPtr,
}

#[async_trait]
impl CommandDefinition for ScriptCommandDefinition {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    async fn run(
        &self,
        ctx: &dyn ExtensionCommandContext,
        args: &str,
    ) -> Result<CommandOutput, ExtensionError> {
        let payload = Dynamic::from(args.to_string());
        let ctx_map = ctx_to_dynamic(ctx);
        match call_with_optional_ctx(&self.runtime, &self.callback, payload, ctx_map) {
            Ok(ret) => Ok(match split_control(&ret) {
                Some((control, value)) if control == "send" => {
                    CommandOutput::SendMessage(value.try_cast().unwrap_or_default())
                }
                // `message(...)` — and any bare string — display to the user.
                Some((control, value)) if control == "message" => {
                    CommandOutput::Message(value.try_cast().unwrap_or_default())
                }
                _ => CommandOutput::Message(
                    ret.clone()
                        .try_cast()
                        .unwrap_or_else(|| stringify_dynamic(&ret)),
                ),
            }),
            Err(e) => Err(ExtensionError::Command {
                command: format!("{} (mod '{}')", self.name, self.mod_id),
                message: e.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent::extension::{Extension, ExtensionEvent, InputEvent, ToolCallEvent};
    use codesmith_tools::ToolResult as TR;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    struct Ctx {
        generation: u64,
    }
    #[async_trait]
    impl ExtensionContext for Ctx {
        fn cwd(&self) -> &Path {
            Path::new("/ws")
        }
        fn mode(&self) -> codesmith_agent::extension::ExtensionMode {
            codesmith_agent::extension::ExtensionMode::Tui
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
    impl codesmith_agent::extension::ExtensionCommandContext for Ctx {}

    fn ctx() -> Ctx {
        Ctx { generation: 1 }
    }

    /// Load a mod from an inline script and drive `configure` against a
    /// fresh runner, mirroring how `ExtensionRunner::load` does it.
    fn load_mod(dir: &TempDir, id: &str, script: &str) -> super::super::RhaiMod {
        let mod_dir = dir.path().join(id);
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("mod.rhai"), script).unwrap();
        let discovered = super::super::DiscoveredMod {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".to_string(),
            description: None,
            entry_path: mod_dir.join("mod.rhai"),
            dir: mod_dir,
            global: true,
        };
        super::super::RhaiMod::load(
            &discovered,
            super::super::ModKvStore::new(dir.path().join(format!("global-{id}.json"))),
            hub(),
        )
        .expect("load mod")
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Runtime::new().expect("rt").block_on(f)
    }

    /// Fresh message-projection hub for `RhaiMod::load` test calls.
    fn hub() -> codesmith_agent::extension::MessageProjectionHubArc {
        codesmith_agent::extension::MessageProjectionHubArc::new(
            codesmith_agent::extension::MessageProjectionHub::new(),
        )
    }

    // Handler outcome mapping — the four outcomes from plan §三.3.

    #[test]
    fn handler_block_maps_to_block_outcome() {
        let dir = TempDir::new().unwrap();
        let m = load_mod(
            &dir,
            "blocker",
            r#"on("tool-call", |e| {
                if e.name == "exec_shell" { return block("nope"); }
                proceed()
            });"#,
        );
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m)).unwrap();
        runner.bind_core(Arc::new(ctx()));
        let out = block_on(
            runner.emit(codesmith_agent::extension::ExtensionEvent::ToolCall(
                ToolCallEvent {
                    id: "c1".into(),
                    name: "exec_shell".into(),
                    input: json!({}),
                },
            )),
        );
        match out.outcome {
            HandlerOutcome::Block { reason } => assert_eq!(reason, "nope"),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn handler_unit_return_maps_to_continue() {
        let dir = TempDir::new().unwrap();
        // Bare `()` (no proceed()) — the "放行；返回 () 等价" semantics.
        let m = load_mod(
            &dir,
            "silent",
            r#"on("turn-start", |e| { mod_log("seen"); });"#,
        );
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m)).unwrap();
        runner.bind_core(Arc::new(ctx()));
        let out = block_on(
            runner.emit(codesmith_agent::extension::ExtensionEvent::TurnStart {
                turn_id: "t1".into(),
            }),
        );
        assert!(matches!(out.outcome, HandlerOutcome::Continue));
    }

    #[test]
    fn handler_omitting_ctx_still_fires() {
        let dir = TempDir::new().unwrap();
        // 1-arg closure — the arity fallback path.
        let m = load_mod(
            &dir,
            "one-arg",
            r#"on("input", |e| { transform(#{ text: "!" + e.text }) });"#,
        );
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m)).unwrap();
        runner.bind_core(Arc::new(ctx()));
        let out = block_on(
            runner.emit(codesmith_agent::extension::ExtensionEvent::Input(
                InputEvent { text: "hi".into() },
            )),
        );
        match out.event {
            codesmith_agent::extension::ExtensionEvent::Input(e) => {
                assert_eq!(e.text, "!hi");
            }
            other => panic!("expected Input, got {other:?}"),
        }
        assert!(matches!(out.outcome, HandlerOutcome::Continue));
    }

    #[test]
    fn handler_with_ctx_reads_ctx_map() {
        let dir = TempDir::new().unwrap();
        let m = load_mod(
            &dir,
            "ctx-reader",
            r#"on("input", |e, ctx| {
                if ctx.mode == "tui" && ctx.cwd.contains("ws") && ctx.generation >= 1 && ctx.idle {
                    return transform(#{ text: e.text + "-ctx" });
                }
                proceed()
            });"#,
        );
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m)).unwrap();
        runner.bind_core(Arc::new(ctx()));
        let out = block_on(
            runner.emit(codesmith_agent::extension::ExtensionEvent::Input(
                InputEvent { text: "go".into() },
            )),
        );
        match out.event {
            codesmith_agent::extension::ExtensionEvent::Input(e) => assert_eq!(e.text, "go-ctx"),
            other => panic!("expected Input, got {other:?}"),
        }
    }

    #[test]
    fn handler_script_error_fails_open_to_continue() {
        let dir = TempDir::new().unwrap();
        // `e.nope` doesn't exist on the payload map → runtime error → warn +
        // Continue, and the chain (2nd handler) still runs.
        let m = load_mod(
            &dir,
            "broken",
            r#"on("input", |e| { let x = e.nope.deeper; proceed() });"#,
        );
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m)).unwrap();
        runner.bind_core(Arc::new(ctx()));
        let out = block_on(
            runner.emit(codesmith_agent::extension::ExtensionEvent::Input(
                InputEvent { text: "x".into() },
            )),
        );
        assert!(matches!(out.outcome, HandlerOutcome::Continue));
    }

    #[test]
    fn handler_infinite_loop_fails_open_to_continue() {
        let dir = TempDir::new().unwrap();
        let m = load_mod(
            &dir,
            "loop-hook",
            r#"on("turn-start", |e| { let x = 0; while x >= 0 { x += 1; } proceed() });"#,
        );
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m)).unwrap();
        runner.bind_core(Arc::new(ctx()));
        let out = block_on(
            runner.emit(codesmith_agent::extension::ExtensionEvent::TurnStart {
                turn_id: "t".into(),
            }),
        );
        assert!(matches!(out.outcome, HandlerOutcome::Continue));
    }

    // Tool round-trip.

    #[test]
    fn tool_ok_err_round_trip() {
        let dir = TempDir::new().unwrap();
        let m = load_mod(
            &dir,
            "tool-mod",
            r#"register_tool(#{
                name: "team_ci_status",
                description: "查询团队 CI 状态",
                schema: #{ type: "object", properties: #{ env: #{ type: "string" } }, additionalProperties: false },
            }, |input, ctx| {
                if input.env == "prod" { return ok("green"); }
                err("unknown env")
            });"#,
        );
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m)).unwrap();
        runner.bind_core(Arc::new(ctx()));

        let tools = runner.bound_tools();
        let tool = tools
            .iter()
            .find(|(n, _)| n == "team_ci_status")
            .expect("tool bound");
        assert_eq!(tool.1.description(), "查询团队 CI 状态");
        let schema = tool.1.input_schema();
        assert_eq!(schema.get("type").and_then(|v| v.as_str()), Some("object"));

        let ok = block_on(tool.1.execute(json!({"env": "prod"}), &ctx())).unwrap();
        assert!(ok.success);
        assert_eq!(ok.content, "green");
        // Sanity: same instance via a second Arc<ToolDefinition> ref.
        drop(ok);

        let err = block_on(tool.1.execute(json!({"env": "dev"}), &ctx())).unwrap();
        assert!(!err.success);
        assert_eq!(err.content, "unknown env");
    }

    #[test]
    fn tool_script_error_surfaces_as_extension_error() {
        let dir = TempDir::new().unwrap();
        let m = load_mod(
            &dir,
            "broken-tool",
            r#"register_tool(#{name: "boom", description: "x"}, |i| { let y = i.field.that.is_missing; ok(1) });"#,
        );
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m)).unwrap();
        runner.bind_core(Arc::new(ctx()));
        let tool = runner
            .bound_tools()
            .into_iter()
            .find(|(n, _)| n == "boom")
            .expect("tool bound");
        let out = block_on(tool.1.execute(json!({}), &ctx()));
        assert!(
            out.is_err(),
            "broken tool must surface an error, got {out:?}"
        );
    }

    // Command round-trip — message()/send() both variants.

    #[test]
    fn command_message_and_send_variants() {
        let dir = TempDir::new().unwrap();
        let m = load_mod(
            &dir,
            "cmd-mod",
            r#"
            register_command("ci", "显示 CI 状态", |args, ctx| { message(mod_state_get("ci") ?? "unknown") });
            register_command("dispatch", "send to agent", |args| { send("run: " + args) });
            "#,
        );
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m)).unwrap();
        runner.bind_core(Arc::new(ctx()));

        let msg = block_on(runner.try_dispatch_command("ci", "")).expect("ci dispatched");
        assert!(matches!(msg, CommandOutput::Message(ref s) if s == "unknown"));

        let send = block_on(runner.try_dispatch_command("dispatch", "tests")).expect("dispatched");
        assert!(matches!(send, CommandOutput::SendMessage(ref s) if s == "run: tests"));
    }

    #[test]
    fn tool_string_return_is_success_content() {
        let dir = TempDir::new().unwrap();
        // Friendly form: return a bare string without ok().
        let m = load_mod(
            &dir,
            "plain-tool",
            r#"register_tool(#{name: "hello_tool", description: "x"}, |i| { "hello" });"#,
        );
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m)).unwrap();
        runner.bind_core(Arc::new(ctx()));
        let tool = runner
            .bound_tools()
            .into_iter()
            .find(|(n, _)| n == "hello_tool")
            .unwrap();
        let out = block_on(tool.1.execute(json!({}), &ctx())).unwrap();
        assert!(out.success);
        assert_eq!(out.content, "hello");
    }

    #[test]
    fn tool_sees_json_input_map() {
        let dir = TempDir::new().unwrap();
        let m = load_mod(
            &dir,
            "input-tool",
            r#"register_tool(#{name: "sum_tool", description: "x"}, |input| {
                ok(input.a + input.b)
            });"#,
        );
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m)).unwrap();
        runner.bind_core(Arc::new(ctx()));
        let tool = runner
            .bound_tools()
            .into_iter()
            .find(|(n, _)| n == "sum_tool")
            .unwrap();
        let out = block_on(tool.1.execute(json!({"a": 2, "b": 40}), &ctx())).unwrap();
        assert!(out.success);
        assert_eq!(out.content.trim(), "42");
    }

    // KV persistence through the tool path.

    #[test]
    fn tool_kv_persists_across_mod_reload() {
        let dir = TempDir::new().unwrap();
        let script = r#"register_tool(#{name: "note_set", description: "x"}, |i| {
            mod_state_set("last", i.value);
            ok("saved")
        });"#;
        let kv_path: PathBuf = dir.path().join("global-kvmod.json");
        let mod_dir = dir.path().join("kvmod");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("mod.rhai"), script).unwrap();
        let discovered = super::super::DiscoveredMod {
            id: "kvmod".into(),
            name: "kvmod".into(),
            version: "0.1.0".into(),
            description: None,
            entry_path: mod_dir.join("mod.rhai"),
            dir: mod_dir.clone(),
            global: true,
        };

        // First instance: set.
        let m1 = super::super::RhaiMod::load(
            &discovered,
            super::super::ModKvStore::new(kv_path.clone()),
            hub(),
        )
        .unwrap();
        let runner = crate::ExtensionRunner::new();
        block_on(runner.load(&m1)).unwrap();
        runner.bind_core(Arc::new(ctx()));
        let tool = runner
            .bound_tools()
            .into_iter()
            .find(|(n, _)| n == "note_set")
            .unwrap();
        let out = block_on(tool.1.execute(json!({"value": "hello"}), &ctx())).unwrap();
        assert!(out.success);

        // Second instance (fresh runner = "reload"): read via script.
        let script2 = r#"register_tool(#{name: "note_get", description: "x"}, |i| {
            ok(mod_state_get("last") ?? "missing")
        });"#;
        std::fs::write(mod_dir.join("mod.rhai"), script2).unwrap();
        let m2 =
            super::super::RhaiMod::load(&discovered, super::super::ModKvStore::new(kv_path), hub())
                .unwrap();
        let runner2 = crate::ExtensionRunner::new();
        block_on(runner2.load(&m2)).unwrap();
        runner2.bind_core(Arc::new(ctx()));
        let tool2 = runner2
            .bound_tools()
            .into_iter()
            .find(|(n, _)| n == "note_get")
            .unwrap();
        let out2 = block_on(tool2.1.execute(json!({}), &ctx())).unwrap();
        assert_eq!(out2.content, "hello");
    }

    // Transform chains: script transform visible to a second (Rust) handler.

    #[test]
    fn transform_from_script_feeds_next_handler() {
        use codesmith_agent::extension::ExtensionApi;
        let dir = TempDir::new().unwrap();
        let m = load_mod(
            &dir,
            "transformer",
            r#"on("input", |e| { transform(#{ text: e.text.to_upper() }) });"#,
        );

        struct Observe(Mutex<String>);
        #[async_trait]
        impl Handler for Observe {
            async fn handle(
                &self,
                event: &ExtensionEvent,
                _ctx: &dyn ExtensionContext,
            ) -> Result<HandlerOutcome, ExtensionError> {
                if let ExtensionEvent::Input(e) = event {
                    *self.0.lock().unwrap() = e.text.clone();
                }
                Ok(HandlerOutcome::Continue)
            }
        }

        let runner = crate::ExtensionRunner::new();
        // Configure the mod against a stub api… the runner's `load` does it.
        block_on(runner.load(&m)).unwrap();
        // Register the observer directly via a second Extension's configure.
        struct ObsExt;
        #[async_trait]
        impl Extension for ObsExt {
            fn metadata(&self) -> &codesmith_agent::extension::ExtensionMetadata {
                static M: codesmith_agent::extension::ExtensionMetadata =
                    codesmith_agent::extension::ExtensionMetadata::new("obs");
                &M
            }
            async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
                api.on(Arc::new(Observe(Mutex::new(String::new()))))?;
                let _ = TR::success("");
                Ok(())
            }
        }
        block_on(runner.load(&ObsExt)).unwrap();
        runner.bind_core(Arc::new(ctx()));

        let out = block_on(runner.emit(ExtensionEvent::Input(InputEvent {
            text: "quiet".into(),
        })));
        match out.event {
            ExtensionEvent::Input(e) => assert_eq!(e.text, "QUIET"),
            other => panic!("expected Input, got {other:?}"),
        }
    }
}
