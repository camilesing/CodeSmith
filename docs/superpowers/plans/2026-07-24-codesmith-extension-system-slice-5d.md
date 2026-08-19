# §F5d — Extension tool/command host wiring + true unload — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans`. Steps use `- [ ]` checkbox syntax; complete each in order. **Read the approved spec first:** `docs/superpowers/specs/2026-07-24-codesmith-extension-system-slice-5d-design.md` (§2 code findings are VERIFIED exact on `main` HEAD `2eba6e9c`; do not re-verify unless a step says so).

## Goal

Wire extension-contributed **tools + slash commands** into the host per-turn tool set so they are actually callable (today both are dead production code — `ExtensionToolSpecAdapter` is constructed only in tests, `bound_tools()` is display-only, `try_dispatch_command` is defined-but-never-called). Add safe **true unload**: reload-deferred clear of `tools`/`commands`/`handlers` + two-phase engine-side `Library` drop — with **no UB** (no dangling dylib `Arc` on the main thread at drop time). Subagents never hold dylib `Arc`s (structurally automatic — verified; a regression test locks it).

## Architecture

1. **Wiring (per-turn, main-thread, fail-closed).** A `register_extension_tools(&mut ToolRegistry, &ExtensionRunner)` helper (in `agent-runtime/tools/extension.rs`, next to `ExtensionToolSpecAdapter`) reads `runner.bound_tools()` + `runner.bound_context()` and registers an `ExtensionToolSpecAdapter` per tool through `ToolRegistry::register` (the existing fail-closed chokepoint). Called from `EngineHost::build_turn_dispatcher` **after** plugin-tools are configured (:440) and **before** the catalog is built (:444). Per-turn rebuild → no persistent host holder; clearing `runner.tools[id]` before the next turn's call is sufficient. A new `ExtensionRunner::bound_context()` accessor upcasts the stored `Arc<dyn ExtensionCommandContext>` → `Arc<dyn ExtensionContext>` (Rust 1.86+ trait-upcasting) for the adapter's `ctx`.
2. **Command wiring.** `ExtensionRunner::try_dispatch_command(name, args)` (runner.rs:294) is wired into `commands::execute` (mod.rs:573) as a new tier after the `/extension` meta-tier (:585) and before the static match (:657). It is async; `execute` is sync → use a current-thread tokio runtime (`block_on`) mirroring `populate_extension_runtime` (engine.rs:431). `CommandOutput` → `CommandResult`: `Message(s)`→`::message(s)`; `SendMessage(s)`→`::action(AppAction::SendMessage(s))` (mirrors `user_commands.rs:222`).
3. **Unload (reload-deferred).** Add `clear_tools()`/`clear_commands()` on `ExtensionRunner` (mirror `clear_handlers` runner.rs:156). Call them from `reload_extension_runtime` (engine.rs:484) in the order: `clear_handlers` → `clear_tools` → `clear_commands` → `drain_libraries_to_pending` → `invalidate` → `populate`. Safe to run concurrently with an in-flight engine turn: `tools`/`commands`/`handlers` are refcounted (`Arc`)/name-keyed and the `Library` stays alive until the engine drops it (§4a).
4. **Two-phase `Library` drop (§0 decision 3, §4b).** A new `pending_drop: Mutex<Vec<Library>>` field + `drain_libraries_to_pending()` (UI-thread: MOVES the `libraries` `Vec` into `pending_drop` under one lock each — a safe `Arc`-move, the `Library` stays alive) + `drop_pending()` (engine op-loop-top: `mem::take` of `pending_drop` and drop it, at the one moment the main-thread `HostAgentExecutor` is already dropped between turns). Inserted at the op-loop top (mod.rs:517) before `match op`. **Subagent exclusion (§4b): RESOLVED — structurally automatic.** `SubAgentToolRegistry::new` (subagent/mod.rs:6095) rebuilds its OWN fresh `ToolRegistry` from built-ins ("Build the full agent surface — same as the parent's Agent mode", :6103) and `SubAgentRuntime` has **no `extension_runner` field** (mod.rs:580-608; only `HostAgentExecutor` binds one, host_executor.rs:1838). So ext tools (added only in `build_turn_dispatcher`) never reach a subagent's set — no provenance marker / force-subset / runtime guard needed. T4 §4b = a regression test (expected Green) + a doc-comment locking the invariant; no impl.

## Tech Stack

Rust **1.90.0** / edition **2024** (toolchain default; use plain `cargo` — NOT `cargo +1.90.0`). `libloading` (existing, in `ExtensionRunner::libraries`). `tokio` current-thread rt (existing pattern). `codesmith_agent::extension` traits (`ExtensionContext`, `ExtensionCommandContext` sub-trait, `ToolDefinition`, `CommandOutput`, `Extension`/`ExtensionApi`). No new dependencies.

## Baseline (main `2eba6e9c`, 0 ahead / 0 behind)

| crate | command | expected |
|---|---|---|
| extensions | `cargo test -p codesmith_extensions` | 48 pass |
| agent | `cargo test -p codesmith_agent` | 98 pass |
| agent-runtime | `cargo test -p codesmith_agent_runtime` | 1163 pass + 2 ignored |
| tui | `cargo test -p codesmith_tui` | 2835 pass + **26 PRE-EXISTING `runtime_api` env-fail** + 2 ignored |

**Reporting rules (operational constraints):**
- tui's 26 `runtime_api::tests` failures are PRE-EXISTING + environmental (HTTP-server won't bind; no panic) — NOT a regression of any slice. **Never** call tui "green"; always report `tui N pass / 26 pre-existing runtime_api fail / 2 ignored`. Never attribute them to new work.
- `agent-runtime ... streamable_http_stale_session_reconnects_and_retries_tool_call` is occasionally flaky — isolate-rerun to green before judging a regression.
- Plain `cargo` only.

## Branch + spec

- This plan is **untracked on `main`** (specs/plans are untracked per project convention). The approved spec is at `docs/superpowers/specs/2026-07-24-codesmith-extension-system-slice-5d-design.md`.
- **Before any code:** `git checkout -b feat/ext-wire-unload` (operational constraint: branch first on non-trivial work).

## File structure

| file | change |
|---|---|
| `crates/extensions/src/runner.rs` | ADD `bound_context()`, `clear_tools()`, `clear_commands()`, `pending_drop` field, `drain_libraries_to_pending()`, `drop_pending()` |
| `crates/agent-runtime/src/tools/extension.rs` | ADD `register_extension_tools(&mut ToolRegistry, &ExtensionRunner)` helper |
| `crates/tui/src/core/engine/runtime_traits.rs` | CALL `register_extension_tools` in `build_turn_dispatcher` after :440, before :444 |
| `crates/tui/src/commands/mod.rs` | WIRE `try_dispatch_command` into `execute()` after :585, before :657 |
| `crates/tui/src/commands/extension_commands.rs` | ADD `try_dispatch_extension_command` helper; REWRITE `uninstall` message (:317) |
| `crates/tui/src/core/engine.rs` | `reload_extension_runtime` (:484): add clear_tools/clear_commands/drain before populate |
| `crates/agent-runtime/src/engine/mod.rs` | op-loop-top (:517): `runner.drop_pending()` before `match op` |
| `crates/tui/src/tools/subagent/mod.rs` | DOC-COMMENT on `SubAgentToolRegistry::new` (:6095) stating the §4b invariant |
| `docs/EXTENSIONS.md`, `ROADMAP.md` | T5 docs |

---

## Task 1 — Wire ext tools: `bound_context()` + `register_extension_tools` helper + `build_turn_dispatcher` call

**Overview.** Make a registered ext tool (`fixture_echo`) callable through the host `ToolSpec` path. Three pieces: (a) `bound_context()` on `ExtensionRunner`; (b) `register_extension_tools` helper in `agent-runtime/tools/extension.rs`; (c) the call site in `build_turn_dispatcher`.

### Step 1.1 — Red: helper test (agent-runtime)

- [ ] Open `crates/agent-runtime/src/tools/extension.rs`. In the `#[cfg(test)] mod tests` block (existing; has `EchoTool` + `ctx()`), add a **static test extension that registers a tool** + a test for the helper. First confirm the existing test-module imports (`use super::*; use codesmith_agent::extension::{...}`) — extend the import list with `Extension`, `ExtensionApi`, `ExtensionMetadata`, `ExtensionError`, `ExtensionCommandContext` if missing.

```rust
// add to the test-module imports as needed:
use codesmith_agent::extension::{
    Extension, ExtensionApi, ExtensionCommandContext, ExtensionContext, ExtensionError,
    ExtensionMetadata, ExtensionMode,
};
use codesmith_extensions::ExtensionRunner; // HostExtensionContext NOT needed — ctx_cmd() uses a local mock
use super::registry::ToolRegistry;

/// A static (in-process) extension that registers `EchoTool` (defined above
/// in this test module) so T1's helper test does not require a built dylib.
struct ToolExt;
#[async_trait::async_trait]
impl Extension for ToolExt {
    fn metadata(&self) -> &ExtensionMetadata {
        static M: ExtensionMetadata = ExtensionMetadata::new("toolext");
        &M
    }
    async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
        api.register_tool(Box::new(EchoTool))?;
        Ok(())
    }
}

/// `ExtensionCommandContext`-typed ctx for `bind_core` (the runner stores
/// `Arc<dyn ExtensionCommandContext>`; `bound_context()` upcasts it back to
/// `Arc<dyn ExtensionContext>` for the adapter). A self-contained test mock
/// mirroring `Ctx` (installer.rs:138-160) — avoids depending on
/// `HostExtensionContext::new`'s ctor signature (a cross-crate guess).
fn ctx_cmd() -> Arc<dyn ExtensionCommandContext> {
    struct CmdCtx { generation: u64 }
    #[async_trait::async_trait]
    impl ExtensionContext for CmdCtx {
        fn cwd(&self) -> &std::path::Path { std::path::Path::new(".") }
        fn mode(&self) -> ExtensionMode { ExtensionMode::Tui }
        fn is_idle(&self) -> bool { true }
        fn signal(&self) -> tokio_util::sync::CancellationToken {
            tokio_util::sync::CancellationToken::new()
        }
        fn generation(&self) -> u64 { self.generation }
    }
    impl ExtensionCommandContext for CmdCtx {}
    Arc::new(CmdCtx { generation: 1 })
}

#[tokio::test]
async fn register_extension_tools_adapts_bound_tools_into_registry() {
    let runner = ExtensionRunner::new();
    runner.load(&ToolExt).await.expect("load ToolExt");
    runner.bind_core(ctx_cmd());

    let mut registry = ToolRegistry::new(ToolContext::new("."));
    register_extension_tools(&mut registry, &runner);

    assert!(registry.contains("echo_ext"), "adapter registered echo_ext");
    let tool = registry.get("echo_ext").expect("echo_ext present");
    let out = tool
        .execute(serde_json::json!({"text":"hi"}), &ToolContext::new("."))
        .await
        .expect("execute via ToolSpec path");
    // EchoTool (extension.rs:115) returns ToolResult::success("echo:hi");
    // the adapter forwards it as the ToolSpec execute result — mirrors
    // `adapter_executes_extension_tool` at extension.rs:133-135.
    assert!(out.success, "ToolSpec execute succeeds");
    assert_eq!(out.content, "echo:hi");
}
```

> **Note on `EchoTool` / `ctx()`:** the existing test module already defines an `EchoTool` (a `ToolDefinition` with name `"echo_ext"`) + a `ctx()` returning `Arc<dyn ExtensionContext>`. Confirm both by reading the `#[cfg(test)] mod tests` block before writing; if `EchoTool`'s name differs, adjust the `"echo_ext"` assertions to match. If `HostExtensionContext::new`'s signature has drifted from `populate_extension_runtime` (engine.rs:463-469), mirror THAT call exactly (it is the canonical host-construction site).

- [ ] Run: `cargo test -p codesmith_agent_runtime --lib tools::extension::tests::register_extension_tools_adapts_bound_tools_into_registry`
- [ ] **Expected fail:** `error[E0425]: cannot find function 'register_extension_tools'` (helper not yet defined). The `bound_context()` absence surfaces once the helper calls it.

### Step 1.2 — Green: `bound_context()` + helper

- [ ] In `crates/extensions/src/runner.rs`, add `bound_context()` immediately after `bound_tools()` (runner.rs:307+). `ExtensionContext` is already in scope (`use codesmith_agent::extension::*;` at top).

```rust
/// §F5d — bound `ExtensionContext` for the host's `ExtensionToolSpecAdapter`
/// (upcasts the stored `Arc<dyn ExtensionCommandContext>` to
/// `Arc<dyn ExtensionContext>` via Rust 1.86+ trait-upcasting). `None` before
/// `bind_core`. Used per-turn by `register_extension_tools`.
#[must_use]
pub fn bound_context(&self) -> Option<Arc<dyn ExtensionContext>> {
    self.context
        .lock()
        .expect("context lock poisoned")
        .clone()
        .map(|ctx| -> Arc<dyn ExtensionContext> { ctx })
}
```

- [ ] In `crates/agent-runtime/src/tools/extension.rs`, add imports + the helper. The file already has `use std::sync::Arc;` + `use super::spec::{ToolContext, ToolSpec};` (confirm by reading the top of the file; adjust import paths to match). Add:

```rust
use super::registry::ToolRegistry;
use codesmith_extensions::ExtensionRunner;

/// §F5d — register every bound extension tool into the host `ToolRegistry`,
/// each wrapped in an [`ExtensionToolSpecAdapter`]. Called from
/// `EngineHost::build_turn_dispatcher` after plugin-tools are configured.
///
/// Per-turn rebuild → no persistent host holder; clearing `runner.tools[id]`
/// before the next turn's call suffices. Ext tools are **main-turn-only**:
/// they are NOT added to any subagent's tool-subset basis (§4b — subagents
/// build their own fresh built-in-only `ToolRegistry` and never hold dylib
/// `Arc`s; the exclusion is structural, not a runtime guard).
pub fn register_extension_tools(registry: &mut ToolRegistry, runner: &ExtensionRunner) {
    let Some(ctx) = runner.bound_context() else {
        // No bound context yet (pre-bind_core) → nothing to adapt.
        return;
    };
    for (_name, tool) in runner.bound_tools() {
        let adapter = ExtensionToolSpecAdapter::new(tool, ctx.clone());
        registry.register(Arc::new(adapter) as Arc<dyn ToolSpec>);
    }
}
```

- [ ] Run: `cargo test -p codesmith_agent_runtime --lib tools::extension::tests::register_extension_tools_adapts_bound_tools_into_registry`
- [ ] **Expected pass.** (Green.) If the `Extension::configure` / `ExtensionApi::register_tool` signatures differ from the fixture's (`crates/extensions-fixture-dylib/src/lib.rs:35` `api.register_tool(Box::new(FixtureEchoTool))`), mirror the fixture exactly.

### Step 1.3 — Wire into `build_turn_dispatcher`

- [ ] In `crates/tui/src/core/engine/runtime_traits.rs`, locate `build_turn_dispatcher` (:236). Find the plugin-tools block ending around :440-441 (`if let Some(ref mut tool_registry) = tool_registry { ... = configure_plugin_tools(...); }`) and the catalog build starting ~:444. Insert **between** them:

```rust
// §F5d T1 — register extension tools (main-turn-only; not inherited by
// subagents — §4b: subagents build their own fresh built-in-only registry).
// Per-turn rebuild; clearing runner.tools[id] before the next turn suffices.
if let Some(ref mut tool_registry) = tool_registry {
    if let Some(runner) = &self.extension_runner {
        codesmith_agent_runtime::tools::extension::register_extension_tools(
            tool_registry,
            runner,
        );
    }
}
```

- [ ] Confirm the import path resolves: `codesmith_agent_runtime::tools` is `pub` (lib.rs:90 `pub mod tools`) → `tools::extension` is `pub` (tools/mod.rs:23 `pub mod extension`) → `register_extension_tools` is `pub fn`. (The `EngineHost.extension_runner: Option<Arc<ExtensionRunner>>` field is already used at :258 for ProjectTrust emit — same field.)
- [ ] Build the workspace: `cargo build` (no new deps; expect 0 warnings in the touched files).
- [ ] Run the full agent-runtime suite: `cargo test -p codesmith_agent_runtime` → expect `1163 pass / 2 ignored` (no regression; the new test adds +1 → `1164 pass / 2 ignored`).
- [ ] Run ext: `cargo test -p codesmith_extensions` → `48 pass` (untouched, sanity).

### Step 1.4 — Commit

- [ ] `git add -A && git commit`. Message (adjust test counts to the ACTUAL `cargo test` numbers — do not fabricate):

```
feat(framework): §F5d T1 wire ext tools into host per-turn registry (bound_context() upcast Arc<dyn ExtensionCommandContext>→Arc<dyn ExtensionContext> via Rust 1.86+ trait-upcast on ExtensionRunner; register_extension_tools(&mut ToolRegistry,&ExtensionRunner) helper in agent-runtime/tools/extension.rs wraps each bound tool in ExtensionToolSpecAdapter→ToolRegistry::register[fail-closed,last-wins,cache-invalidate]; called from EngineHost::build_turn_dispatcher after plugin-tools[~:440] before catalog[~:444]; per-turn rebuild→no persistent host holder+clearing runner.tools[id] before next turn suffices; main-turn-only NOT added to subagent basis[§4b structural]; API reconciliation: bound_tools() was display-only[status cmd]+adapter was test-only-dead→now production-wired; agent-runtime N→N+1 pass/2 ignored; ext 48 unchanged)
```

---

## Task 2 — Wire ext slash commands into `execute()`

**Overview.** `ExtensionRunner::try_dispatch_command(name, args)` (runner.rs:294, async) is defined-but-never-called. Wire it as a new tier in `commands::execute` after the `/extension` meta-tier (:585) and before the static match (:657). Async-from-sync via a current-thread tokio rt (mirror `populate_extension_runtime` engine.rs:431). `CommandOutput` → `CommandResult` mapping.

### Step 2.0 — Confirm `CommandOutput` variants

- [ ] Read `crates/agent/src/extension.rs` around the `CommandOutput` definition (~:383). Confirm the variant names. The spec records `CommandOutput::Message(String)` / `CommandOutput::SendMessage(String)`. If they differ, adjust the `match` in Step 2.2 to the real variants.

### Step 2.1 — Red: command-dispatch test

- [ ] This test needs a runner with a **command** registered. Check whether the fixture dylib (`crates/extensions-fixture-dylib`) registers a command (read its `lib.rs` `codesmith_register_extension`). If it does, mirror the `installer.rs:216-237` round-trip to load it; if it registers only a tool, write a static `CmdExt` (mirror `ToolExt` from T1 but calling `api.register_command(Box::new(SomeCmd))`). Place the test in `crates/tui/src/commands/extension_commands.rs` `#[cfg(test)] mod tests` (or `commands/mod.rs` test module — wherever the existing `execute()` tests live; grep `fn execute` tests to find the harness).

```rust
#[test]
fn try_dispatch_extension_command_resolves_contributed_command() {
    // Build a runner with a contributed command (CmdExt or fixture dylib).
    // Mirror installer.rs:216-237 round-trip for load+bind_core.
    let runner = ExtensionRunner::new();
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(runner.load(&CmdExt)).expect("load CmdExt");
    runner.bind_core(host_cmd_ctx()); // Arc<dyn ExtensionCommandContext>

    let mut app = // minimal App with extension_runner = Some(runner)
        test_app_with_runner(runner);

    let res = try_dispatch_extension_command(&app, "fixture_cmd", "hello");
    assert!(res.is_some(), "contributed command dispatched");
    let res = res.unwrap();
    assert!(!res.is_error, "command succeeded");
    assert!(res.message.unwrap().contains("hello"), "arg forwarded");
}
```

> **Fixture note:** if writing `CmdExt` in-process, `CommandDefinition` (the trait the runner's `commands` registry stores) must be impl'd — mirror the fixture's command registration if one exists, else define `struct EchoCmd; impl CommandDefinition for EchoCmd { fn name(&self)->&str { "fixture_cmd" } async fn run(&self, ctx, args)->Result<CommandOutput,_> { Ok(CommandOutput::Message(format!("echo:{args}"))) } }`. Read `crates/agent/src/extension.rs` `CommandDefinition` trait for the exact signature. `test_app_with_runner` mirrors existing App-test fixtures (grep `App::new` / `test_app` in `crates/tui/src/commands/`).

- [ ] Run: `cargo test -p codesmith_tui --lib commands::...::try_dispatch_extension_command_resolves_contributed_command`
- [ ] **Expected fail:** `cannot find function 'try_dispatch_extension_command'`.

### Step 2.2 — Green: dispatch helper + wire into `execute()`

- [ ] In `crates/tui/src/commands/extension_commands.rs`, add the helper. Imports: `use crate::tui::app::AppAction;` + `use super::CommandResult;` + `use codesmith_agent::extension::CommandOutput;` (confirm the path).

```rust
/// §F5d T2 — dispatch an extension-registered slash command.
///
/// `ExtensionRunner::try_dispatch_command` is async (`CommandDefinition::run`
/// is async) but `commands::execute` is sync → use a current-thread tokio rt
/// (mirror `populate_extension_runtime`, engine.rs:431). Returns `None` when
/// no runner is bound or no command matches `name` (so the static-match tier
/// + built-in commands still run). `CommandOutput` → `CommandResult`:
/// `Message(s)`→display; `SendMessage(s)`→agent send (mirrors
/// `user_commands.rs:222`).
fn try_dispatch_extension_command(app: &App, name: &str, args: &str) -> Option<CommandResult> {
    let runner = app.extension_runner.clone()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("extension command dispatch runtime");
    let out = rt.block_on(runner.try_dispatch_command(name, args))?;
    Some(match out {
        CommandOutput::Message(s) => CommandResult::message(s),
        CommandOutput::SendMessage(s) => CommandResult::action(AppAction::SendMessage(s)),
    })
}
```

> **Nested-runtime caveat:** if `commands::execute` is ever invoked from within an active tokio runtime context, `block_on` panics ("Cannot start a runtime from within a runtime"). The slash-command path runs on the UI thread (not inside the engine's runtime) — same precondition `populate_extension_runtime` relies on. If the Red test reveals otherwise, switch to `std::thread::scope` + a spawned current-thread rt (mirror the `thread::scope` form in engine.rs:429). Surface it in the commit notes; do NOT silently paper over.

- [ ] Wire into `commands::execute` (`crates/tui/src/commands/mod.rs:573`). After the `extension_commands::try_dispatch(app, cmd.trim())` call (~:585) and before the static `match` (~:657), add a tier:

```rust
// §F5d T2 — extension-registered slash command (e.g. /mycmd args).
// Tier after /extension meta-commands, before built-in static match.
if let Some(name_with_args) = cmd.trim().strip_prefix('/') {
    let (name, args) = match name_with_args.split_once(' ') {
        Some((n, rest)) => (n, rest),
        None => (name_with_args, ""),
    };
    if let Some(res) = extension_commands::try_dispatch_extension_command(app, name, args) {
        return res;
    }
}
```

> Read the actual `execute()` body around :580-660 first — the `/extension` tier + static match are the landmarks. If the leading `/` is already stripped by an earlier tier, drop the `strip_prefix` + use the already-stripped form. Match the surrounding style exactly.

- [ ] Run the Step 2.1 test → **expected pass.**
- [ ] Run `cargo test -p codesmith_tui` → expect `2836 pass / 26 pre-existing runtime_api fail / 2 ignored` (+1 from the new test; the 26 are environmental — NOT a regression).

### Step 2.3 — Commit

```
feat(framework): §F5d T2 wire ext slash commands into execute() (try_dispatch_command[runner.rs:294 async,defined-but-never-called]→new tier in commands::execute[mod.rs:573] after /extension meta-tier[~:585] before static match[~:657]; try_dispatch_extension_command helper in extension_commands.rs: current-thread tokio rt block_on[mirror populate engine.rs:431]+CommandOutput→CommandResult[Message→::message, SendMessage→::action(AppAction::SendMessage) mirrors user_commands.rs:222]; None on no-runner/no-match so built-ins still run; API reconciliation: try_dispatch_command was dead→now production-wired; tui N→N+1 pass/26 pre-existing runtime_api fail/2 ignored—zero §F5d regression)
```

---

## Task 3 — Safe unload: `clear_tools` / `clear_commands` + reload ordering

**Overview.** Add `clear_tools()` + `clear_commands()` on `ExtensionRunner` (mirror `clear_handlers` runner.rs:156). Call them from `reload_extension_runtime` (engine.rs:484) in the §3 ordering before re-populate. Safe to run concurrently with an in-flight turn (refcount/name-keyed; `Library` alive until T4 drops it).

### Step 3.0 — Shared test helper: `runner_with_fixture_dylib()`

- [ ] T3 + T4 tests need an `ExtensionRunner` with a live `Library` (loaded fixture dylib) in `runner.libraries` + `fixture_echo` bound. runner.rs tests do NOT currently load a dylib. Add a helper in `crates/extensions/src/runner.rs` `#[cfg(test)] mod tests`. The fixture-dylib artifact path is the compile-time env var `CODESMITH_FIXTURE_DYLIB` (used at `installer.rs:189` — available crate-wide since runner.rs is in the same `codesmith_extensions` crate). A **direct `load_dylib`** suffices (T3/T4 test clear/drain semantics, NOT the install pipeline — `installer.rs:188 install_to_load_roundtrip_binds_fixture_tool` already covers install→discover→load).

```rust
/// §F5d T3/T4 test helper — runner with the fixture dylib loaded +
/// `fixture_echo` bound. Direct `load_dylib` on `env!("CODESMITH_FIXTURE_DYLIB")`
/// (same compile-time env var as installer.rs:189); skips the install→discover
/// round-trip (T3/T4 test clear/drain semantics, not the install pipeline).
fn runner_with_fixture_dylib() -> crate::ExtensionRunner {
    let runner = crate::ExtensionRunner::new();
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(runner.load_dylib(std::path::Path::new(env!("CODESMITH_FIXTURE_DYLIB"))))
        .expect("load fixture dylib");
    runner.bind_core(Arc::new(Ctx { generation: 1 }));
    runner
}
```

> **`Ctx`** already exists in runner.rs tests (the `Ctx { generation }` struct that impls `ExtensionContext` + `ExtensionCommandContext` — mirror of installer.rs:138-160). **No `FakeSource`/`FakeBuilder`/`Installer` duplication needed** for T3/T4 (only the install-pipeline test at installer.rs:188 needs those).

### Step 3.1 — Red: clear-tools / clear-commands unit test

- [ ] In `crates/extensions/src/runner.rs` `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn clear_tools_and_clear_commands_empty_registries() {
    let runner = runner_with_fixture_dylib();

    assert!(
        runner.bound_tools().iter().any(|(n, _)| n == "fixture_echo"),
        "fixture_echo bound before clear"
    );

    runner.clear_tools();
    assert!(
        runner.bound_tools().is_empty(),
        "tools cleared: {:?}",
        runner.bound_tools()
    );

    // clear_commands: safe on whatever the fixture registered (tool-only →
    // commands empty pre- and post-; assert no panic + stays empty). If the
    // fixture also registers a command, add a "present-then-cleared" assert
    // here (read crates/extensions-fixture-dylib/src/lib.rs to confirm).
    runner.clear_commands();

    // Re-load proves clear is non-destructive to the runner itself.
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(runner.load_dylib(std::path::Path::new(env!("CODESMITH_FIXTURE_DYLIB"))))
        .expect("reload");
    runner.bind_core(Arc::new(Ctx { generation: 2 }));
    assert!(
        runner.bound_tools().iter().any(|(n, _)| n == "fixture_echo"),
        "fixture_echo re-bound after reload"
    );
}
```

- [ ] Run: `cargo test -p codesmith_extensions --lib runner::tests::clear_tools_and_clear_commands_empty_registries`
- [ ] **Expected fail:** `no method named 'clear_tools'` (and `clear_commands`).

### Step 3.2 — Green: `clear_tools` / `clear_commands`

- [ ] In `crates/extensions/src/runner.rs`, next to `clear_handlers` (:156), add:

```rust
/// §F5d — clear all bound **tools** (name→`Arc<dyn ToolDefinition>` map).
///
/// Safe to call while an in-flight engine turn holds clones: each tool is
/// refcounted (`Arc`) + the per-turn `ToolRegistry` already captured its own
/// `Arc`; clearing here only affects FUTURE turns' `register_extension_tools`
/// rebuild. Called from `reload_extension_runtime` before re-populate.
pub fn clear_tools(&self) {
    self.tools.lock().expect("tools lock poisoned").clear();
}

/// §F5d — clear all bound **commands** (name→`CommandDefinition` map). Same
/// concurrency reasoning as `clear_tools`. Called from
/// `reload_extension_runtime`.
pub fn clear_commands(&self) {
    self.commands.lock().expect("commands lock poisoned").clear();
}
```

- [ ] Run the Step 3.1 test → **expected pass.**
- [ ] Run `cargo test -p codesmith_extensions` → `49 pass` (+1; was 48).

### Step 3.3 — Reload ordering

- [ ] In `crates/tui/src/core/engine.rs` `reload_extension_runtime` (:484). Current body (verified): `runner.clear_handlers();` (:490) → `runner.invalidate();` (:491) → `populate_extension_runtime(...)` (:492). Insert the new clears + drain **after `clear_handlers` and before `invalidate`**:

```rust
runner.clear_handlers();
// §F5d T3 — also clear tools/commands so the re-populate doesn't leave
// stale bindings (name-keyed maps; safe concurrent w/ in-flight turn).
runner.clear_tools();
runner.clear_commands();
// §F5d T4 — move live dylib Libraries to pending_drop (UI-thread MOVE,
// safe: Library stays alive until the engine drops it at the next op-loop
// top). populate_extension_runtime will load fresh dylibs into libraries.
runner.drain_libraries_to_pending();
runner.invalidate();
populate_extension_runtime(runner, workspace, state, shared_cancel_token);
```

> `drain_libraries_to_pending()` is added in T4. If executing T3 before T4 (recommended order), the `drain_libraries_to_pending()` line will not compile yet — either (a) implement T4's `drain_libraries_to_pending` + `drop_pending` + `pending_drop` FIRST (T3 + T4 share the runner.rs edit), or (b) temporarily omit the drain line in T3 + add it in T4. **Recommended:** do T3 + T4 runner.rs edits together, then the engine.rs reload edit + mod.rs op-loop edit together. The tasks are split for review clarity, not strict ordering. The steps below assume T4's runner methods land before this engine.rs edit is committed.

- [ ] Build: `cargo build`.
- [ ] Run the existing reload test (tui) — grep `reload` in `crates/tui/src/core/engine/tests.rs` (the harness at :4157 / :4198). It should still pass (clears are additive; the re-load repopulates). `cargo test -p codesmith_tui --lib core::engine` → expect pass (the 26 `runtime_api` fails are elsewhere).
- [ ] Run full tui: `cargo test -p codesmith_tui` → `2837 pass / 26 pre-existing runtime_api fail / 2 ignored` (+1 from the T3 unit test if it's in the tui crate; if it's in extensions, tui stays +0).

### Step 3.4 — Commit

```
feat(framework): §F5d T3 safe reload-deferred clear (clear_tools/clear_commands on ExtensionRunner[mirror clear_handlers runner.rs:156, Mutex<HashMap>.clear()]; reload_extension_runtime[engine.rs:484] ordering: clear_handlers→clear_tools→clear_commands→drain_libraries_to_pending[T4]→invalidate→populate; safe concurrent w/ in-flight engine turn[tools/commands refcounted Arc+name-keyed, Library alive until T4 drop]; API reconciliation: clear_handlers was lone clear→now trio+drain; ext N→N+1 pass; tui N pass/26 pre-existing runtime_api fail/2 ignored—zero §F5d regression)
```

---

## Task 4 — Two-phase `Library` drop + §4b subagent-exclusion regression

**Overview.** (a) `pending_drop: Mutex<Vec<Library>>` field + `drain_libraries_to_pending()` (UI-thread MOVE) + `drop_pending()` (engine op-loop-top DROP). (b) §4b: subagents never hold dylib `Arc`s — **structurally automatic** (verified: `SubAgentToolRegistry::new` rebuilds its own fresh built-in registry; `SubAgentRuntime` has no `extension_runner` field). Add a doc-comment locking the invariant + a regression test (expected Green).

### Step 4.1 — Red: drain/drop unit test

- [ ] In `crates/extensions/src/runner.rs` test module, add (uses the fixture-dylib round-trip to get a live `Library` in `libraries`):

```rust
#[test]
fn drain_libraries_to_pending_moves_then_drop_pending_empties() {
    let runner = runner_with_fixture_dylib(); // helper from Step 3.0

    // The loaded Library now lives in runner.libraries.
    // (No pub accessor for libraries count by design — assert via behavior:
    //  drain moves it to pending_drop; drop_pending then frees it.)

    runner.drain_libraries_to_pending();
    // After drain, libraries is empty; pending_drop holds the Library.
    // A second drain is a no-op (drains an empty Vec).
    runner.drain_libraries_to_pending();

    runner.drop_pending();
    // drop_pending actually drops the Library (dylib unloaded). A second
    // drop_pending on an empty pending is a no-op (must not panic).
    runner.drop_pending();
}
```

> **Why no `libraries` count assertion:** `libraries` is `Mutex<Vec<Library>>` with no pub accessor (by design — exposing `Library` would leak `libloading` internals). The test proves the drain/drop semantics behaviorally (idempotent, no panic). If a `pending_drop_len()` test-only accessor is desired, add it `#[cfg(test)]`; otherwise this behavioral test suffices.

- [ ] Run: `cargo test -p codesmith_extensions --lib runner::tests::drain_libraries_to_pending_moves_then_drop_pending_empties`
- [ ] **Expected fail:** `no method named 'drain_libraries_to_pending'`.

### Step 4.2 — Green: `pending_drop` field + `drain_libraries_to_pending` + `drop_pending`

- [ ] In `crates/extensions/src/runner.rs`, add the field to the `ExtensionRunner` struct (next to `libraries: Mutex<Vec<Library>>` ~:112):

```rust
/// §F5d T4 — staging area for `Library`s orphaned by a UI-thread
/// `reload_extension_runtime` clear. Populated by
/// `drain_libraries_to_pending` (a safe `Arc`-MOVE under one lock — the
/// `Library` stays alive) + drained+dropped by `drop_pending` at the engine
/// op-loop top (the one moment the main-thread `HostAgentExecutor` — the
/// only in-flight dylib `Arc` holder between turns — is already dropped).
/// Never dropped on the UI thread: doing so while an in-flight turn holds a
/// dylib `Arc` would be UAF (dangling vtable). See spec §4a/§4b.
pending_drop: Mutex<Vec<Library>>,
```

- [ ] Initialise it in `ExtensionRunner::new` (find the `..Default::default()` / field-init in `new`; add `pending_drop: Mutex::new(Vec::new()),`).

- [ ] Add the methods next to `clear_libraries`-adjacent code (after `clear_handlers`/`clear_tools`/`clear_commands`):

```rust
/// §F5d T4 — MOVE the live `libraries` into `pending_drop` (UI-thread,
/// reload-time). Safe: takes the `libraries` lock once + `std::mem::take`s
/// the `Vec` (each `Library` is an owned handle, not a borrowed `Arc`);
/// the `Library` stays alive until [`drop_pending`] runs. The
/// main-thread executor's per-turn dylib `Arc`s are unaffected (they point
/// at the `Arc<Library>` the engine captured this turn; the runner's own
/// `Vec` move does not touch them). Called from `reload_extension_runtime`
/// before re-populate loads fresh dylibs.
pub fn drain_libraries_to_pending(&self) {
    let mut libs = self.libraries.lock().expect("libraries lock poisoned");
    let drained = std::mem::take(&mut *libs);
    let mut pending = self.pending_drop.lock().expect("pending_drop lock poisoned");
    pending.extend(drained);
}

/// §F5d T4 — DROP the pending `Library`s. Called ONLY from the engine
/// op-loop top (agent-runtime engine/mod.rs:517) before `match op`, at the
/// one moment the main-thread `HostAgentExecutor` (the only in-flight dylib
/// `Arc` holder) is already dropped between turns. Dropping here unloads the
/// dylibs safely. Idempotent (empty pending → no-op).
pub fn drop_pending(&self) {
    let mut pending = self.pending_drop.lock().expect("pending_drop lock poisoned");
    let _drained = std::mem::take(&mut *pending);
    // `_drained` drops here → dylibs unloaded.
}
```

> **`std::mem::take` requires `Vec<Library>: Default`** — `Vec<T>` impls `Default`, so `mem::take` on `Vec<Library>` yields the Vec + leaves an empty Vec. `Library` itself is not `Default`, but we never `mem::take` a single `Library`, only the `Vec`. ✓.

- [ ] Run the Step 4.1 test → **expected pass.**
- [ ] Run `cargo test -p codesmith_extensions` → `50 pass` (+1; was 49).

### Step 4.3 — Engine op-loop-top drain

- [ ] In `crates/agent-runtime/src/engine/mod.rs`, locate the op-loop `while let Some(op) = self.rx_op.recv().await {` (:517). Insert the drain **immediately after the `recv().await` binds `op`, before `match op`**:

```rust
while let Some(op) = self.rx_op.recv().await {
    // §F5d T4 — drop any Libraries orphaned by a UI-thread reload. This is
    // the safe moment: the main-thread HostAgentExecutor from the previous
    // turn was dropped at that turn's return (host_executor.rs:1850,
    // "constructed fresh each turn"); no in-flight dylib Arc lives on the
    // main thread here. See spec §4a/§4b.
    if let Some(runner) = &self.extension_runner {
        runner.drop_pending();
    }
    match op {
        // ... existing arms ...
    }
}
```

> `self.extension_runner: Option<Arc<ExtensionRunner>>` is an `Engine` field (:196). `drop_pending` is `pub` (added above). Confirm `self.extension_runner` is accessible from the op-loop method (it's a field on `&mut self` / `&self` per the loop's receiver). The loop is in an `impl Engine` method — field access is fine.

- [ ] Build: `cargo build`.
- [ ] Run agent-runtime: `cargo test -p codesmith_agent_runtime` → `1164 pass / 2 ignored` (the T1 test already +1'd; T4 adds no agent-runtime test of its own except the §4b one in Step 4.5 — but that's in tui). If a deadlock/liveness test in agent-runtime touches the op-loop, ensure it still drains (pending is empty in those tests → no-op).

### Step 4.4 — §4b doc-comment (locks the invariant)

- [ ] In `crates/tui/src/tools/subagent/mod.rs`, on `SubAgentToolRegistry::new` (:6096), add/extend the doc-comment:

```rust
/// §F5b/§F5d — Build the sub-agent's own `ToolRegistry` fresh from built-ins
/// ("the full agent surface — same as the parent's Agent mode"). The
/// sub-agent does **not** clone the parent's `ToolRegistry` `Arc`s + does
/// **not** bind an `extension_runner` (`SubAgentRuntime` has no such field,
/// see mod.rs:580-608; only `HostAgentExecutor` binds one). Consequently
/// extension-contributed dylib tools — which are registered only in
/// `EngineHost::build_turn_dispatcher` (§F5d T1) — can **never** reach a
/// sub-agent's effective tool set, regardless of `inherit_full_registry`.
///
/// §4b invariant: a sub-agent never holds a dylib `Arc` across a turn
/// boundary (it holds none at all). This needs **no runtime subagent-check
/// guard** — the exclusion is the structural precondition, not a guard. The
/// regression test `subagent_ext_tool_excluded_*` locks this.
```

### Step 4.5 — §4b regression test (expected Green)

- [ ] This test asserts a subagent's `tools_for_model` excludes an ext tool that IS bound on the parent's runner. It needs the subagent test harness. Grep `tools_for_model\|SubAgentToolRegistry::new\|SubAgentRuntime` in `crates/tui/src/tools/subagent/tests.rs` (or wherever the subagent unit tests live) to find an existing test that constructs a `SubAgentToolRegistry` + calls `tools_for_model`. Mirror it. The assertion:

```rust
#[test]
fn subagent_ext_tool_excluded_from_effective_set() {
    // Parent has fixture_echo bound on its ExtensionRunner (load fixture dylib).
    // The sub-agent is built WITHOUT the runner (SubAgentRuntime has none).
    // Assert fixture_echo is NOT in the sub-agent's tools_for_model, under
    // BOTH inherit_full_registry=false AND =true.
    //
    // Mirror the existing SubAgentToolRegistry::new construction (mod.rs:4643):
    //   let reg = SubAgentToolRegistry::new(runtime, agent_type, allowed, todo, plan);
    //   let tools = reg.tools_for_model(&agent_type);
    //   assert!(!tools.iter().any(|t| t.function.name == "fixture_echo"));
    // Build `runtime` (SubAgentRuntime) per the existing harness; set
    // inherit_full_registry true for the second assertion.
    //
    // EXPECTED GREEN (exclusion is structural). If this is RED, ext tools are
    // leaking into subagents — implement provenance-marker exclusion as the
    // fallback (spec §4b) + surface in commit notes.
}
```

> **Harness note:** `SubAgentToolRegistry::new` (mod.rs:6096) takes `(SubAgentRuntime, SubAgentType, Option<Vec<String>>, Arc<Mutex<TodoList>>, Arc<Mutex<PlanState>>)`. `SubAgentRuntime::new` (mod.rs:616) is heavy (client, model, context, allow_shell, event_tx, manager). The existing subagent tests construct minimal fixtures — mirror one precisely. The assertion itself is trivial + expected Green. **If constructing a full SubAgentRuntime is too heavy for a clean unit test**, fall back to a narrower invariant test: assert that `SubAgentToolRegistry`'s `tools_for_model` (built from any existing harness fixture) never includes a name that was registered ONLY on a separate `ExtensionRunner` that was never passed to the subagent. The point is to lock "ext tools don't transit" — pick the lightest construction that proves it.

- [ ] Run → **expected Green.** If Green, the §4b exclusion is confirmed automatic (no fallback impl needed). If Red, STOP + surface it (do not silently add a guard — the spec's resolution was exclusion, not a guard).

### Step 4.6 — Commit

```
feat(framework): §F5d T4 two-phase Library drop + §4b subagent exclusion (pending_drop:Mutex<Vec<Library>> field on ExtensionRunner; drain_libraries_to_pending[UI-thread MOVE: mem::take libraries→pending_drop under one lock, Library stays alive] called from reload_extension_runtime; drop_pending[engine op-loop top mod.rs:517 before match op: mem::take pending→drop, the safe moment main-thread HostAgentExecutor already dropped between turns host_executor.rs:1850]; §4b RESOLVED structurally-automatic: SubAgentToolRegistry::new[:6096] rebuilds OWN fresh built-in registry[no parent Arc clone]+SubAgentRuntime has NO extension_runner field[mod.rs:580-608]→ext tools[added only in build_turn_dispatcher]never reach subagent set→doc-comment locks invariant+regression test expected-Green[no provenance-marker/guard needed]; API reconciliation: libraries was Mutex<Vec> unload-only→now drain→pending→drop two-phase safe; ext N→N+1 pass; agent-runtime N pass/2 ignored; tui N pass/26 pre-existing runtime_api fail/2 ignored—zero §F5d regression)
```

---

## Task 5 — Docs: `/extension uninstall` message + EXTENSIONS.md + ROADMAP.md

**Overview.** Update the uninstall message (drop the stale "bounded retention" caveat → state the §F5d reality), + reflect wire-now-live in EXTENSIONS.md, + add the §F5d progress block to ROADMAP.md.

### Step 5.1 — Rewrite uninstall message

- [ ] In `crates/tui/src/commands/extension_commands.rs`, the `uninstall` fn success message (current text at :317-319):

```rust
// CURRENT (stale):
"Uninstalled extension '{id}'.\n⚠ tools/commands remain bound until process restart (bounded retention, §F5b Q1); handlers clear on next /extension reload."
```

Replace with the §F5d reality (live bindings clear on the next `/extension reload` — the clears added in T3 + the dylib drop in T4):

```rust
if report.removed {
    CommandResult::message(format!(
        "Uninstalled extension '{id}'.\nLive tool/command bindings clear on the next /extension reload (§F5d); the dylib unloads safely at the next turn boundary."
    ))
} else {
    // ... existing not-found branch unchanged
}
```

> Read the full `uninstall` fn (:298-325) first; only the `report.removed` success arm's message changes. The `else` (not-found) arm + the `error` returns stay.

### Step 5.2 — EXTENSIONS.md

- [ ] Read `docs/EXTENSIONS.md`. Update:
  - **Intro:** note tools + commands are now **wired live** (per-turn rebuild in `build_turn_dispatcher`; slash commands via `execute()` tier) — was previously "registered but not yet wired".
  - **In-TUI / Sandbox Stance section:** add a note that ext tools are **main-turn-only** — they are NOT inherited by subagents (§4b structural); subagents build their own fresh built-in registry.
  - **Uninstall row** (the §F5c install/uninstall table): update the post-§F5d behavior — "live bindings clear on next `/extension reload`; dylib unloads at next turn boundary" (was "bounded retention until restart").

### Step 5.3 — ROADMAP.md

- [ ] Read `ROADMAP.md` §F5c progress block (~:2608) + the §F section (~:2941). Add a **§F5d progress block** mirroring the §F5c format: R-items reconciled, T1-T5 done, test-count deltas, by-design gaps (§4b automatic-exclusion resolution). Mark §F5d-done in the §F section + set the next-focus mark.

### Step 5.4 — Build + full-suite sanity

- [ ] `cargo build` (docs-only + message change → no new compile errors).
- [ ] Full baseline run (plain `cargo`):
  - `cargo test -p codesmith_extensions` → expect `50 pass` (48 + T3 + T4).
  - `cargo test -p codesmith_agent` → `98 pass` (untouched).
  - `cargo test -p codesmith_agent_runtime` → `1164 pass / 2 ignored` (1163 + T1). Isolate-rerun `streamable_http_stale_session_reconnects_and_retries_tool_call` if flaky.
  - `cargo test -p codesmith_tui` → `2837 pass / 26 pre-existing runtime_api fail / 2 ignored` (2835 + T2 + T4-§4b). **Report exactly this** — never "green", never attribute the 26 to §F5d.

### Step 5.5 — Commit

```
docs(framework): §F5d T5 wire-live + unload docs (uninstall message: drop stale 'bounded retention, §F5b Q1' caveat→'live tool/command bindings clear on next /extension reload (§F5d); dylib unloads safely at next turn boundary'; EXTENSIONS.md intro: tools+commands now wired-live per-turn[was 'registered but not wired']+In-TUI/Sandbox Stance: ext-tools-main-turn-only NOT inherited by subagents[§4b structural]+uninstall row updated; ROADMAP.md §F5d progress block[R-items,T1-T5,test deltas,by-design §4b automatic-exclusion]+§F5d-done+next-focus; ext 50/agent 98/agent-runtime 1164+2/tui 2837 pass+26 pre-existing runtime_api fail+2 ignored—zero §F5d regression)
```

---

## Self-Review (run after writing, before execution handoff)

- [ ] **Spec coverage:** every spec §10 task (T1-T5) is a plan Task; every §0 decision (combined scope / reload-deferred / two-phase drop) is reflected. §4b's "RESOLVED by exclusion" maps to T4 Steps 4.4-4.5.
- [ ] **Placeholder scan:** grep the plan for `TODO`, `TBD`, `...`, `FIXME`. The `// ... existing arms ...` / `// ... existing branches ...` comments mark CODE TO KEEP (not placeholders) — confirm each is at a clear landmark. No incomplete code steps.
- [ ] **Type consistency:** `bound_context()` returns `Option<Arc<dyn ExtensionContext>>` (matches `ExtensionToolSpecAdapter::new`'s `ctx: Arc<dyn ExtensionContext>`). `register_extension_tools` takes `&mut ToolRegistry` + `&ExtensionRunner` (both `pub`/accessible). `drain_libraries_to_pending`/`drop_pending` take `&self` (called via `&Arc<ExtensionRunner>`). `CommandOutput` variants confirmed in Step 2.0.
- [ ] **Insertion points:** runtime_traits.rs :440/:444 (T1), mod.rs :585/:657 (T2), engine.rs :490/:492 (T3), mod.rs :517 (T4) — all verified on `main` HEAD `2eba6e9c` (spec §2). Re-check each on the branch before editing (line numbers may have drifted if T1-T4 edits are in the same files).
- [ ] **Concurrency reasoning:** `clear_tools`/`clear_commands`/`clear_handlers` safe (refcount + name-keyed, no `Library` touch); `drain_libraries_to_pending` safe (MOVE under one lock, `Library` alive); `drop_pending` safe ONLY at op-loop top (main-thread executor already dropped). The plan never drops `Library` on the UI thread.
- [ ] **Reporting:** every commit + the final sanity use REAL `cargo test` counts; tui is always `N pass / 26 pre-existing runtime_api fail / 2 ignored`.

## Execution handoff

After this plan is approved, offer the user the execution choice (per `writing-plans` skill):
1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per Task (T1→T5), each running Red→Green→commit with review between.
2. **Inline Execution** — execute the steps in this session sequentially via `executing-plans`.

Do NOT `git checkout -b feat/ext-wire-unload` or write any code until the user picks. (HARD-GATE / operational constraint: branch first on non-trivial work.)
