# Architecture — pluggable framework core

This document describes the **provider pluggability** layer: how the CodeSmith
stack separates LLM *abstraction* from *implementation*, and how a host
assembles providers like Lego blocks at build time.

For a gentler whole-codebase overview, see [ARCHITECTURE.md](ARCHITECTURE.md).
For planned work, see [`ROADMAP.md`](../ROADMAP.md).

## Design goals

1. **Framework core, LangChain-style** — a small set of traits (`LlmClient`,
   `ProviderFactory`) and a registry that any provider can plug into, with no
   concrete client compiled into the core.
2. **Abstraction / implementation split, pi-mono-style** — the host never names
   a concrete client type; it builds a neutral `ProviderConfig` and asks a
   `ProviderRegistry` for a client. Developers can replace any implementation
   by registering a different factory.
3. **Lego blocks at install time** — providers live behind Cargo features in a
   separate `codesmith-providers` crate; a host pulls in only what it needs.

## Crate layering

```
                         ┌───────────────────────────┐
                         │ codesmith-config           │  ProviderKind, config TOML
                         │ codesmith-secrets          │  key resolution
                         └─────────────┬─────────────┘
                                       │ dep
              ┌────────────────────────┴────────────────────────┐
              ▼                                                  ▼
┌──────────────────────────────┐                ┌─────────────────────────────┐
│ codesmith-agent (CORE)       │                │ codesmith-providers (IMPLS) │
│  • llm_client::LlmClient     │   traits ─────▶│  • mock (echo, no network)   │
│  • provider::{ProviderId,    │   ◀──── cfg    │  • rig factories: openai /   │
│      ProviderConfig,         │     features   │    anthropic / deepseek /    │
│      ProviderFactory,        │                │    openai-compat ×13         │
│      ProviderRegistry}       │                │    (catalog: providers.toml) │
│  • models, retry             │                └─────────────────────────────┘
└──────────────┬───────────────┘                            ▲
               │ path dep                                    │ path dep
               ▼                                             │
┌──────────────────────────────┐                            │
│ codesmith-agent-runtime      │                            │
│  • Engine, prompt_runtime,   │                            │
│    retry_status, config_types│                            │
└──────────────┬───────────────┘                            │
               │ path dep                                    │
               ▼                                             │
┌──────────────────────────────────────────────────────────┐ │
│ codesmith-tui  (HOST / binary)                            │─┘ (optional)
│  • build_engine → resolve_llm_client → registry.build     │
│  • Config, logging, retry_status (UI globals)             │
└───────────────────────────────────────────────────────────┘
```

The arrow that matters: **`codesmith-tui` depends on `codesmith-providers`
(optional, feature-gated), never the reverse.** Providers depend only on
`codesmith-agent` and `codesmith-config` (the declarative `providers.toml`
schema and loader).

## The provider seam

A client is built without the host naming a concrete type:

```text
  Host (tui)                      codesmith-agent                  codesmith-providers
  ─────────                       ──────────────                   ───────────────────
  Config ──▶ resolve_llm_client
                 │ builds ProviderConfig
                 │ (6 neutral fields + on_retry)
                 ▼
               ProviderRegistry::build(&cfg)
                 │ resolves factory by cfg.provider
                 ▼
               ProviderFactory::build(&cfg) ───────▶ MockClient / RigLlmClient / ...
                 │
                 ▼
               LlmClientHandle (Arc<dyn LlmClient>)
```

- **`ProviderId`** — open union: `Builtin(ProviderKind)` for known providers,
  `Custom(String)` for anything else. Mirrors pi-ai's `KnownProvider | string`.
- **`ProviderConfig`** — neutral construction input (`api_key`, `base_url`,
  `default_model`, `retry`, `http_headers`, `on_retry`). No TUI `Config`
  dependency, so a provider crate stays host-agnostic.
- **`ProviderFactory`** — `id()` + `build(&cfg) -> LlmClientHandle`. Implement
  in `codesmith-providers` (or your own crate) and register it.
- **`ProviderRegistry`** — `HashMap<ProviderId, Arc<dyn ProviderFactory>>`.
  `register` upserts (last wins, like pi-ai's `setProvider`); `build` resolves
  and delegates, erroring with the registered ids if none match.

## The framework-core agent seam

The provider seam above is the first LangChain analog. The core also carries a
fuller agent framework with four host-agnostic traits that mirror LangChain's
`BaseTool` / `Memory` / `Callbacks` / `AgentExecutor`. They live in
`codesmith-agent` so any provider or host can drive an agent loop without
depending on `codesmith-agent-runtime`'s production `Engine`.

```text
  Host                             codesmith-agent (CORE)
  ────                             ──────────────────────
  Arc<dyn AgentExecutor> ◀── built from LlmClientHandle + Arc<ToolSet> + Arc<dyn Callback>
        │
        ▼  AgentExecutor::run(&mut dyn ChatHistory, user_text)
   ┌────┴────────────────────────────────────────────────────┐
   │ DefaultAgentExecutor loop (cap = config.max_steps):      │
   │   build MessageRequest from ChatHistory + ToolSet        │
   │   ▶ Callback::on_llm_start  → LlmClient::create_message  │
   │                                _stream                   │
   │   ▶ accumulate StreamEvent → Vec<ContentBlock>           │
   │   ▶ Callback::on_llm_end    → push assistant Message     │
   │   extract ContentBlock::ToolUse{ id, name, input }       │
   │   if none → Callback::on_complete(NoToolCalls); return   │
   │   for each tool_use:                                     │
   │     ▶ Callback::on_tool_start → Tool::run(input)         │
   │     ▶ Callback::on_tool_end   → push ToolResult Message  │
   │   ▶ Callback::on_step; if step+1 >= max_steps → return  │
   └──────────────────────────────────────────────────────────┘
```

- **`Tool`** (`tools::Tool`) — the executable tool contract (LangChain
  `BaseTool` analog). Host-agnostic: each impl owns its dependencies and
  `tools::Tool::run` takes only a parsed `input` — there is **no fat
  per-call `ToolContext`** in the core (that lives in
  `codesmith-agent-runtime::tools::spec`). The bridge onto the production
  `ToolSpec`+`ToolContext` is `ToolSpecAdapter` (in
  `codesmith-agent-runtime::tools::framework_adapter`): it captures a
  shared `ToolContext` and delegates `run` → `ToolSpec::execute`. The wire
  definition sent to the model is the separate `models::Tool`; `ToolSet`
  converts executable → wire via `to_api_tools()`.
- **`ChatHistory`** (`memory::ChatHistory`) — the transcript view (LangChain
  `Memory` analog): `messages` / `push` / `clear`. `VecChatHistory` is the
  in-memory default; the host backs it with its `Session` via `SessionChatHistory`
  (in `codesmith-agent-runtime::session_history`).
- **`Callback`** (`callback::Callback`) — observation hooks (LangChain
  `Callbacks` analog): `on_llm_start` / `on_llm_end` / `on_tool_start` /
  `on_tool_end` / `on_step` / `on_complete`, all default no-ops. `CallbackSet`
  fans out to several observers; `NoopCallback` is the default. The bridge onto
  the host's `Event` UI channel + `HookHost` shell hooks is `CallbackBridge`
  (in `codesmith-agent-runtime::callback_bridge`): it forwards the
  tool-lifecycle hooks onto both paths; the LLM/step/complete hooks are
  documented no-ops (the production caller and stream-reduction code own those).
- **`AgentExecutor`** (`executor::AgentExecutor`) — drives the loop;
  `DefaultAgentExecutor` is the reference impl (core). The host-side
  `HostAgentExecutor` (in `codesmith-agent-runtime::engine::host_executor`)
  mirrors the bare loop over the three bridges and carries the production
  guardrails — **ten** in all (see the `host_executor.rs` module doc,
  "Absorbed guardrails", for the full list):

  - **loop-guard** — block the 3rd identical call, warn/halt on 3/8
    consecutive failures; per-tool / post-tool seams.
  - **LSP flush** — collect diagnostics per successful edit, flush them as a
    user message before the next request; per-tool / per-step pre-request
    seams.
  - **transparent-retry** — re-issue the request when the stream dies
    mid-flight before any content commits, up to 3 times, resetting the
    budget on a healthy round; per-step post-stream seam.
  - **steer** — drain queued user inputs as `user` messages before the next
    request; per-step pre-request seam.
  - **approval** — gate write/code-exec tools behind user permission: emit
    `ApprovalRequired` + block on the decision channel by wire tool id;
    denied ⇒ `permission_denied` error, tool skipped; per-tool seam. Static
    derivation is from `Tool::capabilities`; per-input override and sandbox
    elevation are deferred (see the `host_executor.rs` module doc).
  - **compaction** — micro-compact stale tool results past the 32KB cache
    trigger without an LLM call, then auto-compact via an LLM summary when
    `should_compact` passes; both wholesale-replace via `clear()`+`push()`;
    the summary path merges the summary prompt, re-injects attachments, and
    runs post-compact cleanup; per-step pre-request seam. Enhancements and
    working-set pins are deferred (see the `host_executor.rs` module doc).
  - **capacity**, **subagent** (completion hold + sentinel),
    **early-tool-start**, and the **cycle** guardrail.

The per-step machinery is split by phase into private submodules under
`engine/turn/` (`stream.rs`, `batches.rs`, `approval.rs`, `seams.rs`,
`postprocess.rs`, `truncation.rs`); `host_executor.rs` keeps the step loop
itself plus the cross-cutting guardrails it owns directly.

**Interior-mutability slices.** The LSP accumulator, the steer receiver, the
approval receiver, and the compaction probe are the interior-mutability
slices: `AgentExecutor::run` is `&self` while the accumulator mutates on
collect/flush and `try_recv`/`recv` take `&mut self`. `LspProbe.pending` is
`Arc<std::sync::Mutex<Vec<DiagnosticBlock>>>` (lock never held across an
`await`, matching `CallbackBridge`).

`steer` is `Option<Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>>` —
`tokio::sync::Mutex` (not `std`) so the guard may cross the blocking
`recv().await` in the subagent blocking hold's `biased select!` steer arm
(same rationale as `approval`; the pre-request `try_recv` drain is
non-blocking and uncontended — single consumer — so the tokio mutex is a
no-cost upgrade there).

These slices persist across `run` calls, so diagnostics from an edit on a
turn ending via `MaxSteps` surface on the next turn's first flush, and a
steer queued between turns is picked up on the next turn's first drain.
`approval` uses `Option<Arc<tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>>>`
because the guard must cross the blocking `recv().await` (a std mutex guard
isn't `Send`).

**Compaction state.** `compaction` carries `micro_state:
Arc<std::sync::Mutex<MicroCompactState>>` and `circuit_breaker:
Arc<std::sync::Mutex<CompactionCircuitBreaker>>` (no lock crosses an `await`
— messages are cloned out before the async `compact_messages_safe` call). It
persists across `run` calls so a failed compaction on turn N still trips the
breaker on turn N+1 (matching `Engine.micro_compact_state` /
`.compaction_circuit_breaker`).

**Session fact ledger.** Both the compaction and capacity probes also carry
the session fact ledger (`Session::fact_ledger`, `compaction/fact_ledger.rs`):
every compaction's drop set feeds it rule-extracted must-not-lose facts (task
constraints, key paths, failure causes), its rendered section rides every
compaction summary and the cycle-reset seed, and the first task instruction
is pinned verbatim by `plan_compaction` when under
`TASK_INSTRUCTION_PIN_TOKEN_CAP`.

The ledger is also the reflection loop's store: the layered summary's
"Refuted Assumptions & Invariants" section — lessons the model derives from
its own failures — is parsed back into the ledger as `RefutedAssumption`
entries, so a learned invariant outlives the summary that carried it (no
external playbook knowledge involved).

**Deliverables watchdog.** A separate deliverables watchdog
(`engine/deliverables.rs`, `DeliverablesProbe` on the executor) shares the
pre-request seam: the output paths parsed from the task instruction are
re-checked on disk every `DELIVERABLES_CHECK_CADENCE_STEPS` steps and a
missing one is pushed back as a `<codesmith:runtime_event
kind="deliverables_check">` user message, so a missing deliverable surfaces
mid-run instead of at grading. Compaction summaries also pass a
layered-section gate (`summary_section_count`): a flat draw is retried once.

transparent-retry reuses the local-state pattern (a per-run `u32` counter,
matching loop-guard). Guardrail status surfaces over the host's `Event`
channel (`event_tx`), not the `Callback`. `StopReason` (`NoToolCalls` /
`MaxSteps` / `Error`) is the terminal outcome.

**Known gaps in the LSP flush (by design):** `apply_patch` path derivation is
deferred (needs `HostServices::preflight_apply_patch_paths`, unreachable from
`agent-runtime` without the heavy host trait); the synthetic flush message
carries no `<turn_meta>` enrichment (the framework path has no turn_meta
anywhere yet — cross-cutting host-side concern, deferred to its own slice);
no `emit_session_updated` for the push (consistent with the executor's other
message pushes; UI surfacing deferred to the wire-in step).

**Known tradeoffs in transparent-retry (by design):** `accumulate_stream`
bails on the first erroring stream item and drops partial blocks, so the
retry fires even when the engine would otherwise ship partial content (it
tracks `any_content_received` inline) — since the partial content is lost,
retrying is the only recovery path; the planned inline stream reduction that
replaces `accumulate_stream` closes the gap. Pre-stream connection errors
(`create_message_stream` `Err`) are not retried (those are context-recovery /
hard-fail, a separate guardrail). The cancel-token short-circuit
(`should_transparently_retry_stream` checks `!cancelled`) is wired (see the
`host_executor.rs` module doc); the bounded budget (`MAX_STREAM_RETRIES = 3`)
can't loop forever.

Streaming deltas (`MessageDelta`/`ThinkingDelta`) flow over the `Event`
channel directly (no `Callback` method); the planned inline stream reducer
keeps that path.

The provider catalog is declarative: the `providers.toml` manifest (schema and
loader in `codesmith-config`) drives `default_registry()` behind a `OnceLock`
cache, and its `base_url`/`model` columns are the per-provider fallback when
the host passes an empty `ProviderConfig` value — the manifest is a complete
per-provider default source.

Known limitations: the resolver chain still falls back to the hardcoded
`DEFAULT_*` constants rather than the manifest for env overrides, and
flash/kimi-code model variants stay host-side (no manifest entry). Both are
tracked in [`ROADMAP.md`](../ROADMAP.md).

The framework traits are validated against an inline mock LLM + mock tool
(see `crates/agent/src/executor/mod.rs` tests) — no `codesmith-providers`
dependency required, mirroring the provider foundation slice's `mock` sample.
The `ToolSpec` adapter is additionally validated by driving a real `ToolSpec`
through the framework executor end-to-end (see
`crates/agent-runtime/src/tools/framework_adapter.rs` tests), and the
`CallbackBridge` is validated by driving a tool-call roundtrip through the
executor that lights up both a mock `Event` channel and a mock `HookHost`
(see `crates/agent-runtime/src/callback_bridge.rs` tests).

The `host_executor.rs` module doc carries the complete `Known gaps (by
design)` list across nine areas — LSP flush, system-prompt refresh, thinking-only,
transparent-retry, approval, compaction, capacity, early-tool-start, and
subagent. This narrative elaborates the four most load-bearing (LSP flush /
transparent-retry / approval / compaction); for the remaining five
(system-prompt refresh / thinking-only / capacity / early-tool-start / subagent),
see the module doc directly.

## The extension system

The extension system builds on the framework-core traits. The same
three-layer split applies:

- **Contract** (`codesmith-agent::extension`): host-agnostic traits an
  extension author implements — `Extension` (the factory), `ExtensionApi`
  (the imperative registration surface), `ExtensionContext` /
  `ExtensionCommandContext` (read-mostly host state + stale-context guard),
  `ExtensionEvent` (`#[non_exhaustive]` minimal 6-variant set), `Handler`
  (observer), `ToolDefinition` / `CommandDefinition` (contribution
  contracts). The extension traits use `#[async_trait]` (unlike the
  framework core's manual `Pin<Box<dyn Future>>`) because they face extension
  authors in external crates where the macro is markedly friendlier.
- **Runtime** (`codesmith-extensions`): `ExtensionRunner` (best-effort event
  fan-out, `Arc<AtomicU64>` stale-context guard,
  two-phase stub→real `ExtensionApi`), `inventory`-based static discovery
  (`discover_static`), `EventBus` skeleton, install-source traits
  (implementations are planned).
- **Adapters** (`codesmith-agent-runtime`): `ExtensionToolSpecAdapter`
  wraps a `Box<dyn ToolDefinition>` into a `ToolSpec` so the agent loop
  sees a normal tool (mirrors `ToolSpecAdapter`); `HostAgentExecutor` holds
  an `Option<Arc<ExtensionRunner>>` + emits at four turn seams (TurnStart /
  ToolCall ×2 / ToolResult ×2 / TurnEnd ×2).
- **Host wiring** (`codesmith-tui`): `build_extension_runtime()` runs the
  discover → reconcile → load → `bind_core` sequence once at engine build
  (shares the engine's `cancel_token` so handlers observe user ESC);
  `ExtensionStateStore` (mirrors `SkillStateStore`) tracks enabled/disabled
  per id; `/extension` command group (list/info/enable/disable/status/
  reload; install/uninstall are stubs).

The `sample_scratchpad` in-tree extension exercises all three contribution
points (tool + command + handler) + the full discover → load → configure →
bind_core → emit path. `/extension list` shows it.

```
   extension author ──impls──▶ codesmith_agent::extension (contract)
                                        │ used by
                                        ▼
              codesmith_extensions (runtime: Runner + discovery + Bus)
                                        │ bridged by
                                        ▼
              codesmith_agent_runtime (ExtensionToolSpecAdapter + executor seams)
                                        │ wired by
                                        ▼
              codesmith_tui (build_extension_runtime + StateStore + /extension cmd)
```

The contract and runtime core carry the full 23-variant `ExtensionEvent` set
plus `ExtensionEventKind`/`kind()`, the `HandlerOutcome`
(`Continue`/`Cancel`/`Block`/`Transform`) cross-handler chain
(`Handler::handle` returns `Result<HandlerOutcome, _>`), per-variant
`ExtensionApi::on_variant` subscription, and `catch_unwind` isolation in
`ExtensionRunner::emit` (owned-in / `EmitOutcome`-out).

At the host seams, `EmitOutcome` is `#[must_use]` (forcing seam inspection):
`Block` at `ToolCall` skips dispatch (permission-denied), `Cancel` at
`SessionBefore*` skips compaction/switch, and `Transform` at
`Input`/`BeforeAgentStart`/`BeforeProviderRequest`/`ToolResult` rewrites the
actionable field (`ToolResult` reorders emit→`on_tool_end`→propagate to
`outcomes[idx]`); out-of-place outcomes map to `Continue`. 22 of the 23
events are emitted; `ToolExecutionUpdate` needs a
`Callback::on_tool_progress` stream hook.

Live reload re-populates the shared runner `Arc` via
`ExtensionRunner::clear_handlers` + `reload_extension_runtime`
(clear→invalidate→discover→reconcile→load→bind_core), so `/extension reload`
updates both `App.extension_runner` and the Engine's field.

Not implemented yet: `ToolExecutionUpdate` (stream hook), reload sharing the
engine's `cancel_token`, 3 tui-level seams unreachable from the App runner
(`ProjectTrust` sync context / `ResourcesDiscover` separate MCP process /
`SessionBeforeFork` dead-code fork path), the `EventBus` implementation,
`registerProvider`, `registerShortcut`/`registerFlag`/renderers, dylib
loading, install-source implementations, and the embed API. Hot-load is
permanently out; install + reload only.

## What is wired today

| Concern | Where |
|---|---|
| Core abstractions (`LlmClient`, `ProviderFactory`, `ProviderRegistry`) | `crates/agent/src/{llm_client,provider}/` |
| Registry in the engine loop (`resolve_llm_client` seeds from `default_registry()`) | `crates/tui/src/core/engine.rs` |
| `codesmith-providers` crate: `mock` provider, rig adapter `RigLlmClient<C,S>`, Cargo features | `crates/providers/` |
| Declarative factory catalog (`openai` / `anthropic` / `deepseek` / `openai-compat` ×13) | `crates/providers/providers.toml` |
| Parity bridge: reasoning heuristics + `shape_messages` / `shape_max_tokens` | `crates/providers/src/rig_adapter/{reasoning,shaper}.rs` |
| Provider selection via config, including `custom_provider` + `[[providers.custom]]` + `--custom-provider <id>`; the bare `provider = "<custom-id>"` form is rejected by design | `crates/config`, `crates/cli` |
| Framework-core agent traits (`Tool`, `ChatHistory`, `Callback`, `AgentExecutor`) | `crates/agent/src/{tools,memory,callback,executor}/` |
| Host bridges (`ToolSpecAdapter`, `CallbackBridge`, `SessionChatHistory`) and the production `HostAgentExecutor` with its guardrail set | `crates/agent-runtime/src/{tools/framework_adapter,callback_bridge,session_history}.rs`, `crates/agent-runtime/src/engine/host_executor.rs` |
| Extension system (contract + runtime + adapters + host wiring) | `crates/agent/src/extension.rs`, `crates/extensions/`, `crates/agent-runtime/src/tools/extension.rs`, `crates/tui/src/{extension_state.rs,commands/extension_commands.rs}` |
| Extension system docs | `docs/EXTENSIONS.md` |

## Registering a provider (developer guide)

A provider is a `ProviderFactory` impl behind a Cargo feature. The mock
provider (`crates/providers/src/mock.rs`) is the reference sample — copy its
shape to add a new one.

```rust
use std::sync::Arc;
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::provider::{ProviderConfig, ProviderFactory, ProviderId};

pub struct AcmeFactory;
impl ProviderFactory for AcmeFactory {
    fn id(&self) -> ProviderId { ProviderId::from("acme") }
    fn build(&self, cfg: &ProviderConfig) -> anyhow::Result<LlmClientHandle> {
        // construct your client from cfg.api_key / cfg.base_url / cfg.default_model / ...
        todo!()
    }
}
```

A host seeds the registry and may override any default:

```rust
// default_registry() returns a cached &'static ProviderRegistry (built once
// from providers.toml); clone to mutate.
let mut registry = codesmith_providers::default_registry().clone();
registry.register(Arc::new(AcmeFactory));                   // add/replace
let client = registry.build(&cfg)?;                          // never names a concrete type
```
