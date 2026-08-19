# CodeSmith §F2a — Extension system contract + runtime core — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the full pi-mono lifecycle event set + `HandlerOutcome` cancel/transform/block chaining + per-variant `Handler` subscription + `catch_unwind` isolation in the extension contract (`codesmith-agent`) and runtime (`codesmith-extensions`), plus a mechanical emit-signature update at the 7 `host_executor.rs` seam sites — no new host behavior (that is §F2b).

**Architecture:** §F2 splits into two sub-slices (user-approved): **§F2a** = contract + runtime core (this plan), **§F2b** = host-executor seam wiring for every new event + full e2e round-trip test + App field live wiring + `/extension reload` re-discover. §F2a keeps the existing 7 `host_executor.rs` emit sites emitting the *same* events under a *new* `emit` signature (owned event in, `EmitOutcome` out) — it does NOT add cancel/block/transform *handling* at those seams (§F2b wires the host to honor `EmitOutcome`). The handler trait stays a single dyn-safe `Handler` (`Arc<dyn Handler>` unchanged) returning `Result<HandlerOutcome, ExtensionError>`; per-variant dispatch is a `kind_filter` on a registered-handler entry, not a per-variant trait. This is the smallest delta from §F1 that matches pi-mono's single-handler/union-return model and stays object-safe.

**Tech Stack:** Rust 1.90.0 workspace; `async-trait`, `tokio`, `tokio-util`, `inventory`, `serde_json`; NEW dep `futures-util = "0.3.31"` (matches `codesmith-agent-runtime`'s existing version — needed for `FutureExt::catch_unwind`).

---

## Context — what §F1 left (zero-context recap)

§F1 (commit `961bf380`) landed the extension system foundational core. Relevant state for §F2a:

- **Contract** `crates/agent/src/extension.rs`: `Extension`/`ExtensionApi`/`ExtensionContext`/`ExtensionCommandContext`/`Handler`/`ToolDefinition`/`CommandDefinition` traits; `ExtensionEvent` is `#[non_exhaustive]` with **6 variants** (`SessionStart`/`TurnStart`/`ToolCall`/`ToolResult`/`TurnEnd`/`SessionShutdown`); `Handler::handle` returns `Result<(), ExtensionError>` (observer-only).
- **Runtime** `crates/extensions/`:
  - `runner.rs` — `ExtensionRunner`: `emit(&self, event: &ExtensionEvent)` (best-effort fan-out, awaits directly, **no** `catch_unwind`), stale-context `Arc<AtomicU64>` guard, `bind_core` drains `pending_*` into bound registries.
  - `api.rs` — `StubExtensionApi` (queues into `pending`) + `RealExtensionApi` (defined, `#[allow(dead_code)]`, not constructed in §F1).
  - `bus.rs` — `EventBus` skeleton (§F3).
  - `state.rs` — `HostExtensionContext`.
- **Adapter** `crates/agent-runtime/src/tools/extension.rs` — `ExtensionToolSpecAdapter`.
- **Host** `crates/agent-runtime/src/engine/host_executor.rs` — `extension: Option<Arc<ExtensionRunner>>` field + `with_extension_runner` builder + **7 emit sites** (all `runner.emit(&codesmith_agent::extension::ExtensionEvent::…).await;`): TurnStart `:3735`, TurnEnd-Interrupted `:3783`, TurnEnd-NoToolCalls `:4267`, ToolCall-parallel `:4389`, ToolResult-parallel `:4477`, ToolCall-serial `:4495`, ToolResult-serial `:4590`.
- **Green baseline (confirmed this session):** `cargo +1.90.0 build --workspace` green (142 tui warnings = slice-47 baseline); `codesmith-extensions --lib` 8 pass; `codesmith-agent --lib` 93 pass; `codesmith-agent-runtime --lib` 1152 pass + 2 ignored; `codesmith-tui --bin` 2853 pass + 2 ignored.

## Design decisions (approved brainstorming output — design lives here per slice-1 convention)

1. **Handler trait shape = Approach A (single trait + outcome enum).** Keep §F1's single dyn-safe `Handler` trait; change return to `Result<HandlerOutcome, ExtensionError>`. `Arc<dyn Handler>` stays object-safe. Per-variant subscription is a `kind_filter: Option<ExtensionEventKind>` on the registered-handler entry (NOT a per-variant trait). Rationale: smallest delta from §F1; matches pi-mono's single-handler/union-return model; `ExtensionEvent` is `#[non_exhaustive]` so exhaustive per-variant matching is impossible across crates anyway; pi-mono itself uses one generic handler (no exhaustiveness).

2. **`HandlerOutcome` flat enum.** `Continue` / `Cancel { reason }` / `Block { reason }` / `Transform(ExtensionEvent)`. Variant-specific semantics from spec §4: `SessionBefore*` → cancel; `ToolCall` → block; `Input`/`BeforeAgentStart`/`BeforeProviderRequest`/`ToolResult` → transform; rest observe-only. Enforced at runtime by the host (§F2b), NOT by the type system (Approach A's accepted cost). A `Transform` is folded into the running event and the chain continues; the terminal `EmitOutcome::outcome` is never `Transform` (always `Continue`/`Cancel`/`Block`).

3. **`emit` signature change = owned-in, struct-out.** `pub async fn emit(&self, event: ExtensionEvent) -> EmitOutcome` where `EmitOutcome { event, outcome }`. Chain: iterate subscribed handlers in registration order; `Transform(new)` replaces the running event + continues; `Cancel`/`Block` short-circuit; handler `Err`/panic recorded + best-effort continues (§8.3). The 7 `host_executor.rs` sites mechanically drop the `&` (§F2a discards `EmitOutcome`; §F2b adds inspection). `EmitOutcome` is NOT `#[must_use]` in §F2a (so the mechanical 7-site change is just dropping `&`); §F2b may add `#[must_use]` + inspection.

4. **Per-variant dispatch = one ordered `Vec<RegisteredHandler>`.** Single ordered list `Vec<RegisteredHandler { handler, kind_filter: Option<ExtensionEventKind> }>` — global registration order preserved (deterministic chaining); on emit, dispatch only entries whose filter matches (`None` = all). `ExtensionEvent::kind()` returns an `ExtensionEventKind` discriminant.

5. **`catch_unwind` via `futures-util`.** Add `futures-util = "0.3.31"` to `codesmith-extensions` (matches `codesmith-agent-runtime`'s version). Wrap each handler call in `AssertUnwindSafe(h.handle(...)).catch_unwind().await`; a panic → `tracing::error` + continue. This closes §F1's documented by-design gap.

6. **§F2a/§F2b split.** §F2a = contract + runtime + mechanical 7-site signature update, proven by runner-isolated tests. §F2b = wire every new event to its `HostAgentExecutor`/app seam + full e2e round-trip + App live wiring + reload re-discover. The split lets the hard-to-reverse contract land + stabilize before host wiring.

## File structure

| File | Responsibility | §F2a change |
|---|---|---|
| `crates/agent/src/extension.rs` | Contract (traits + event enum + outcome) | ADD 2 reason enums + 6 payload structs + 17 event variants + `ExtensionEventKind`/`kind()` + `HandlerOutcome` + `Handler::handle` return change + `ExtensionApi::on_variant`; update in-crate test handler |
| `crates/extensions/Cargo.toml` | Runtime deps | ADD `futures-util = "0.3.31"` |
| `crates/extensions/src/runner.rs` | `ExtensionRunner` | ADD `RegisteredHandler`/`EmitOutcome`; rewrite `emit` (owned, chained, per-variant, `catch_unwind`); `handlers` field → `Vec<RegisteredHandler>`; `bind_core` drain w/ kind_filter; update existing tests + add 5 new tests |
| `crates/extensions/src/api.rs` | `Stub`/`Real` `ExtensionApi` | `PendingHandler` + `kind_filter`; impl `on_variant` in both |
| `crates/extensions/src/lib.rs` | Re-exports | re-export `EmitOutcome` |
| `crates/extensions/src/sample_scratchpad.rs` | Sample | `TurnStartLogger` return `Continue` |
| `crates/agent-runtime/src/engine/host_executor.rs` | Host seams | 7 emit sites drop `&` (mechanical); test `RecHandler` return `Continue` |
| `ROADMAP.md` + `ARCHITECTURE.md` + `docs/EXTENSIONS.md` | Docs | §F2a progress entry + status row + dev-guide update |

---

## Task 1: New reason enums + payload structs (contract data)

**Files:**
- Modify: `crates/agent/src/extension.rs` (insert after the existing `ToolResultEvent` struct, before `// === ExtensionContext ===`)

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/agent/src/extension.rs`:

```rust
    #[test]
    fn f2a_payload_structs_construct_and_debug() {
        let _ = TrustReason::FirstLoad;
        let _ = DiscoverReason::Startup;
        let input = InputEvent { text: "hi".into() };
        assert_eq!(input.text, "hi");
        let start = AgentStartEvent { system_prompt: Some("s".into()), inject_message: None };
        assert!(start.system_prompt.is_some());
        let req = BeforeProviderRequestEvent { messages: json!({}) };
        let resp = AfterProviderResponseEvent { response: json!({}) };
        let upd = ToolExecutionUpdateEvent {
            id: "c1".into(),
            name: "echo".into(),
            message: "ok".into(),
        };
        // Debug renders without panic for every new type.
        format!("{start:?} {req:?} {resp:?} {upd:?}");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo +1.90.0 test -p codesmith-agent --lib f2a_payload_structs_construct_and_debug`
Expected: FAIL — `cannot find type TrustReason` / `InputEvent` etc.

- [ ] **Step 3: Write minimal implementation**

Insert into `crates/agent/src/extension.rs`, immediately after the `ToolResultEvent` struct (after line ~147, before `// === ExtensionContext ===`):

```rust
// === §F2a additions: reason enums + payload structs ======================

/// Why a project-trust event fires. Mirrors pi-mono `TrustReason`. §F2a
/// defines the variant + reason enum so handlers can subscribe; the host
/// emits it in §F2b/§F5 (trust prompt is §F5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustReason {
    FirstLoad,
    Trusted,
    Untrusted,
}

/// Why a resource-discovery event fires. Mirrors pi-mono `DiscoverReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverReason {
    Startup,
    Manual,
    Reload,
}

/// Payload for [`ExtensionEvent::Input`] — user input that may be intercepted
/// or transformed. `text` is the actionable field: a handler returning
/// [`HandlerOutcome::Transform`] with a modified `InputEvent` replaces the
/// input the host injects into the conversation (§F2b wires the seam).
#[derive(Debug, Clone)]
pub struct InputEvent {
    pub text: String,
}

/// Payload for [`ExtensionEvent::BeforeAgentStart`]. Both fields are
/// actionable: a handler returning [`HandlerOutcome::Transform`] may set
/// `system_prompt` (replace the system prompt) and/or `inject_message`
/// (prepend a user message before the agent starts). `None` ⇒ no change.
#[derive(Debug, Clone)]
pub struct AgentStartEvent {
    pub system_prompt: Option<String>,
    pub inject_message: Option<String>,
}

/// Payload for [`ExtensionEvent::BeforeProviderRequest`]. `messages` is the
/// actionable field (the provider request body); a handler may transform it.
#[derive(Debug, Clone)]
pub struct BeforeProviderRequestEvent {
    pub messages: Value,
}

/// Payload for [`ExtensionEvent::AfterProviderResponse`]. Observe-only.
#[derive(Debug, Clone)]
pub struct AfterProviderResponseEvent {
    pub response: Value,
}

/// Payload for [`ExtensionEvent::ToolExecutionUpdate`]. Observe-only progress.
#[derive(Debug, Clone)]
pub struct ToolExecutionUpdateEvent {
    pub id: String,
    pub name: String,
    pub message: String,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo +1.90.0 test -p codesmith-agent --lib f2a_payload_structs_construct_and_debug`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/extension.rs
git commit -m "feat(framework): §F2a T1 reason enums + payload structs"
```

---

## Task 2: `ExtensionEvent` 17 new variants + `ExtensionEventKind` + `kind()`

**Files:**
- Modify: `crates/agent/src/extension.rs` — the `ExtensionEvent` enum (lines ~149-163) + the `RecordingHandler` match in tests (lines ~404-411)

- [ ] **Step 1: Write the failing test**

Append to the test block in `crates/agent/src/extension.rs`:

```rust
    #[test]
    fn f2a_event_kind_round_trips_every_variant() {
        use ExtensionEventKind as K;
        let cases: Vec<(ExtensionEvent, ExtensionEventKind)> = vec![
            (ExtensionEvent::ProjectTrust { reason: TrustReason::Trusted }, K::ProjectTrust),
            (ExtensionEvent::SessionStart { reason: SessionReason::Startup }, K::SessionStart),
            (ExtensionEvent::ResourcesDiscover { reason: DiscoverReason::Startup }, K::ResourcesDiscover),
            (ExtensionEvent::Input(InputEvent { text: "x".into() }), K::Input),
            (ExtensionEvent::BeforeAgentStart(AgentStartEvent { system_prompt: None, inject_message: None }), K::BeforeAgentStart),
            (ExtensionEvent::AgentStart, K::AgentStart),
            (ExtensionEvent::TurnStart { turn_id: "t".into() }, K::TurnStart),
            (ExtensionEvent::BeforeProviderHeaders, K::BeforeProviderHeaders),
            (ExtensionEvent::BeforeProviderRequest(BeforeProviderRequestEvent { messages: json!({}) }), K::BeforeProviderRequest),
            (ExtensionEvent::AfterProviderResponse(AfterProviderResponseEvent { response: json!({}) }), K::AfterProviderResponse),
            (ExtensionEvent::ToolExecutionStart, K::ToolExecutionStart),
            (ExtensionEvent::ToolCall(ToolCallEvent { id: "c".into(), name: "n".into(), input: json!({}) }), K::ToolCall),
            (ExtensionEvent::ToolExecutionUpdate(ToolExecutionUpdateEvent { id: "c".into(), name: "n".into(), message: "m".into() }), K::ToolExecutionUpdate),
            (ExtensionEvent::ToolResult(ToolResultEvent { id: "c".into(), name: "n".into(), result: Ok(ToolResult::success("ok")) }), K::ToolResult),
            (ExtensionEvent::ToolExecutionEnd, K::ToolExecutionEnd),
            (ExtensionEvent::TurnEnd { turn_id: "t".into(), reason: TurnEndReason::NoToolCalls }, K::TurnEnd),
            (ExtensionEvent::AgentEnd, K::AgentEnd),
            (ExtensionEvent::AgentSettled, K::AgentSettled),
            (ExtensionEvent::SessionBeforeSwitch, K::SessionBeforeSwitch),
            (ExtensionEvent::SessionBeforeFork, K::SessionBeforeFork),
            (ExtensionEvent::SessionShutdown, K::SessionShutdown),
            (ExtensionEvent::SessionBeforeCompact, K::SessionBeforeCompact),
            (ExtensionEvent::SessionCompact, K::SessionCompact),
        ];
        assert_eq!(cases.len(), 23, "all variants covered");
        for (ev, kind) in cases {
            assert_eq!(ev.kind(), kind, "kind mismatch for {ev:?}");
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo +1.90.0 test -p codesmith-agent --lib f2a_event_kind_round_trips_every_variant`
Expected: FAIL — `cannot find type ExtensionEventKind` + `no variant ProjectTrust`.

- [ ] **Step 3: Write minimal implementation**

Replace the existing `ExtensionEvent` enum (lines ~149-163) with:

```rust
/// Lifecycle events. §F1 minimal set (spec §10.1) + §F2a full set (spec
/// §10.2 + §4): 23 variants total. `#[non_exhaustive]` so future slices can
/// add variants without breaking downstream match arms. Handler dispatch is
/// open (any `Handler` may subscribe to any variant via `on` /
/// `on_variant`). Variant-specific outcome semantics (spec §4): `SessionBefore*`
/// → cancel-capable; `ToolCall` → block-capable; `Input` / `BeforeAgentStart` /
/// `BeforeProviderRequest` / `ToolResult` → transform-capable; rest observe-only.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ExtensionEvent {
    // --- §F1 minimal set (unchanged) ---
    SessionStart { reason: SessionReason },
    TurnStart { turn_id: String },
    ToolCall(ToolCallEvent),
    ToolResult(ToolResultEvent),
    TurnEnd { turn_id: String, reason: TurnEndReason },
    SessionShutdown,
    // --- §F2a additions (spec §10.2 + §4) ---
    ProjectTrust { reason: TrustReason },
    ResourcesDiscover { reason: DiscoverReason },
    Input(InputEvent),
    BeforeAgentStart(AgentStartEvent),
    AgentStart,
    BeforeProviderHeaders,
    BeforeProviderRequest(BeforeProviderRequestEvent),
    AfterProviderResponse(AfterProviderResponseEvent),
    ToolExecutionStart,
    ToolExecutionUpdate(ToolExecutionUpdateEvent),
    ToolExecutionEnd,
    AgentEnd,
    AgentSettled,
    SessionBeforeSwitch,
    SessionBeforeFork,
    SessionBeforeCompact,
    SessionCompact,
}

/// Discriminant of an [`ExtensionEvent`], for per-variant handler subscription
/// via [`ExtensionApi::on_variant`] (§F2a). One variant per `ExtensionEvent`
/// variant; `#[non_exhaustive]` to grow with the event enum.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtensionEventKind {
    ProjectTrust,
    SessionStart,
    ResourcesDiscover,
    Input,
    BeforeAgentStart,
    AgentStart,
    TurnStart,
    BeforeProviderHeaders,
    BeforeProviderRequest,
    AfterProviderResponse,
    ToolExecutionStart,
    ToolCall,
    ToolExecutionUpdate,
    ToolResult,
    ToolExecutionEnd,
    TurnEnd,
    AgentEnd,
    AgentSettled,
    SessionBeforeSwitch,
    SessionBeforeFork,
    SessionShutdown,
    SessionBeforeCompact,
    SessionCompact,
}

impl ExtensionEvent {
    /// The discriminant of this event, for per-variant dispatch. Exhaustive
    /// within this crate: adding an `ExtensionEvent` variant without a
    /// `kind()` arm is a compile error (the future-variant guard).
    #[must_use]
    pub fn kind(&self) -> ExtensionEventKind {
        match self {
            ExtensionEvent::ProjectTrust { .. } => ExtensionEventKind::ProjectTrust,
            ExtensionEvent::SessionStart { .. } => ExtensionEventKind::SessionStart,
            ExtensionEvent::ResourcesDiscover { .. } => ExtensionEventKind::ResourcesDiscover,
            ExtensionEvent::Input(_) => ExtensionEventKind::Input,
            ExtensionEvent::BeforeAgentStart(_) => ExtensionEventKind::BeforeAgentStart,
            ExtensionEvent::AgentStart => ExtensionEventKind::AgentStart,
            ExtensionEvent::TurnStart { .. } => ExtensionEventKind::TurnStart,
            ExtensionEvent::BeforeProviderHeaders => ExtensionEventKind::BeforeProviderHeaders,
            ExtensionEvent::BeforeProviderRequest(_) => ExtensionEventKind::BeforeProviderRequest,
            ExtensionEvent::AfterProviderResponse(_) => ExtensionEventKind::AfterProviderResponse,
            ExtensionEvent::ToolExecutionStart => ExtensionEventKind::ToolExecutionStart,
            ExtensionEvent::ToolCall(_) => ExtensionEventKind::ToolCall,
            ExtensionEvent::ToolExecutionUpdate(_) => ExtensionEventKind::ToolExecutionUpdate,
            ExtensionEvent::ToolResult(_) => ExtensionEventKind::ToolResult,
            ExtensionEvent::ToolExecutionEnd => ExtensionEventKind::ToolExecutionEnd,
            ExtensionEvent::TurnEnd { .. } => ExtensionEventKind::TurnEnd,
            ExtensionEvent::AgentEnd => ExtensionEventKind::AgentEnd,
            ExtensionEvent::AgentSettled => ExtensionEventKind::AgentSettled,
            ExtensionEvent::SessionBeforeSwitch => ExtensionEventKind::SessionBeforeSwitch,
            ExtensionEvent::SessionBeforeFork => ExtensionEventKind::SessionBeforeFork,
            ExtensionEvent::SessionShutdown => ExtensionEventKind::SessionShutdown,
            ExtensionEvent::SessionBeforeCompact => ExtensionEventKind::SessionBeforeCompact,
            ExtensionEvent::SessionCompact => ExtensionEventKind::SessionCompact,
        }
    }
}
```

Then update the in-crate `RecordingHandler` match (test block, lines ~404-411) to add a `_` arm (the existing match is exhaustive over 6 variants; adding 17 breaks it). Replace:

```rust
            let label = match event {
                ExtensionEvent::SessionStart { .. } => "SessionStart",
                ExtensionEvent::TurnStart { .. } => "TurnStart",
                ExtensionEvent::ToolCall(_) => "ToolCall",
                ExtensionEvent::ToolResult(_) => "ToolResult",
                ExtensionEvent::TurnEnd { .. } => "TurnEnd",
                ExtensionEvent::SessionShutdown => "SessionShutdown",
            };
```

with:

```rust
            let label = match event {
                ExtensionEvent::SessionStart { .. } => "SessionStart",
                ExtensionEvent::TurnStart { .. } => "TurnStart",
                ExtensionEvent::ToolCall(_) => "ToolCall",
                ExtensionEvent::ToolResult(_) => "ToolResult",
                ExtensionEvent::TurnEnd { .. } => "TurnEnd",
                ExtensionEvent::SessionShutdown => "SessionShutdown",
                _ => "other",
            };
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo +1.90.0 test -p codesmith-agent --lib`
Expected: PASS — `f2a_event_kind_round_trips_every_variant` + the existing `handler_observes_every_minimal_event_variant` + `extension_event_is_non_exhaustive_safe` all green.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/extension.rs
git commit -m "feat(framework): §F2a T2 full ExtensionEvent set (23 variants) + ExtensionEventKind + kind()"
```

---

## Task 3: `HandlerOutcome` enum

**Files:**
- Modify: `crates/agent/src/extension.rs` (insert before the `Handler` trait, ~line 281)

- [ ] **Step 1: Write the failing test**

Append to the test block:

```rust
    #[test]
    fn f2a_handler_outcome_constructs_each_variant() {
        let _ = HandlerOutcome::Continue;
        let c = HandlerOutcome::Cancel { reason: "no".into() };
        let b = HandlerOutcome::Block { reason: "denied".into() };
        let t = HandlerOutcome::Transform(ExtensionEvent::SessionShutdown);
        format!("{c:?} {b:?} {t:?}");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo +1.90.0 test -p codesmith-agent --lib f2a_handler_outcome_constructs_each_variant`
Expected: FAIL — `cannot find type HandlerOutcome`.

- [ ] **Step 3: Write minimal implementation**

Insert into `crates/agent/src/extension.rs`, immediately before the `// === Handler (observer, slice 1) ===` comment (~line 281):

```rust
// === HandlerOutcome (§F2a) ================================================

/// What a [`Handler`] returns. Drives the cross-handler chain in
/// [`ExtensionRunner::emit`](codesmith_extensions::ExtensionRunner::emit)
/// (spec §4: "一个 handler 的修改对下一个可见"):
///
/// - `Continue` — no change; proceed to the next handler.
/// - `Cancel { reason }` — abort the surrounding operation. Only meaningful
///   for `SessionBefore*` variants (spec §4); ignored (treated as `Continue`)
///   at non-cancel-capable seams so a stray `Cancel` cannot break unrelated
///   flows.
/// - `Block { reason }` — prevent the operation. Only meaningful for
///   `ToolCall`; ignored at non-block-capable seams.
/// - `Transform(event)` — replace the running event with `event` for
///   subsequent handlers AND (at transform-capable seams) apply `event`'s
///   actionable field to the live operation. Only meaningful for `Input` /
///   `BeforeAgentStart` / `BeforeProviderRequest` / `ToolResult`; ignored
///   elsewhere.
///
/// The terminal [`codesmith_extensions::EmitOutcome::outcome`] is never
/// `Transform` — a transform is folded into the running event and the chain
/// continues; the terminal is `Continue` unless a handler short-circuits
/// with `Cancel`/`Block`.
#[derive(Debug, Clone)]
pub enum HandlerOutcome {
    Continue,
    Cancel { reason: String },
    Block { reason: String },
    Transform(ExtensionEvent),
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo +1.90.0 test -p codesmith-agent --lib f2a_handler_outcome_constructs_each_variant`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/extension.rs
git commit -m "feat(framework): §F2a T3 HandlerOutcome enum (Continue/Cancel/Block/Transform)"
```

---

## Task 4: `Handler::handle` return → `Result<HandlerOutcome>` + update all in-tree handlers

**Files:**
- Modify: `crates/agent/src/extension.rs` — `Handler` trait (~line 289-295) + `RecordingHandler` test (~line 403)
- Modify: `crates/extensions/src/runner.rs` — `RecHandler` test (~line 231-243)
- Modify: `crates/extensions/src/api.rs` — `Nop` test handler (~line 136-146)
- Modify: `crates/extensions/src/sample_scratchpad.rs` — `TurnStartLogger` (~line 125-135)
- Modify: `crates/agent-runtime/src/engine/host_executor.rs` — `RecHandler` test (~line 15622-15638)

- [ ] **Step 1: Write the failing test (asserting the new return shape)**

Append to the test block in `crates/agent/src/extension.rs`:

```rust
    #[test]
    fn f2a_handler_handle_returns_continue_by_default() {
        let h = RecordingHandler {
            seen: Mutex::new(Vec::new()),
        };
        let ctx = TestContext::new();
        let out = block_on(h.handle(&ExtensionEvent::SessionShutdown, &ctx)).unwrap();
        assert!(matches!(out, HandlerOutcome::Continue));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo +1.90.0 test -p codesmith-agent --lib f2a_handler_handle_returns_continue_by_default`
Expected: FAIL — `mismatched types: expected HandlerOutcome, found ()` (the trait still returns `Result<(), _>`).

- [ ] **Step 3: Write minimal implementation — change the trait return**

In `crates/agent/src/extension.rs`, replace the `Handler` trait block (~lines 281-295):

```rust
// === Handler (observer, slice 1) ==========================================

/// Lifecycle event observer. Slice 1: observer-only — returns `Ok(())` or
/// an [`ExtensionError`]; the runner fans out best-effort (per §8.3; one
/// failing handler does not block others — slice 1 awaits directly, §F2
/// hardens with proper `catch_unwind`). `HandlerOutcome`
/// (cancel/transform/block) is §F2.
#[async_trait]
pub trait Handler: Send + Sync {
    async fn handle(
        &self,
        event: &ExtensionEvent,
        ctx: &dyn ExtensionContext,
    ) -> Result<(), ExtensionError>;
}
```

with:

```rust
// === Handler (§F2a: outcome-returning) ===================================

/// Lifecycle event handler. §F2a: returns [`HandlerOutcome`]
/// (cancel/transform/block), chained across handlers in
/// [`ExtensionRunner::emit`](codesmith_extensions::ExtensionRunner::emit).
/// §F1 was observer-only (`Result<(), _>`); §F2a upgrades to the outcome
/// chain. `catch_unwind` isolation is in `emit` (§F2a T8), so a panicking
/// handler cannot tear down the agent loop.
#[async_trait]
pub trait Handler: Send + Sync {
    async fn handle(
        &self,
        event: &ExtensionEvent,
        ctx: &dyn ExtensionContext,
    ) -> Result<HandlerOutcome, ExtensionError>;
}
```

- [ ] **Step 4: Update all in-tree handlers to return `Ok(HandlerOutcome::Continue)`**

This is the compile-fix step (the trait change breaks every existing impl). In each file, change the `Ok(())` in the `handle` body to `Ok(HandlerOutcome::Continue)`.

`crates/agent/src/extension.rs` `RecordingHandler` (~line 414):
```rust
            self.seen.lock().unwrap().push(label);
            Ok(())  // ← change to:
```
→
```rust
            self.seen.lock().unwrap().push(label);
            Ok(HandlerOutcome::Continue)
```

`crates/extensions/src/runner.rs` `RecHandler` (~line 242):
```rust
            Ok(())
```
→
```rust
            Ok(HandlerOutcome::Continue)
```
(Add `use super::*` already imports `HandlerOutcome` via `codesmith_agent::extension::*` re-export at module top — verify the `use codesmith_agent::extension::*;` line is present at `runner.rs:17`; it is.)

`crates/extensions/src/api.rs` `Nop` (~line 144):
```rust
                Ok(())
```
→
```rust
                Ok(HandlerOutcome::Continue)
```

`crates/extensions/src/sample_scratchpad.rs` `TurnStartLogger` (~line 134):
```rust
        Ok(())
```
→
```rust
        Ok(HandlerOutcome::Continue)
```

`crates/agent-runtime/src/engine/host_executor.rs` `RecHandler` (~line 15637):
```rust
            Ok(())
```
→
```rust
            Ok(HandlerOutcome::Continue)
```
(The host_executor test module already imports `ExtensionEvent` etc.; verify `HandlerOutcome` is in scope — it re-exports via the `use codesmith_agent::extension::*` if present, else add `HandlerOutcome` to the existing extension import. Check the test module's `use` statements; if `ExtensionEvent` is imported qualified as `codesmith_agent::extension::ExtensionEvent`, use `codesmith_agent::extension::HandlerOutcome::Continue`.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo +1.90.0 test -p codesmith-agent --lib` && `cargo +1.90.0 test -p codesmith-extensions --lib` && `cargo +1.90.0 test -p codesmith-agent-runtime --lib`
Expected: PASS — all three suites green (93 / 8 / 1152+2). The trait return change is the only delta; existing tests assert behavior, not the return value, so they stay green once handlers return `Continue`.

- [ ] **Step 6: Commit**

```bash
git add crates/agent/src/extension.rs crates/extensions/src/runner.rs crates/extensions/src/api.rs crates/extensions/src/sample_scratchpad.rs crates/agent-runtime/src/engine/host_executor.rs
git commit -m "feat(framework): §F2a T4 Handler::handle returns HandlerOutcome; update in-tree handlers to Continue"
```

---

## Task 5: Add `futures-util` dependency

**Files:**
- Modify: `crates/extensions/Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `crates/extensions/Cargo.toml`, add to `[dependencies]` (after `async-trait`):

```toml
futures-util = "0.3.31"
```

(Matches `codesmith-agent-runtime`'s existing `futures-util = "0.3.31"` — needed for `FutureExt::catch_unwind` in T8.)

- [ ] **Step 2: Verify it resolves + builds**

Run: `cargo +1.90.0 build -p codesmith-extensions`
Expected: green (new dep resolves; no code change yet).

- [ ] **Step 3: Commit**

```bash
git add crates/extensions/Cargo.toml
git commit -m "feat(framework): §F2a T5 add futures-util dep (catch_unwind)"
```

---

## Task 6: `ExtensionApi::on_variant` trait method + `PendingHandler.kind_filter` + Stub/Real impls

**Files:**
- Modify: `crates/agent/src/extension.rs` — `ExtensionApi` trait (~line 307-314)
- Modify: `crates/extensions/src/runner.rs` — `PendingHandler` struct (~line 33)
- Modify: `crates/extensions/src/api.rs` — `StubExtensionApi` + `RealExtensionApi` impls

- [ ] **Step 1: Write the failing test**

Append to the test block in `crates/extensions/src/api.rs`:

```rust
    #[tokio::test]
    async fn f2a_stub_on_variant_queues_with_kind_filter() {
        use codesmith_agent::extension::ExtensionEventKind;
        let generation = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(Pending::default()));
        let stub = StubExtensionApi::new(generation.clone(), pending.clone());
        struct Nop;
        #[async_trait]
        impl Handler for Nop {
            async fn handle(
                &self,
                _: &ExtensionEvent,
                _: &dyn ExtensionContext,
            ) -> Result<HandlerOutcome, ExtensionError> {
                Ok(HandlerOutcome::Continue)
            }
        }
        stub.on_variant(ExtensionEventKind::ToolCall, Arc::new(Nop))
            .unwrap();
        let p = pending.lock().unwrap();
        assert_eq!(p.handlers.len(), 1);
        assert_eq!(p.handlers[0].kind_filter, Some(ExtensionEventKind::ToolCall));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo +1.90.0 test -p codesmith-extensions --lib f2a_stub_on_variant_queues_with_kind_filter`
Expected: FAIL — `no method on_variant` + `no field kind_filter on PendingHandler`.

- [ ] **Step 3: Write minimal implementation**

3a. In `crates/agent/src/extension.rs`, replace the `ExtensionApi` trait block (~lines 297-314):

```rust
// === ExtensionApi (registration surface, two-phase) =======================

/// The imperative registration surface an [`Extension::configure`] receives.
/// Two-phase (spec §4 key semantics): the **stub** impl (constructed at
/// load time by `ExtensionRunner`, in `codesmith-extensions`) queues
/// registrations into `pending_*`; `ExtensionRunner::bind_core` swaps in the
/// **real** impl which flushes `pending_*` into the host registries.
///
/// `generation()` exposes the stale-context counter so a handler/command
/// captured `Arc<dyn ExtensionApi>` can assert liveness before use.
#[async_trait]
pub trait ExtensionApi: Send + Sync {
    fn generation(&self) -> u64;

    fn register_tool(&self, tool: Box<dyn ToolDefinition>) -> Result<(), ExtensionError>;
    fn register_command(&self, command: Box<dyn CommandDefinition>) -> Result<(), ExtensionError>;
    /// Subscribe `handler` to ALL events (backward-compat with §F1).
    fn on(&self, handler: Arc<dyn Handler>) -> Result<(), ExtensionError>;
    /// §F2a — subscribe `handler` to a single event variant only. The runner
    /// dispatches only matching events to this handler (per-variant dispatch,
    /// spec §F2). Equivalent to [`on`](Self::on) with a `None` filter.
    fn on_variant(
        &self,
        kind: ExtensionEventKind,
        handler: Arc<dyn Handler>,
    ) -> Result<(), ExtensionError>;
}
```

3b. In `crates/extensions/src/runner.rs`, replace the `PendingHandler` struct (~lines 32-35):

```rust
/// A handler subscribed during `configure`.
pub(crate) struct PendingHandler {
    pub handler: Arc<dyn Handler>,
    pub kind_filter: Option<ExtensionEventKind>,
}
```

3c. In `crates/extensions/src/api.rs`, update `StubExtensionApi` impl — replace the `on` method (~lines 66-70) and add `on_variant`:

```rust
    fn on(&self, handler: Arc<dyn Handler>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.pending.lock().unwrap().handlers.push(crate::runner::PendingHandler {
            handler,
            kind_filter: None,
        });
        Ok(())
    }
    fn on_variant(
        &self,
        kind: ExtensionEventKind,
        handler: Arc<dyn Handler>,
    ) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.pending.lock().unwrap().handlers.push(crate::runner::PendingHandler {
            handler,
            kind_filter: Some(kind),
        });
        Ok(())
    }
```

3d. In `crates/extensions/src/api.rs`, update `RealExtensionApi` impl — replace its `on` method (~lines 118-121) and add `on_variant`:

```rust
    fn on(&self, handler: Arc<dyn Handler>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.handlers.lock().unwrap().push(crate::runner::RegisteredHandler {
            handler,
            kind_filter: None,
        });
        Ok(())
    }
    fn on_variant(
        &self,
        kind: ExtensionEventKind,
        handler: Arc<dyn Handler>,
    ) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.handlers.lock().unwrap().push(crate::runner::RegisteredHandler {
            handler,
            kind_filter: Some(kind),
        });
        Ok(())
    }
```

(Note: `RealExtensionApi`'s `handlers` field type changes to `Arc<Mutex<Vec<RegisteredHandler>>>` in T7; the `push` shape here anticipates that. If T6 compile fails because `RegisteredHandler` doesn't exist yet, define it in T7 first — but to keep T6 self-contained, define `RegisteredHandler` in `runner.rs` now as part of 3b. See T7 for the full struct; in T6 add the minimal definition alongside `PendingHandler`.)

Actually — to keep T6 compiling, add `RegisteredHandler` to `runner.rs` in 3b too:

```rust
/// A bound handler + its variant filter (None = subscribe-to-all). §F2a T7
/// makes `ExtensionRunner::handlers` a `Vec<RegisteredHandler>`; T6 defines
/// it so `RealExtensionApi` (which pushes into the runner's handlers) compiles.
pub(crate) struct RegisteredHandler {
    pub handler: Arc<dyn Handler>,
    pub kind_filter: Option<ExtensionEventKind>,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo +1.90.0 test -p codesmith-extensions --lib`
Expected: PASS — `f2a_stub_on_variant_queues_with_kind_filter` + existing `stub_after_invalidate_returns_stale_context` (the `Nop` there now returns `Continue` from T4) green.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/extension.rs crates/extensions/src/runner.rs crates/extensions/src/api.rs
git commit -m "feat(framework): §F2a T6 ExtensionApi::on_variant + PendingHandler/RegisteredHandler kind_filter + Stub/Real impls"
```

---

## Task 7: `ExtensionRunner.handlers` → `Vec<RegisteredHandler>` + `bind_core` drain w/ kind_filter

**Files:**
- Modify: `crates/extensions/src/runner.rs` — `ExtensionRunner` field (~line 64) + `bind_core` (~lines 116-135)

- [ ] **Step 1: Write the failing test**

Append to the test block in `crates/extensions/src/runner.rs`:

```rust
    #[tokio::test]
    async fn f2a_bind_core_preserves_kind_filter_from_pending() {
        // The `VariantExt` from T8's per-variant test registers a handler
        // via `on_variant`; here we verify `bind_core` drains the kind_filter
        // into the live `handlers` vec by observing that a per-variant
        // handler does NOT fire for a non-matching event (the real dispatch
        // assertion is in T8's `f2a_on_variant_dispatches_only_matching_kind`).
        // This test is a thin structural check: bind_core runs without panic
        // after the field-type change.
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        runner
            .load(&RecExt { seen: seen.clone() })
            .await
            .unwrap();
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        // Structural: emit is a no-op pre-bind in §F1 tests; post-bind it must
        // not panic. (Full dispatch proven in T8.)
        let _ = runner
            .emit(ExtensionEvent::SessionShutdown)
            .await;
    }
```

> Note: this test uses the §F1 `emit` signature; it will need the `&` dropped in T8. For T7, the test compiles only if T8's `emit` signature is already in place. **Run this test in T8**, not T7 — T7 only verifies the field-type change compiles + `bind_core` drains. For T7's Red step, use `cargo +1.90.0 build -p codesmith-extensions` (compile-only) as the gate instead. The dispatch assertion is fully covered by T8.

- [ ] **Step 2: Run build to verify it compiles (T7's gate is compile + existing tests)**

Run: `cargo +1.90.0 build -p codesmith-extensions`
Expected: green (T6 already pushed `RegisteredHandler`; T7 wires the field).

- [ ] **Step 3: Write minimal implementation**

In `crates/extensions/src/runner.rs`, change the `handlers` field type (~line 64):

```rust
    handlers: Mutex<Vec<Arc<dyn Handler>>>,
```
→
```rust
    handlers: Mutex<Vec<RegisteredHandler>>,
```

And update `bind_core`'s handler-drain loop (~lines 132-134):

```rust
        for ph in pending.handlers.drain(..) {
            handlers.push(ph.handler);
        }
```
→
```rust
        for ph in pending.handlers.drain(..) {
            handlers.push(RegisteredHandler {
                handler: ph.handler,
                kind_filter: ph.kind_filter,
            });
        }
```

(`RegisteredHandler` was defined in T6 step 3b.)

- [ ] **Step 4: Run build + existing tests to verify green**

Run: `cargo +1.90.0 build -p codesmith-extensions` && `cargo +1.90.0 test -p codesmith-extensions --lib`
Expected: build green; tests green (the `RecHandler`-based `emit_fans_out_to_bound_handler` still passes — `emit` signature unchanged in T7; the field-type change is internal). NOTE: `emit` still takes `&ExtensionEvent` in T7 — the signature change is T8. So `emit_fans_out_to_bound_handler` calls `runner.emit(&ExtensionEvent::TurnStart{...})` — still valid in T7.

- [ ] **Step 5: Commit**

```bash
git add crates/extensions/src/runner.rs
git commit -m "feat(framework): §F2a T7 ExtensionRunner.handlers Vec<RegisteredHandler> + bind_core drain kind_filter"
```

---

## Task 8: `EmitOutcome` + `emit` rewrite (owned, chained, per-variant, `catch_unwind`) + 5 tests + host_executor 7-site signature update

**Files:**
- Modify: `crates/extensions/src/runner.rs` — add `EmitOutcome`, rewrite `emit`, update existing 2 test emit-calls, add 5 new tests
- Modify: `crates/extensions/src/lib.rs` — re-export `EmitOutcome`
- Modify: `crates/agent-runtime/src/engine/host_executor.rs` — 7 emit sites drop `&`

- [ ] **Step 1: Write the 5 failing tests**

Append to the test block in `crates/extensions/src/runner.rs`. First add the test imports + helpers at the top of the `#[cfg(test)] mod tests` block (after `use super::*;`):

```rust
    use codesmith_tools::ToolResult;
    use serde_json::json;
```

Then append these test structs + tests:

```rust
    // === §F2a emit chaining / per-variant / catch_unwind ====================

    /// An extension that registers two handlers in deterministic order
    /// (first, then second) so chain ordering is observable.
    struct TwoHandlerExt {
        first: Arc<dyn Handler>,
        second: Arc<dyn Handler>,
    }
    #[async_trait::async_trait]
    impl Extension for TwoHandlerExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("two");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.on(self.first.clone())?;
            api.on(self.second.clone())?;
            Ok(())
        }
    }

    /// Transforms a `ToolResult` event's result to a fixed success string.
    struct TransformResultHandler;
    #[async_trait::async_trait]
    impl Handler for TransformResultHandler {
        async fn handle(
            &self,
            event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            if let ExtensionEvent::ToolResult(tr) = event {
                let mut tr = tr.clone();
                tr.result = Ok(ToolResult::success("transformed"));
                Ok(HandlerOutcome::Transform(ExtensionEvent::ToolResult(tr)))
            } else {
                Ok(HandlerOutcome::Continue)
            }
        }
    }

    /// Records the `ToolResult` content string it observes.
    struct ObserveResultHandler {
        seen: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl Handler for ObserveResultHandler {
        async fn handle(
            &self,
            event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            if let ExtensionEvent::ToolResult(tr) = event {
                if let Ok(r) = &tr.result {
                    self.seen.lock().unwrap().push(r.content.clone());
                }
            }
            Ok(HandlerOutcome::Continue)
        }
    }

    #[tokio::test]
    async fn f2a_emit_chains_transform_to_next_handler() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let transform: Arc<dyn Handler> = Arc::new(TransformResultHandler);
        let observer: Arc<dyn Handler> = Arc::new(ObserveResultHandler { seen: seen.clone() });
        runner
            .load(&TwoHandlerExt { first: transform, second: observer })
            .await
            .unwrap();
        runner.bind_core(Arc::new(Ctx { generation: 1 }));

        let original = ExtensionEvent::ToolResult(ToolResultEvent {
            id: "c1".into(),
            name: "echo".into(),
            result: Ok(ToolResult::success("original")),
        });
        let out = runner.emit(original).await;
        // Observer saw the TRANSFORMED result (transform visible to next handler).
        assert_eq!(*seen.lock().unwrap(), vec!["transformed".to_string()]);
        // Final event carries the transformed result.
        match out.event {
            ExtensionEvent::ToolResult(tr) => {
                let r = tr.result.expect("ok result");
                assert_eq!(r.content, "transformed");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
        // Transform folds in — terminal outcome is Continue.
        assert!(matches!(out.outcome, HandlerOutcome::Continue));
    }

    struct CancelOnBeforeCompact;
    #[async_trait::async_trait]
    impl Handler for CancelOnBeforeCompact {
        async fn handle(
            &self,
            event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            if matches!(event, ExtensionEvent::SessionBeforeCompact) {
                Ok(HandlerOutcome::Cancel { reason: "user aborted".into() })
            } else {
                Ok(HandlerOutcome::Continue)
            }
        }
    }

    #[tokio::test]
    async fn f2a_emit_cancel_short_circuits_chain() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        // Cancel first; the second observer must NOT fire (cancel short-circuits).
        runner
            .load(&TwoHandlerExt {
                first: Arc::new(CancelOnBeforeCompact),
                second: Arc::new(RecHandler { seen: seen.clone() }),
            })
            .await
            .unwrap();
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let out = runner.emit(ExtensionEvent::SessionBeforeCompact).await;
        assert!(matches!(out.outcome, HandlerOutcome::Cancel { .. }));
        // The observer after the cancel handler never fired.
        assert!(seen.lock().unwrap().is_empty());
    }

    struct BlockOnToolCall;
    #[async_trait::async_trait]
    impl Handler for BlockOnToolCall {
        async fn handle(
            &self,
            event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            if matches!(event, ExtensionEvent::ToolCall(_)) {
                Ok(HandlerOutcome::Block { reason: "policy".into() })
            } else {
                Ok(HandlerOutcome::Continue)
            }
        }
    }

    #[tokio::test]
    async fn f2a_emit_block_short_circuits_chain() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        runner
            .load(&TwoHandlerExt {
                first: Arc::new(BlockOnToolCall),
                second: Arc::new(RecHandler { seen: seen.clone() }),
            })
            .await
            .unwrap();
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let out = runner
            .emit(ExtensionEvent::ToolCall(ToolCallEvent {
                id: "c1".into(),
                name: "echo".into(),
                input: json!({}),
            }))
            .await;
        assert!(matches!(out.outcome, HandlerOutcome::Block { .. }));
        assert!(seen.lock().unwrap().is_empty());
    }

    /// An extension that subscribes a handler to ToolCall ONLY (per-variant).
    struct VariantExt {
        seen: Arc<Mutex<Vec<&'static str>>>,
    }
    #[async_trait::async_trait]
    impl Extension for VariantExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("var");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.on_variant(
                ExtensionEventKind::ToolCall,
                Arc::new(RecHandler { seen: self.seen.clone() }),
            )?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn f2a_on_variant_dispatches_only_matching_kind() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        runner.load(&VariantExt { seen: seen.clone() }).await.unwrap();
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        // ToolCall fires the per-variant handler (RecHandler pushes "other").
        runner
            .emit(ExtensionEvent::ToolCall(ToolCallEvent {
                id: "c1".into(),
                name: "echo".into(),
                input: json!({}),
            }))
            .await;
        // TurnStart does NOT fire the per-variant handler.
        runner
            .emit(ExtensionEvent::TurnStart { turn_id: "t1".into() })
            .await;
        let s = seen.lock().unwrap();
        assert_eq!(*s, vec!["other"]);
    }

    struct PanickingHandler;
    #[async_trait::async_trait]
    impl Handler for PanickingHandler {
        async fn handle(
            &self,
            _event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            panic!("boom");
        }
    }

    #[test]
    fn f2a_emit_catch_unwind_isolates_panicking_handler() {
        // Dedicated multi-thread runtime (NOT #[tokio::test]) to avoid
        // current-thread panic-abort subtleties + nested-runtime panics
        // (the same lesson as §F1's configure-runtime discovery).
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("multi-thread runtime for catch_unwind test");
        rt.block_on(async {
            let runner = ExtensionRunner::new();
            let seen = Arc::new(Mutex::new(Vec::new()));
            // Panicking handler first; the second observer must STILL fire
            // (catch_unwind isolates the panic).
            runner
                .load(&TwoHandlerExt {
                    first: Arc::new(PanickingHandler),
                    second: Arc::new(RecHandler { seen: seen.clone() }),
                })
                .await
                .unwrap();
            runner.bind_core(Arc::new(Ctx { generation: 1 }));
            let out = runner
                .emit(ExtensionEvent::TurnStart { turn_id: "t1".into() })
                .await;
            // Observer after the panicking handler still fired.
            assert_eq!(*seen.lock().unwrap(), vec!["TurnStart"]);
            // Chain continued past the panic → Continue.
            assert!(matches!(out.outcome, HandlerOutcome::Continue));
        });
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo +1.90.0 test -p codesmith-extensions --lib f2a`
Expected: FAIL — `emit` takes `&ExtensionEvent` (not owned), returns `()` (not `EmitOutcome`), and the chaining/catch_unwind behavior doesn't exist.

- [ ] **Step 3: Write minimal implementation — `EmitOutcome` + rewrite `emit`**

3a. At the top of `crates/extensions/src/runner.rs`, add the imports (after the existing `use` block, ~line 17):

```rust
use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;
```

3b. Add `EmitOutcome` after the `RegisteredHandler` struct (defined in T6):

```rust
/// The result of [`ExtensionRunner::emit`]: the final (possibly transformed)
/// event + the terminal chain outcome. The host inspects `outcome` at each
/// seam (proceed / cancel / block) and, at transform-capable seams, applies
/// `event`'s actionable field (§F2b wires the host to honor these; §F2a
/// returns them + proves the chain in isolation).
///
/// NOT `#[must_use]` in §F2a so the mechanical 7-site host_executor update is
/// just dropping the `&`; §F2b may add `#[must_use]` to force inspection at
/// transform/block seams.
#[derive(Debug, Clone)]
pub struct EmitOutcome {
    /// The event after all handlers (possibly transformed by `Transform`).
    pub event: ExtensionEvent,
    /// Terminal outcome: `Continue` if no handler short-circuited; `Cancel`
    /// or `Block` if one did. Never `Transform` (folds into `event`).
    pub outcome: HandlerOutcome,
}
```

3c. Replace the `emit` method (~lines 137-151):

```rust
    /// Emit an event to every bound handler, best-effort. A handler error
    /// is discarded (§8.3 — one failing handler does not block others).
    /// No-op if `bind_core` has not run. Slice 1 awaits each handler
    /// directly; §F2 hardens with `catch_unwind`.
    pub async fn emit(&self, event: &ExtensionEvent) {
        let ctx = match self.context.lock().unwrap().clone() {
            Some(ctx) => ctx,
            None => return,
        };
        let handlers = self.handlers.lock().unwrap().clone();
        for h in handlers {
            // Slice 1: discard errors (best-effort); §F2 adds catch_unwind.
            let _ = h.handle(event, &*ctx).await;
        }
    }
```

with:

```rust
    /// Emit `event` to every bound handler whose variant filter matches,
    /// chaining transforms (spec §4: "一个 handler 的修改对下一个可见").
    /// Returns [`EmitOutcome`] — the final (possibly transformed) event +
    /// the terminal outcome (`Continue` / `Cancel` / `Block`). Each handler
    /// call is wrapped in `catch_unwind` (§8.3) so a panicking handler cannot
    /// tear down the agent loop; its panic is recorded via `tracing` and the
    /// chain continues. A handler `Err` (non-panic) is likewise recorded +
    /// the chain continues (best-effort, §8.3). `Cancel` / `Block`
    /// short-circuit: no further handlers run. `Transform(new)` replaces the
    /// running event and the chain continues with the next handler.
    /// No-op (returns the input event + `Continue`) if `bind_core` has not
    /// run.
    pub async fn emit(&self, event: ExtensionEvent) -> EmitOutcome {
        let ctx = match self.context.lock().unwrap().clone() {
            Some(ctx) => ctx,
            None => return EmitOutcome { event, outcome: HandlerOutcome::Continue },
        };
        // Snapshot the matching handlers under a short lock; dispatch outside
        // the lock so a long-running handler doesn't hold it.
        let kind = event.kind();
        let matching: Vec<Arc<dyn Handler>> = self
            .handlers
            .lock()
            .unwrap()
            .iter()
            .filter(|rh| rh.kind_filter.is_none() || rh.kind_filter == Some(kind))
            .map(|rh| Arc::clone(&rh.handler))
            .collect();
        let mut event = event;
        let mut outcome = HandlerOutcome::Continue;
        for h in matching {
            // §F2a: catch_unwind so a panicking handler can't tear down the
            // agent loop (§8.3 error isolation).
            let result = AssertUnwindSafe(h.handle(&event, &*ctx))
                .catch_unwind()
                .await;
            match result {
                // Panic → record + continue (one handler can't break the chain).
                Err(panic) => {
                    tracing::error!(
                        target: "codesmith_extensions::runner",
                        "extension handler panicked: {panic:?}",
                    );
                    continue;
                }
                // Handler error → record + continue (best-effort, §8.3).
                Ok(Err(err)) => {
                    tracing::error!(
                        target: "codesmith_extensions::runner",
                        "extension handler error: {err}",
                    );
                    continue;
                }
                Ok(Ok(HandlerOutcome::Continue)) => continue,
                Ok(Ok(HandlerOutcome::Transform(new_event))) => {
                    event = new_event;
                    continue;
                }
                // Cancel / Block short-circuit the chain.
                Ok(Ok(c @ HandlerOutcome::Cancel { .. })) => {
                    outcome = c;
                    break;
                }
                Ok(Ok(b @ HandlerOutcome::Block { .. })) => {
                    outcome = b;
                    break;
                }
            }
        }
        EmitOutcome { event, outcome }
    }
```

3d. Update the existing `emit_fans_out_to_bound_handler` test's two emit calls (~lines 263-265) — drop the `&`:

```rust
        runner
            .emit(&ExtensionEvent::TurnStart { turn_id: "t1".into() })
            .await;
        runner.emit(&ExtensionEvent::SessionShutdown).await;
```
→
```rust
        runner
            .emit(ExtensionEvent::TurnStart { turn_id: "t1".into() })
            .await;
        runner.emit(ExtensionEvent::SessionShutdown).await;
```

And the `emit_before_bind_core_is_noop` test's emit call (~line 276):

```rust
        runner.emit(&ExtensionEvent::SessionShutdown).await;
```
→
```rust
        runner.emit(ExtensionEvent::SessionShutdown).await;
```

- [ ] **Step 4: Re-export `EmitOutcome` from `codesmith-extensions`**

In `crates/extensions/src/lib.rs`, update the runner re-export (~line 48):

```rust
pub use runner::ExtensionRunner;
```
→
```rust
pub use runner::{EmitOutcome, ExtensionRunner};
```

- [ ] **Step 5: Update the 7 `host_executor.rs` emit sites (mechanical — drop the `&`)**

In `crates/agent-runtime/src/engine/host_executor.rs`, all 7 emit sites use the identical substring `.emit(&codesmith_agent::extension::ExtensionEvent`. Use a single `replace_all` Edit:

old_string:
```
.emit(&codesmith_agent::extension::ExtensionEvent
```
new_string:
```
.emit(codesmith_agent::extension::ExtensionEvent
```
`replace_all: true`

This replaces all 7 occurrences (TurnStart `:3735`, TurnEnd-Interrupted `:3783`, TurnEnd-NoToolCalls `:4267`, ToolCall-parallel `:4389`, ToolResult-parallel `:4477`, ToolCall-serial `:4495`, ToolResult-serial `:4590`). The sites discard `EmitOutcome` as a statement (no `let _ =` needed since `EmitOutcome` is not `#[must_use]`).

> Verify the count after the edit: `grep -c "\.emit(codesmith_agent::extension::ExtensionEvent" crates/agent-runtime/src/engine/host_executor.rs` should print `7`; `grep -c "\.emit(&codesmith_agent::extension::ExtensionEvent" ...` should print `0`.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo +1.90.0 test -p codesmith-extensions --lib` && `cargo +1.90.0 test -p codesmith-agent-runtime --lib`
Expected:
- `codesmith-extensions --lib`: the 5 new `f2a_*` tests pass + existing `emit_fans_out_to_bound_handler` / `emit_before_bind_core_is_noop` / `stale_context_guard_*` green. (Count: 8 → 13.)
- `codesmith-agent-runtime --lib`: the existing `extension_runner_bound_emits_lifecycle_events_on_minimal_run` round-trip still passes (1152 + 2 ignored) — the 7 emit-site signature change is behavior-preserving (the host discards `EmitOutcome`; the handler still observes `[TurnStart, ToolCall, ToolResult, TurnEnd]`).

- [ ] **Step 7: Commit**

```bash
git add crates/extensions/src/runner.rs crates/extensions/src/lib.rs crates/agent-runtime/src/engine/host_executor.rs
git commit -m "feat(framework): §F2a T8 EmitOutcome + chained per-variant emit (catch_unwind) + 5 runner tests + 7 host_executor signature sites"
```

---

## Task 9: Docs — ROADMAP §F2a entry + ARCHITECTURE §F status row + EXTENSIONS.md update

**Files:**
- Modify: `ROADMAP.md` (§F section + add §F2a progress entry after the §F1 entry ~line 2451)
- Modify: `ARCHITECTURE.md` (§F status table row)
- Modify: `docs/EXTENSIONS.md` (HandlerOutcome + per-variant + catch_unwind)

- [ ] **Step 1: ROADMAP §F2a progress entry**

In `ROADMAP.md`, after the §F1 "下一聚焦工作" bullet (~line 2451, the line starting `- §F2：完整事件集...`), insert a new §F2a progress entry ABOVE it (so §F2a is the latest done slice). Mirror the §F1 entry's shape (date, scope, key design decisions, landed steps, tests/verification, by-design gaps, 下一聚焦工作). Key points to capture:

- **Scope:** §F2a = contract + runtime core (full 23-variant `ExtensionEvent` set + `HandlerOutcome` cancel/transform/block + per-variant `on_variant` subscription + `catch_unwind` isolation in `codesmith-agent`/`codesmith-extensions` + mechanical 7-site `host_executor` emit-signature update). §F2b (host seam wiring + e2e + App live wiring + reload re-discover) explicitly out of scope.
- **Key design decisions:** (1) handler trait shape = Approach A (single dyn-safe `Handler`, `Arc<dyn Handler>` unchanged, return `Result<HandlerOutcome>`; per-variant via `kind_filter` not per-variant trait — smallest delta, matches pi-mono single-handler/union-return model, object-safe); (2) `HandlerOutcome` flat enum `Continue`/`Cancel`/`Block`/`Transform`, variant-specific semantics enforced at runtime by host (§F2b) not type system; terminal `EmitOutcome.outcome` never `Transform` (folds into event); (3) `emit` owned-in/`EmitOutcome`-out, chained, registration-order; `Transform` visible to next handler, `Cancel`/`Block` short-circuit; (4) per-variant = single ordered `Vec<RegisteredHandler{handler, kind_filter}>`, global registration order preserved; (5) `catch_unwind` via `futures-util` (matches `codesmith-agent-runtime` version); (6) §F2a/§F2b split so contract lands + stabilizes before host wiring.
- **Landed steps:** T1 reason enums + payload structs; T2 17 new variants + `ExtensionEventKind`/`kind()` (23 total); T3 `HandlerOutcome`; T4 `Handler::handle` return change + 5 in-tree handlers → `Continue`; T5 `futures-util` dep; T6 `on_variant` + `kind_filter` + Stub/Real; T7 `handlers: Vec<RegisteredHandler>` + `bind_core` drain; T8 `EmitOutcome` + `emit` rewrite + 5 runner tests + 7 host_executor signature sites; T9 docs.
- **Tests/verification:** `cargo +1.90.0 build --workspace` green; `codesmith-extensions --lib` 8 → 13 (5 new `f2a_*`); `codesmith-agent --lib` 93 → 95 (2 new `f2a_*` contract tests); `codesmith-agent-runtime --lib` 1152 + 2 ignored (round-trip regression green); `codesmith-tui --bin` 2853 + 2 ignored; grep `\.emit(&codesmith_agent::extension::ExtensionEvent` 0-hit; grep "observer-only" stale refs in extension.rs doc comments updated.
- **By-design gaps (§F2b):** host_executor cancel/block/transform *handling* at the 7 seams (currently discards `EmitOutcome`); the ~15 new events not yet emitted by `host_executor` (only the §F1 6 are live); full e2e round-trip asserting the complete ordered event sequence; App field live wiring; `/extension reload` re-discover (currently only `invalidate()`).
- **下一聚焦工作:** §F2b — wire every new event to its `HostAgentExecutor`/app seam + full e2e round-trip test + App field live wiring + `/extension reload` re-discover.

Update the §F section status line (~line 2783-2784) if it says "slice 1" to note §F2a done; keep §F2b as next.

- [ ] **Step 2: ARCHITECTURE.md §F status row**

Update the §F status table row to reflect §F2a done (full event set + HandlerOutcome + per-variant + catch_unwind landed; host wiring = §F2b). Match the existing row shape.

- [ ] **Step 3: EXTENSIONS.md update**

In `docs/EXTENSIONS.md`, update the handler section to document: `HandlerOutcome` (Continue/Cancel/Block/Transform with variant-specific semantics), `on` vs `on_variant` (subscribe-to-all vs per-variant), and the `catch_unwind` isolation guarantee (a panicking handler is logged + skipped, does not crash the agent). Add a short example of a per-variant cancel handler. Cross-reference the §F1 "observer-only" note as superseded by §F2a.

- [ ] **Step 4: Commit**

```bash
git add ROADMAP.md ARCHITECTURE.md docs/EXTENSIONS.md
git commit -m "feat(framework): §F2a T9 docs (ROADMAP §F2a entry + ARCHITECTURE status + EXTENSIONS HandlerOutcome/per-variant/catch_unwind)"
```

---

## Task 10: Verification gate

**Files:** none (verification only)

- [ ] **Step 1: Full build**

Run: `cargo +1.90.0 build --workspace`
Expected: green (142 tui warnings = slice-47 baseline, non-new).

- [ ] **Step 2: Four test suites**

Run:
```bash
cargo +1.90.0 test -p codesmith-extensions --lib
cargo +1.90.0 test -p codesmith-agent --lib
cargo +1.90.0 test -p codesmith-agent-runtime --lib
cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui
```
Expected:
- `codesmith-extensions --lib`: 13 passed (8 + 5 new `f2a_*`); 0 failed.
- `codesmith-agent --lib`: 95 passed (93 + 2 new `f2a_*`); 0 failed.
- `codesmith-agent-runtime --lib`: 1152 passed + 2 ignored (unchanged — round-trip regression green).
- `codesmith-tui --bin`: 2853 passed + 2 ignored (unchanged).

- [ ] **Step 3: grep verifications**

Run:
```bash
# No stale §F1 "observer-only" return references left in the Handler trait doc.
grep -n "observer-only" crates/agent/src/extension.rs
# Expected: only historical/contextual mentions, NOT the Handler trait's
# current contract (should say "§F2a: returns HandlerOutcome").

# All 7 host_executor emit sites use the new owned signature.
grep -c "\.emit(codesmith_agent::extension::ExtensionEvent" crates/agent-runtime/src/engine/host_executor.rs
# Expected: 7
grep -c "\.emit(&codesmith_agent::extension::ExtensionEvent" crates/agent-runtime/src/engine/host_executor.rs
# Expected: 0

# EmitOutcome re-exported.
grep -n "EmitOutcome" crates/extensions/src/lib.rs
# Expected: a re-export line.

# futures-util dep present.
grep -n "futures-util" crates/extensions/Cargo.toml
# Expected: 1 hit.
```

- [ ] **Step 4: Final commit (if any drift) + status report**

Report the green gate + the 5 new `f2a_*` tests + the unchanged agent-runtime/tui baselines. If all green, §F2a is done; §F2b is the next slice (design pass first).

---

## Self-review (writing-plans checklist)

**1. Spec coverage:** spec §F2 scope (todo.md 6 items) → §F2a covers: full event set (T1-T2), HandlerOutcome (T3-T4), per-variant subscription (T6-T7), catch_unwind (T8), runner-isolated tests (T8). The 6th item (e2e round-trip + App live wiring + reload re-discover) is explicitly §F2b (user-approved split, recorded in design decision #6 + Task 9's by-design gaps). No spec requirement missed for §F2a.

**2. Placeholder scan:** No "TBD"/"TODO"/"implement later". All code steps show complete code. T9 (docs) describes the entry shape + exact points to capture (the doc content itself is prose, not code — acceptable; the bullet list of key points IS the content). T7 step 1's test is deliberately deferred to T8 with a stated reason (compile-ordering: the test needs T8's `emit` signature) — flagged inline, not a placeholder.

**3. Type consistency:** `HandlerOutcome` variants (`Continue`/`Cancel{reason}`/`Block{reason}`/`Transform(ExtensionEvent)`) used identically in T3 (definition), T4 (handlers return `Continue`), T8 (emit match arms + tests). `EmitOutcome { event, outcome }` consistent in T8 definition + tests (`out.event`, `out.outcome`). `RegisteredHandler { handler, kind_filter }` consistent T6 (def) → T7 (field) → T8 (emit filter). `ExtensionEventKind` consistent T2 (def) → T6 (`on_variant` param + test) → T8 (`VariantExt`). `emit` signature `emit(&self, event: ExtensionEvent) -> EmitOutcome` consistent T8 (def) + host_executor sites (drop `&`). `PendingHandler { handler, kind_filter }` consistent T6 (def + Stub/Real push).

**Compile-ordering note (for the executor):** Tasks 1-5 are independently green. T6 defines `RegisteredHandler` (needed by `RealExtensionApi`'s `on`/`on_variant` push) — done in T6 step 3b as noted. T7 changes the `handlers` field type + `bind_core` drain; `emit` signature is unchanged in T7 (still `&ExtensionEvent`) so existing tests stay green. T8 changes `emit` signature + updates the 2 existing runner.rs test emit-calls + 7 host_executor sites in one task — green after T8. If executing task-by-task, the workspace build is red only mid-T8 (between the `emit` rewrite and the host_executor `replace_all`); the gate (T10) runs after T8 is complete.
