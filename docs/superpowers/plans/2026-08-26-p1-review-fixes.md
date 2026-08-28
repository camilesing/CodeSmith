# P1 Review Fixes: Gate Sandbox Routing, State Pragmas, Subagent Hold Timeout, Trim Pair Integrity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the four P1 defects from the 2026-08-26 review: (1) `task_gate_run` executing `/bin/sh -lc` directly with inherited environment and no sandbox, (2) inert `ON DELETE CASCADE` + no WAL/busy_timeout in the state crate, (3) the sub-agent blocking hold hanging forever when a child panics or is aborted, (4) emergency context trims orphaning tool_use/tool_result pairs.

**Architecture:** (1) routes the gate command through the session's `ShellManager` via `ToolContext::shell_manager` (sandbox decision + env allowlist + process-group timeout kill) inside `spawn_blocking`; (2) adds per-connection SQLite pragmas in `StateStore::conn`; (3) wraps the hold's `biased select!` in a re-check loop with a timeout arm; (4) adds a shrink-only pair-enforcement helper applied after both front-trim sites. Each fix is independent; all are test-first.

**Tech Stack:** Rust 2024, tokio (`select!`, `spawn_blocking`), rusqlite 0.32 (bundled), existing test harnesses (`HostAgentExecutor` mock harness in `host_executor.rs`, `ToolContext::new` builder).

**Verified defect background (do not re-derive):**

- `crates/tui/src/tools/tasks.rs:28-43` builds `/bin/sh -lc` (`-l` = login shell sources profile files) and `:461-464` runs it via `tokio::process::Command::output()` — no `ShellManager`, no sandbox decision, no env allowlist (`child_env`), inherits every parent env var including API keys. `ToolContext.shell_manager: Arc<dyn ShellManagerApi>` (`crates/agent-runtime/src/tools/spec.rs:160`) exposes `execute_with_options_env(&self, command, working_dir, timeout_ms, background, stdin, tty, policy_override, extra_env)` (`crates/agent-runtime/src/host_services.rs:287`) which internally: clamps timeout 1s–600s, applies the session sandbox (`decide_sandbox`), env_clear + allowlist (`prepare`/`prepare_unsandboxed_for_fallback`), kills the process group on timeout. It is synchronous/blocking; the exec_shell tool calls it directly on the async path only through the background+poll pattern — for the gate tool use `tokio::task::spawn_blocking` (the API object is `Send + Sync`, `Arc` clone moves in). `ShellResult` fields: `status: ShellStatus {Running|Completed|Failed|Killed|TimedOut}`, `exit_code`, `stdout`, `stderr`, `sandboxed`, `sandbox_backend`, `sandbox_denied`, `sandbox_unavailable_reason` (`crates/agent-runtime/src/tools/shell_types.rs:20-34`). `ShellStatus`/`ShellResult` are re-exported into the TUI via `crate::tools::shell::*` (glob re-export of tool-impls, which re-exports agent-runtime types).
- `crates/state/src/lib.rs:264-267` — `conn()` opens `Connection::open(&self.db_path)` with zero pragmas. SQLite defaults `foreign_keys=OFF` per connection, so the schema's `FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE` (on `messages`, `checkpoints`, `thread_dynamic_tools`) never fires: `delete_thread` (line 524) removes only the thread row. No `journal_mode=WAL`, no `busy_timeout` — concurrent writers get immediate `SQLITE_BUSY`. rusqlite idioms: `conn.pragma_update(None, "foreign_keys", "1")`, `conn.pragma_update(None, "journal_mode", "WAL")`, `conn.busy_timeout(Duration::from_secs(5))`.
- `crates/agent-runtime/src/engine/host_executor.rs:2839-2950` — the sub-agent hold: non-blocking `try_recv` drain, then `if completions.is_empty() && let Some(api) = &self.subagent_api { let running = api.running_count().await; if should_hold_turn_for_subagents(0, running) { … biased select! over cancel / completion recv / steer recv — NO timeout arm } }`. A child that panics or is aborted never emits its sentinel (`emit_parent_completion` is only on the normal completion path), so the parent parks in `recv().await` forever. The enclosing step loop is `loop {` at line 2452, UNLABELED; the steer arm's `step += 1; continue;` (line ~2937) targets it. `SubAgentApi` (`crates/agent-runtime/src/host_services.rs:197`) is an async trait (`running_count`, `list`, `cleanup`, `live_running_snapshots`) — mockable in tests. The executor test harness at `host_executor.rs:4020+` constructs `HostAgentExecutor::new(client, tools, callback, config, event_tx, lsp, steer, approval, compaction, capacity, subagent, cancel_token, subagent_api)` — the last two params are exactly what a hold test needs.
- Both emergency trims do blind `messages.remove(0)` loops: `Engine::trim_oldest_messages_to_budget` (`crates/agent-runtime/src/engine/mod.rs:2001-2010`, `recover_context_overflow` caller) and `trim_oldest_messages_to_budget_history` (`crates/agent-runtime/src/engine/capacity_flow.rs:166-186`, the live mid-loop `TargetedContextRefresh` fallback). A front-trim that removes an assistant `tool_use` message leaves its `tool_result` orphaned; providers reject the next request and the request-build safety net silently discards the orphaned results. The compaction planner's `enforce_tool_call_pairs` (`crates/agent-runtime/src/compaction/compact.rs:427`) grows a PIN set (re-adds counterparts) — wrong direction for an emergency trim, which must only shrink to preserve the budget guarantee. Correct emergency semantics: drop kept messages that contain a `tool_result` whose `tool_use` id is no longer present in the kept set (only results can be orphaned by a front-trim; calls precede their results), cascading until stable.

**Working agreement:**

- Branch: `fix/p1-review-findings` created FROM `fix/p0-review-fixes` (stacked; P1 files are disjoint from P0 files). The user's staged `docs/*.md` deletions stay untouched — commit with explicit pathspecs only.
- TDD: failing test → verify red → implement → verify green → commit, per task.
- Out of scope (follow-ups): `Retry-After` clamping, LLM request timeouts, MCP 10s-timeout client, state-crate schema-migration framework, approval-cache grouping keys, session-resume pair reconciliation (A8).

---

### Task 1: State crate pragmas (foreign_keys / WAL / busy_timeout)

**Files:**
- Modify: `crates/state/src/lib.rs` (`conn()` at lines 264-267, tests module)

- [ ] **Step 1: Write the failing cascade test**

Append to the `#[cfg(test)] mod tests` module in `crates/state/src/lib.rs` (match existing test imports — check what the module already imports for `ThreadMetadata`/item construction and mirror an existing persistence test's setup; `ThreadMetadata` requires: `id`, `rollout_path`, `preview`, `ephemeral`, `model_provider`, `created_at`, `updated_at`, `status: ThreadStatus::Active` (check exact variant in the enum at line ~1280), `path`, `cwd`, `cli_version`, `source`, plus `Default`-able tails — consult an existing test that calls `upsert_thread` and copy its metadata construction verbatim):

```rust
    #[test]
    fn delete_thread_cascades_to_messages_and_checkpoints() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = StateStore::open(Some(tmp.path().join("cascade.db"))).expect("open");
        let meta = /* existing test's ThreadMetadata construction, id = "t-cascade" */;
        store.upsert_thread(&meta).expect("upsert");

        // One assistant tool_use message + its tool_result child, plus a
        // checkpoint — all FK-linked to the thread.
        store
            .append_message("t-cascade", None, /* assistant Message JSON item */)
            .expect("append call");
        store
            .append_message("t-cascade", /* parent id */, /* tool_result item */)
            .expect("append result");
        store
            .save_checkpoint("t-cascade", "cp", /* thread leaf id */, &serde_json::json!({"k": 1}))
            .expect("checkpoint");
        assert!(!store.list_messages("t-cascade", None).expect("list").is_empty());

        store.delete_thread("t-cascade").expect("delete");

        // ON DELETE CASCADE must remove the FK-linked rows; without
        // PRAGMA foreign_keys=ON they were orphaned forever.
        let msgs = store.list_messages("t-cascade", None).expect("list after");
        let cps = store.list_checkpoints("t-cascade").expect("cps after");
        assert!(
            msgs.is_empty() && cps.is_empty(),
            "delete_thread must cascade; got {} messages / {} checkpoints",
            msgs.len(),
            cps.len()
        );
    }
```

Before writing, read `append_message`/`save_checkpoint`/`list_checkpoints` signatures (lines 684, 898, 978) and an existing test that uses them; fill the placeholders from those — do NOT invent parameter shapes.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p codesmith-state delete_thread_cascades`
Expected: FAIL — non-empty messages/checkpoints after delete (cascade inert).

- [ ] **Step 3: Implement the pragmas**

Replace `conn()` (lines 264-267):

```rust
    fn conn(&self) -> Result<Connection> {
        let conn =
            Connection::open(&self.db_path).with_context(|| {
                format!("failed to open state db {}", self.db_path.display())
            })?;
        // foreign_keys is OFF by default in SQLite (per connection), which
        // made the schema's ON DELETE CASCADE clauses inert — deleting a
        // thread orphaned its messages/checkpoints/dynamic tools forever.
        // WAL allows concurrent readers alongside a writer; busy_timeout
        // makes concurrent writers wait instead of failing SQLITE_BUSY
        // immediately (StateStore is Clone-shared across axum handlers).
        conn.pragma_update(None, "foreign_keys", "1")
            .context("failed to enable foreign_keys")?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("failed to enable WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("failed to set busy_timeout")?;
        Ok(conn)
    }
```

- [ ] **Step 4: Run the full state suite**

Run: `cargo test -p codesmith-state`
Expected: all pass (existing 6 tests + parity suite + new test). If a pre-existing test asserts non-cascade behavior (leftover rows), update it to the cascade semantics and note it in the commit.

- [ ] **Step 5: Commit**

```bash
git commit -m "fix(state): enable foreign_keys/WAL/busy_timeout on every connection

ON DELETE CASCADE was inert (SQLite defaults foreign_keys=OFF per
connection), so delete_thread orphaned messages/checkpoints
permanently — unbounded DB growth and 'deleted' conversations
retained in plaintext. WAL + 5s busy_timeout stop immediate
SQLITE_BUSY failures between the Clone-shared store's concurrent
writers." -- crates/state/src/lib.rs
```

---

### Task 2: `task_gate_run` routes through the ShellManager

**Files:**
- Modify: `crates/tui/src/tools/tasks.rs` (imports; delete `build_gate_command_parts`/`build_gate_command` at lines 28-46; `TaskGateRunTool::execute` at ~440-530; tests at 1150+)

- [ ] **Step 1: Write the failing tests**

Replace the obsolete `gate_command_uses_login_shell_invocation` test with:

```rust
    #[tokio::test]
    async fn gate_run_executes_through_shell_manager() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let context = crate::tools::spec::ToolContext::new(tmp.path().to_path_buf());
        let input = serde_json::json!({
            "gate": "test",
            "command": "echo gate-routed",
        });
        let result = TaskGateRunTool
            .execute(input, &context)
            .await
            .expect("gate execute");
        let content = result.content.as_object().expect("content object");
        let gate = content["gate"].as_object().expect("gate record");
        assert_eq!(gate["status"], "passed", "stdout: {:?}", result.content);
        assert!(
            result.content.to_string().contains("gate-routed"),
            "gate stdout must be captured"
        );
        // Routing through ShellManager carries sandbox metadata.
        assert!(result.metadata.get("sandboxed").is_some());
    }

    #[tokio::test]
    async fn gate_run_timeout_enforced_by_manager() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let context = crate::tools::spec::ToolContext::new(tmp.path().to_path_buf());
        let input = serde_json::json!({
            "gate": "test",
            "command": "sleep 30",
            "timeout_ms": 1000,
        });
        let result = TaskGateRunTool
            .execute(input, &context)
            .await
            .expect("gate execute");
        assert!(
            result.content.to_string().contains("\"timeout\""),
            "sleep 30 with 1s budget must time out"
        );
    }
```

Check how sibling tests in the file obtain `ToolContext` (none currently do — import `crate::tools::spec::ToolContext` explicitly; it is already in scope via the file's `use crate::tools::spec::{…}` if `ToolContext` is listed — add it). `execute` is `async_trait` — `TaskGateRunTool.execute(input, &context)` must be called as a trait method; mirror how other tool tests in the repo call execute (e.g. `tool.execute(input, &context).await` where `Tool` is imported).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p codesmith-tui gate_run`
Expected: FAIL — compile error (`build_gate_command` still used) or metadata assertion failure. If compile blocks the run, that counts as red; proceed.

- [ ] **Step 3: Implement the routing**

Delete `build_gate_command_parts` and `build_gate_command` (lines 28-46). Extend imports: `use std::collections::HashMap;`, `ShellStatus` via the existing shell re-export (`use crate::tools::shell::{ExecShellTool, ShellStatus, ShellWaitTool};` — verify `ShellStatus` is re-exported there; if not, `use codesmith_agent_runtime::tools::shell_types::ShellStatus;`). Replace the direct-spawn block inside `TaskGateRunTool::execute` (the `let started = …; let mut cmd = build_gate_command(&command, &cwd); let output = tokio::time::timeout(…, cmd.output()).await; …` region) with:

```rust
        let started = Instant::now();
        // Route through the session ShellManager: sandbox decision, env
        // allowlist (no inherited API keys), non-login shell, and
        // process-group kill on timeout. The manager API is synchronous and
        // blocks up to the timeout — keep it off the async worker.
        let manager = context.shell_manager.clone();
        let cwd_str = cwd.to_string_lossy().to_string();
        let sandbox_runtime = context.sandbox_runtime.clone();
        let command_for_spawn = command.clone();
        let spawn = tokio::task::spawn_blocking(move || {
            manager.set_sandbox_runtime(sandbox_runtime);
            manager.execute_with_options_env(
                &command_for_spawn,
                Some(&cwd_str),
                timeout_ms,
                false,
                None,
                false,
                None,
                HashMap::new(),
            )
        })
        .await;

        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (exit_code, stdout, stderr, timed_out, spawn_error, sandboxed, sandbox_backend) =
            match spawn {
                Ok(Ok(result)) => {
                    let timed_out = matches!(result.status, ShellStatus::TimedOut);
                    let spawn_error = if result.sandbox_denied {
                        result
                            .sandbox_unavailable_reason
                            .or_else(|| Some("sandbox denied execution".to_string()))
                    } else {
                        None
                    };
                    (
                        result.exit_code,
                        result.stdout,
                        result.stderr,
                        timed_out,
                        spawn_error,
                        result.sandboxed,
                        result.sandbox_backend,
                    )
                }
                Ok(Err(err)) => (
                    None,
                    String::new(),
                    String::new(),
                    false,
                    Some(err.to_string()),
                    false,
                    None,
                ),
                Err(join_err) => (
                    None,
                    String::new(),
                    String::new(),
                    false,
                    Some(format!("gate worker task failed: {join_err}")),
                    false,
                    None,
                ),
            };
```

Then in the existing `metadata` construction add:

```rust
            "sandboxed": sandboxed,
            "sandbox_backend": sandbox_backend,
```

The downstream mapping (`status`, `full_log`, `classify_gate_failure`, `TaskGateRecord`) keeps working unchanged — the tuple shape matches what the old code produced. Remove the now-unused `use std::process::Stdio;` and `use tokio::process::Command;` imports if nothing else in the file uses them (grep first).

- [ ] **Step 4: Run the gate tests**

Run: `cargo test -p codesmith-tui gate_run`
Expected: both pass. `echo` is a safe command; default context sandbox policy is `SandboxPolicy::None` → unsandboxed fallback with env allowlist — the test asserts routing, not sandbox enforcement.

- [ ] **Step 5: Run the full tui unit suite for the file's neighborhood**

Run: `cargo test -p codesmith-tui tools::tasks`
Expected: pass (schema tests unchanged).

- [ ] **Step 6: Commit**

```bash
git commit -m "fix(tui): route task_gate_run through the ShellManager

The gate tool spawned /bin/sh -lc directly: no sandbox decision, no
env allowlist (children inherited every parent env var including
API keys), login-shell profile sourcing, and a tokio timeout that
left the process group running. Route through
context.shell_manager.execute_with_options_env (sandbox decision +
env allowlist + non-login shell + process-group kill) inside
spawn_blocking; surface sandboxed/sandbox_backend in the gate
metadata." -- crates/tui/src/tools/tasks.rs
```

---

### Task 3: Sub-agent hold timeout arm

**Files:**
- Modify: `crates/agent-runtime/src/engine/host_executor.rs` (label `loop {` at 2452 as `'step:`, hold block at 2851-2950, steer arm continue at ~2937, tests module ~4020+)

- [ ] **Step 1: Write the failing test**

In the `#[cfg(test)] mod tests` module (it already has `MockLlm`, `text_block`, `finish`, `fresh_session`, `SessionChatHistory`, `CallbackBridge`, `RecordingHookHost` helpers — reuse them). Add a mock near the other test doubles:

```rust
    /// Sub-agent API whose running count flips to 0 after the first call —
    /// simulates a child that panicked/was aborted after the hold decision
    /// was made: no completion sentinel will ever arrive.
    struct VanishingSubAgentApi {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl VanishingSubAgentApi {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl crate::host_services::SubAgentApi for VanishingSubAgentApi {
        async fn running_count(&self) -> usize {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 { 1 } else { 0 }
        }
        async fn list(&self) -> Vec<crate::host_services::SubAgentResult> {
            Vec::new()
        }
        async fn cleanup(&self, _max_age: std::time::Duration) {}
        async fn live_running_snapshots(&self) -> Vec<crate::host_services::SubAgentResult> {
            Vec::new()
        }
    }
```

(Check whether `SubAgentApi` is `#[async_trait]`-based or native async-in-trait — mirror the trait's actual shape from `host_services.rs:197`; if the trait is not `async_trait`, drop the attribute. `SubAgentResult` type name/path per the trait's return types.) Then the test — copy the structure of `host_executor_drives_full_bridge_trio` (MockLlm with ONE text-only call → `NoToolCalls`), passing a subagent receiver whose sender is dropped or never used, and the mock api:

```rust
    #[tokio::test(start_paused = true)]
    async fn subagent_hold_exits_when_children_vanish_without_sentinel() {
        // …same registry/session/callback setup as the bridge-trio test…
        let (sub_tx, sub_rx) = tokio::sync::mpsc::unbounded_channel();
        drop(sub_tx); // no sentinel will EVER arrive

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![/* single text-only call: "done" */])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            Some(Arc::new(VanishingSubAgentApi::new())),
        );
        // With paused time the re-check sleep auto-advances; a hang (the
        // bug) makes this timeout fire and fail the test.
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            executor.run(&mut history, "do it"),
        )
        .await
        .expect("hold must not hang when children vanish");
        // Whatever the stop reason, reaching here proves the hold exited.
        let _ = outcome;
    }
```

Adjust `run`'s actual signature/arguments to match the harness's existing tests exactly (look at how `host_executor_drives_full_bridge_trio` invokes `run` and copy it, swapping only the `subagent`/`subagent_api` args). `start_paused = true` requires that nothing in the path performs real blocking I/O — the MockLlm harness qualifies (existing tests are tokio::test).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p codesmith-agent-runtime subagent_hold_exits`
Expected: FAIL by timeout (60 simulated seconds advance instantly under paused time, then `timeout` fires).

- [ ] **Step 3: Implement the re-check loop**

3a. Label the step loop: change `loop {` at line 2452 to `'step: loop {`.

3b. Replace the hold block (from `if completions.is_empty()` at 2859 through the post-select `while let Ok(extra) = sub_guard.try_recv() { … }` drain at ~2947) with:

```rust
                    if completions.is_empty()
                        && let Some(api) = &self.subagent_api
                    {
                        let running = api.running_count().await;
                        if should_hold_turn_for_subagents(0, running) {
                            // Re-check loop: a child that panicked or was
                            // aborted never emits its completion sentinel, and
                            // `running_count()` can drop to 0 after this check —
                            // without the timeout arm the turn would park in
                            // recv().await forever ("Waiting on N sub-agent(s)"
                            // with nothing left to wait for).
                            let mut running_now = running;
                            loop {
                                self.emit_status(format!(
                                    "Waiting on {running_now} sub-agent(s) to complete..."
                                ))
                                .await;
                                let sub_arc = Arc::clone(probe);
                                let mut sub_guard = sub_arc.lock().await;
                                let cancel_token = self.cancel_token.clone();
                                tokio::select! {
                                    biased;
                                    _ = async {
                                        match &cancel_token {
                                            Some(token) => token.cancelled().await,
                                            None => std::future::pending::<()>().await,
                                        }
                                    } => {
                                        self.emit_status(
                                            "Request cancelled while waiting for sub-agents"
                                                .to_string(),
                                        )
                                        .await;
                                        callback
                                            .on_complete(&StopReason::Interrupted)
                                            .await;
                                        return Ok(StopReason::Interrupted);
                                    }
                                    Some(c) = sub_guard.recv() => {
                                        completions.push(c);
                                    }
                                    Some(steer) = async {
                                        match &self.steer {
                                            Some(rx) => rx.lock().await.recv().await,
                                            None => {
                                                std::future::pending::<Option<String>>().await
                                            }
                                        }
                                    } => {
                                        let trimmed = steer.trim().to_string();
                                        if !trimmed.is_empty() {
                                            let status = format!(
                                                "Steer input accepted: {}",
                                                summarize_text(&trimmed, 120)
                                            );
                                            self.push_steer_message(trimmed, history);
                                            self.emit_status(status).await;
                                        }
                                        step += 1;
                                        continue 'step;
                                    }
                                    _ = tokio::time::sleep(SUBAGENT_HOLD_RECHECK_INTERVAL) => {
                                        let still_running = match &self.subagent_api {
                                            Some(api) => api.running_count().await,
                                            None => 0,
                                        };
                                        if still_running == 0 {
                                            tracing::warn!(
                                                "sub-agent hold re-check: no running \
                                                 sub-agents left but no completion sentinel \
                                                 arrived (child panicked or was aborted)"
                                            );
                                            break;
                                        }
                                        running_now = still_running;
                                        continue;
                                    }
                                }
                                // Completion arm won: drain anything batched
                                // behind the first, then exit the hold.
                                while let Ok(extra) = sub_guard.try_recv() {
                                    completions.push(extra);
                                }
                                break;
                            }
                        }
                    }
```

Preserve any doc comments from the original block that still apply (the borrow-notes comments about `sub_arc`/`cancel_token`). Add the const near `should_hold_turn_for_subagents` (find its definition in the file):

```rust
/// How often the sub-agent blocking hold re-checks `running_count()` while
/// waiting for completion sentinels. Bounds how long the turn can stay
/// parked when a child dies without emitting its sentinel.
const SUBAGENT_HOLD_RECHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
```

3c. Verify no other `continue` statements between the `'step:` label and the hold were relying on unlabeled targeting (the goal-continuation `continue` at ~2818 sits OUTSIDE the new inner loop — unchanged; grep `continue` within 2839-2950 after the edit and confirm each one's target explicitly).

- [ ] **Step 4: Run the new test and the executor suite**

Run: `cargo test -p codesmith-agent-runtime subagent_hold_exits && cargo test -p codesmith-agent-runtime host_executor`
Expected: pass. Also run `cargo test -p codesmith-tui core::engine` (the TUI engine tests exercise the executor through production wiring).

- [ ] **Step 5: Commit**

```bash
git commit -m "fix(agent-runtime): re-check running_count in the sub-agent hold

A sub-agent that panicked or was aborted never emits its completion
sentinel; the hold's select had no timeout arm, so the parent turn
parked in recv().await forever once running_count had already been
observed >0. Wrap the hold in a re-check loop (5s interval) that
falls through when no children remain, and label the step loop so
the steer arm keeps targeting it." -- crates/agent-runtime/src/engine/host_executor.rs
```

---

### Task 4: Emergency trims preserve tool_use/tool_result pairs

**Files:**
- Modify: `crates/agent-runtime/src/engine/capacity_flow.rs` (new helper + call site at 166-186), `crates/agent-runtime/src/engine/mod.rs` (call site at 2001-2010)

- [ ] **Step 1: Write the failing tests**

Add a `#[cfg(test)] mod front_trim_tests` module to `capacity_flow.rs` (check the file's existing imports for `Message`/`ContentBlock` — it works with `ChatHistory`; import `codesmith_agent::models::{ContentBlock, Message}` as needed):

```rust
#[cfg(test)]
mod front_trim_tests {
    use super::*;
    use codesmith_agent::models::{ContentBlock, Message};

    fn user(text: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn assistant_call(id: &str) -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
                caller: None,
            }],
        }
    }

    fn tool_result(id: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: "ok".to_string(),
                is_error: None,
            }],
        }
    }

    #[test]
    fn front_trim_drops_orphaned_results() {
        // Front-trim removed the call for A but kept its result.
        let mut kept = vec![
            tool_result("A"), // orphaned — its call was trimmed
            assistant_call("B"),
            tool_result("B"),
            user("tail"),
        ];
        let removed = enforce_tool_pairs_after_front_trim(&mut kept);
        assert_eq!(removed, 1);
        assert_eq!(kept.len(), 3);
        assert!(kept.iter().all(|m| !m
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "A"))));
    }

    #[test]
    fn intact_pairs_survive() {
        let mut kept = vec![
            assistant_call("X"),
            tool_result("X"),
            user("tail"),
        ];
        let removed = enforce_tool_pairs_after_front_trim(&mut kept);
        assert_eq!(removed, 0);
        assert_eq!(kept.len(), 3);
    }
}
```

Check `ContentBlock::ToolResult`'s exact field set (`tool_use_id`, `content`, `is_error`?) against `codesmith_agent::models` and adjust the constructor; check `Message`'s exact fields likewise. Do not invent fields — read the enum/struct first.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p codesmith-agent-runtime front_trim_tests`
Expected: compile error (helper does not exist). Red.

- [ ] **Step 3: Implement the helper and wire both call sites**

Add to `capacity_flow.rs` (near `trim_oldest_messages_to_budget_history`):

```rust
/// Drop kept messages whose `tool_result` blocks reference a `tool_use` that
/// a front-trim removed. A front-trim can only orphan *results* (calls
/// precede their results), and dropping only ever shrinks the transcript —
/// preserving the caller's token-budget guarantee. Cascades until stable in
/// case a dropped message also carried calls other results depend on.
/// Returns how many additional messages were removed.
pub(crate) fn enforce_tool_pairs_after_front_trim(kept: &mut Vec<Message>) -> usize {
    let mut removed = 0usize;
    loop {
        let live_calls: std::collections::HashSet<&str> = kept
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let before = kept.len();
        kept.retain(|m| {
            let orphaned = m.content.iter().any(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    !live_calls.contains(tool_use_id.as_str())
                }
                _ => false,
            });
            !orphaned
        });
        let dropped = before - kept.len();
        removed += dropped;
        if dropped == 0 {
            break;
        }
    }
    removed
}
```

Wire `trim_oldest_messages_to_budget_history` (after its remove loop, before the `history.clear()` rebuild):

```rust
    if removed > 0 {
        removed +=
            enforce_tool_pairs_after_front_trim(&mut messages);
        history.clear();
        for m in messages {
            history.push(m);
        }
    }
```

Wire `Engine::trim_oldest_messages_to_budget` in `engine/mod.rs` (the helper is `pub(crate)` in the `capacity_flow` submodule — check the module declaration name in `engine/mod.rs` and use the matching path):

```rust
    fn trim_oldest_messages_to_budget(&mut self, target_input_budget: usize) -> usize {
        let mut removed = 0usize;
        while self.session.messages.len() > MIN_RECENT_MESSAGES_TO_KEEP
            && self.estimated_input_tokens() > target_input_budget
        {
            self.session.messages.remove(0);
            removed = removed.saturating_add(1);
        }
        if removed > 0 {
            removed += crate::engine::capacity_flow::enforce_tool_pairs_after_front_trim(
                &mut self.session.messages,
            );
        }
        removed
    }
```

(Confirm `session.messages` is `Vec<Message>`; if it is behind an accessor, adapt.)

- [ ] **Step 4: Run the tests**

Run: `cargo test -p codesmith-agent-runtime front_trim_tests && cargo test -p codesmith-agent-runtime capacity`
Expected: pass, no regressions in capacity tests.

- [ ] **Step 5: Commit**

```bash
git commit -m "fix(agent-runtime): keep tool_use/tool_result pairs intact after emergency trims

Both emergency front-trims (recover_context_overflow and the live
TargetedContextRefresh fallback) peeled messages off the front
blindly, leaving orphaned tool_results that providers reject and the
request-build safety net silently discards — losing the model's most
recent tool output exactly when context is tightest, prompting
re-runs of (possibly write) tools. Drop orphaned results after the
trim (shrink-only, so the budget guarantee holds)." \
  -- crates/agent-runtime/src/engine/capacity_flow.rs crates/agent-runtime/src/engine/mod.rs
```

---

### Task 5: Workspace verification

- [ ] **Step 1:** `cargo fmt --all` (commit any rewrites with `style: cargo fmt` + explicit paths).
- [ ] **Step 2:** `cargo clippy -p codesmith-state -p codesmith-agent-runtime -p codesmith-tui --all-features --all-targets` — no warnings in the four touched files.
- [ ] **Step 3:** `cargo test -p codesmith-state -p codesmith-agent-runtime -p codesmith-tool-impls` — all pass.
- [ ] **Step 4:** `cargo test -p codesmith-tui tools::tasks core::engine` — pass (full tui suite has 6 pre-existing changelog-release failures unrelated to this work; record and ignore).
- [ ] **Step 5:** `git status --short` — only the user's staged docs deletions and the two plan files unaccounted; commit the plan file:
   `git add docs/superpowers/plans/2026-08-26-p1-review-fixes.md && git commit -m "docs: implementation plan for P1 review fixes" -- docs/superpowers/plans/2026-08-26-p1-review-fixes.md`

---

## Follow-ups recorded (NOT in this plan)

- P2/P3 review items: sandbox read-side sealing, ReadOnly fail-closed defaults, MCP 10s-timeout client + stdio line caps, `Retry-After` clamping + wiring the unused retry machinery, LLM request timeouts, DeepSeek cache-token cost accounting, TUI hooks off the UI thread.
- state crate: versioned migration framework (A3), crash pair-reconciliation on resume (A8), JSONL index compaction (A7).
