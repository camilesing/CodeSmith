# Architecture — pluggable framework core

This document describes the **provider pluggability** layer introduced by the
"foundation slice" refactor: how the CodeSmith stack separates LLM *abstraction*
from *implementation*, and how a host assembles providers like Lego blocks at
build time.

For the backlog of work that extends this slice, see [`ROADMAP.md`](./ROADMAP.md).

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
│  • provider::{ProviderId,    │   ◀──── cfg    │  • openai-compat  (ROADMAP) │
│      ProviderConfig,         │     features   │  • anthropic      (ROADMAP) │
│      ProviderFactory,        │                └─────────────────────────────┘
│      ProviderRegistry}       │                            ▲
│  • models, retry             │                            │ path dep
└──────────────┬───────────────┘                            │
               │ path dep                                    │
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
│  • tui-local DeepSeekProviderFactory (wraps DeepSeekClient)│
│  • Config, logging, retry_status (UI globals)             │
└───────────────────────────────────────────────────────────┘
```

The arrow that matters: **`codesmith-tui` depends on `codesmith-providers`
(optional, feature-gated), never the reverse.** Providers depend only on
`codesmith-agent` (and, for now, `codesmith-agent-runtime` for shared globals —
see ROADMAP §B for removing that).

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
               ProviderFactory::build(&cfg) ───────▶ MockClient / RigLlmClient / DeepSeekClient / ...
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

## The framework-core agent seam (§E)

The provider seam above is the first LangChain analog. §E extends the core
toward a fuller agent framework with four host-agnostic traits that mirror
LangChain's `BaseTool` / `Memory` / `Callbacks` / `AgentExecutor`. They live in
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
  [`run`](tools::Tool::run) takes only a parsed `input` — there is **no fat
  per-call `ToolContext`** in the core (that lives in
  `codesmith-agent-runtime::tools::spec`). The bridge onto the production
  `ToolSpec`+`ToolContext` is `ToolSpecAdapter` (in
  `codesmith-agent-runtime::tools::framework_adapter`, §E): it captures a
  shared `ToolContext` and delegates `run` → `ToolSpec::execute`. The wire
  definition sent to the model is the separate `models::Tool`; `ToolSet`
  converts executable → wire via `to_api_tools()`.
- **`ChatHistory`** (`memory::ChatHistory`) — the transcript view (LangChain
  `Memory` analog): `messages` / `push` / `clear`. `VecChatHistory` is the
  in-memory default; the host backs it with its `Session` via `SessionChatHistory`
  (in `codesmith-agent-runtime::session_history`, §E).
- **`Callback`** (`callback::Callback`) — observation hooks (LangChain
  `Callbacks` analog): `on_llm_start` / `on_llm_end` / `on_tool_start` /
  `on_tool_end` / `on_step` / `on_complete`, all default no-ops. `CallbackSet`
  fans out to several observers; `NoopCallback` is the default. The bridge onto
  the host's `Event` UI channel + `HookHost` shell hooks is `CallbackBridge`
  (in `codesmith-agent-runtime::callback_bridge`, §E): it forwards the
  tool-lifecycle hooks onto both paths; the LLM/step/complete hooks are
  documented no-ops (the production caller and stream-reduction code own those).
- **`AgentExecutor`** (`executor::AgentExecutor`) — drives the loop;
  `DefaultAgentExecutor` is the reference impl (core). The host-side
  `HostAgentExecutor` (in `codesmith-agent-runtime::engine::host_executor`,
  §E) mirrors the bare loop over the three bridges and is the designated home
  for absorbing the production `Engine`'s guardrails slice by slice — **four**
  are now absorbed: **loop-guard** (block the 3rd identical call, warn/halt on
  3/8 consecutive failures) at its per-tool / post-tool seams, **LSP flush**
  (collect diagnostics per successful edit, flush them as a user message before
  the next request) at its per-tool / per-step-pre-request seams,
  **transparent-retry** (re-issue the request when the stream dies mid-flight
  before any content commits, up to 3 times; reset the budget on a healthy
  round) at its per-step post-stream seam, and **steer** (drain queued user
  inputs as `user` messages before the next request) at its per-step
  pre-request seam. The LSP accumulator and the steer receiver are the
  **interior-mutability slices**: `LspProbe.pending` is
  `Arc<std::sync::Mutex<Vec<DiagnosticBlock>>>` and `steer` is
  `Option<Arc<std::sync::Mutex<mpsc::Receiver<String>>>>` (because
  `AgentExecutor::run` is `&self` while the accumulator mutates on
  collect/flush and `try_recv` takes `&mut self`; locks never held across an
  `await`, matching `CallbackBridge`), persisting across `run` calls so
  diagnostics from an edit on a turn ending via `MaxSteps` surface on the next
  turn's first flush, and a steer queued between turns is picked up on the
  next turn's first drain. transparent-retry reuses the local-state pattern
  (a per-run `u32` counter, matching loop-guard). Guardrail status surfaces
  over the host's `Event` channel (`event_tx`), not the `Callback`.
  `StopReason` (`NoToolCalls` / `MaxSteps` / `Error`) is the terminal outcome.

What is **not** here yet (later §E slices): absorbing the production
`Engine`/`turn_loop.rs` guardrails into `HostAgentExecutor` — the three
host→framework bridges are all landed (`ToolSpecAdapter`, `CallbackBridge`,
`SessionChatHistory`), and the host-side `HostAgentExecutor` runs the bare
LLM↔tool loop over them with **four guardrails absorbed**: **loop-guard** at
its per-tool / post-tool seams (block the 3rd identical call, warn/halt on 3/8
consecutive failures), **LSP flush** at its per-tool (post-edit collect) /
per-step pre-request (flush) seams — the first guardrail to need `Engine`
mutable state, landed as `Arc<std::sync::Mutex<Vec<DiagnosticBlock>>>` on
`LspProbe` (the first interior-mutability slice; lock never held across an
`await`, matching `CallbackBridge`; persists across `run` calls so a
`MaxSteps`-ended turn's edit diagnostics surface next turn) —
**transparent-retry** at its per-step post-stream seam (re-issue the request
when the stream dies mid-flight before any content commits, up to 3 times;
budget resets on a healthy round; transparent to the `Callback`) — and
**steer** at its per-step pre-request seam (drain queued user inputs as `user`
messages before the request snapshot), landed as
`Option<Arc<std::sync::Mutex<mpsc::Receiver<String>>>>` (interior-mutable
because `try_recv` takes `&mut self` while `run` is `&self`; same
`Arc<Mutex<…>>` pattern as `LspProbe`; persists across `run` calls so a steer
queued between turns is picked up next turn). Its four seams (per-step
pre-request / post-stream / per-tool / post-tool) grow the remaining six
guardrails slice by slice, after which `handle_deepseek_turn` retires.
loop-guard proved `&self` + local state suffices for self-contained
guardrails; LSP flush proves the `Arc<Mutex<…>>` shape for guardrails needing
shared mutable state (steer follows the same pattern);
transparent-retry proves the seam-2 post-stream shape (local counter + the
`accumulate_stream` `Err` signal). **Known gaps in the LSP flush (by design):**
`apply_patch` path derivation is deferred (needs
`HostServices::preflight_apply_patch_paths`, unreachable from `agent-runtime`
without the heavy host trait; the live `handle_deepseek_turn` still covers
it); the synthetic flush message carries no `<turn_meta>` enrichment (the
framework path has no turn_meta anywhere yet — cross-cutting host-side
concern, deferred to its own slice); no `emit_session_updated` for the push
(consistent with the executor's other message pushes; UI surfacing deferred
to the wire-in step). **Known tradeoffs in transparent-retry (by design):**
`accumulate_stream` bails on the first erroring stream item and drops partial
blocks, so the retry fires even when production would ship partial content (it
tracks `any_content_received` inline) — since the partial content is lost,
retrying is the only recovery path, and inline stream reduction (a later slice
that replaces `accumulate_stream`) closes the gap; pre-stream connection
errors (`create_message_stream` `Err`) are not retried (production treats those
as context-recovery / hard-fail, a separate guardrail); the cancel-token
short-circuit (production's `should_transparently_retry_stream` checks
`!cancelled`) is deferred to the wire-in step — the bounded budget
(`MAX_STREAM_RETRIES = 3`) can't loop forever. Streaming deltas
(`MessageDelta`/`ThinkingDelta`) will keep flowing over the `Event` channel
directly (no `Callback` method) once an inline stream reducer replaces
`accumulate_stream`. E4 (declarative `providers.toml` + lazy loading) is also
deferred. The framework traits are validated
against an inline mock LLM + mock tool (see `crates/agent/src/executor/mod.rs`
tests) — no `codesmith-providers` dependency required, mirroring the provider
foundation slice's `mock` sample. The `ToolSpec` adapter is additionally
validated by driving a real `ToolSpec` through the framework executor
end-to-end (see `crates/agent-runtime/src/tools/framework_adapter.rs` tests),
and the `CallbackBridge` is validated by driving a tool-call roundtrip through
the executor that lights up both a mock `Event` channel and a mock `HookHost`
(see `crates/agent-runtime/src/callback_bridge.rs` tests).

## What is wired today (foundation slice + §D1 parity bridge)

| Concern | Status | Where |
|---|---|---|
| Core abstractions (`LlmClient`, `ProviderFactory`, `ProviderRegistry`) | ✅ done | `crates/agent/src/{llm_client,provider}/` |
| Registry in the real engine loop | ✅ done | `crates/tui/src/core/engine.rs` `resolve_llm_client` |
| TUI-local `DeepSeekProviderFactory` (wraps `DeepSeekClient`) | ✅ done — DeepSeek family only | `crates/tui/src/core/engine.rs` |
| `DeepSeekClient::from_parts` (neutral 6-field constructor) | ✅ done | `crates/tui/src/client.rs` |
| `codesmith-providers` crate + `mock` provider + Cargo features | ✅ done | `crates/providers/` |
| rig adapter `RigLlmClient<C,S>` impls `LlmClient` | ✅ done | `crates/providers/src/rig_adapter/` |
| Four rig-backed factories (`openai` / `anthropic` / `deepseek` / `openai-compat` ×13) | ✅ done | `crates/providers/src/{openai,anthropic,deepseek,openai_compat}.rs` |
| `resolve_llm_client` seeds from `default_registry()` for all non-DeepSeek | ✅ done (§D1 partial) | `crates/tui/src/core/engine.rs` |
| `AnthropicClient` retired — rig `AnthropicFactory` replaces it (§A2) | ✅ done | `crates/tui/src/client/anthropic.rs` deleted |
| Parity bridge: reasoning heuristics + `shape_messages` / `shape_max_tokens` | ✅ done | `crates/providers/src/rig_adapter/{reasoning,shaper}.rs` |
| Extract `DeepSeekClient` into `codesmith-providers` (retire tui-local factory) | ⏳ deferred — needs DeepSeek replay bridge | ROADMAP §A1 |
| Decoupling substitutions (B3 `ApiProvider`→`ProviderKind`) | ⏳ deferred — mitigated: reasoning is `&str`-keyed | ROADMAP §B |
| Host selects providers via config (e.g. `provider = "mock"` / custom id) | ⏳ deferred | ROADMAP §D2 |
| Agent executor loop, tool/memory abstractions (LangChain parity) | ✅ framework-core traits landed (E1/E2/E3); `ToolSpec`→`Tool` adapter landed (§E); `Event`/`HookHost`→`Callback` bridge landed (§E); `Session`→`ChatHistory` bridge landed (§E); `HostAgentExecutor` skeleton + loop-guard + LSP flush + transparent-retry + steer absorbed (§E, bare loop + 4/10 guardrails via `event_tx`; interior-mutability `Arc<Mutex<…>>` on `LspProbe` + steer receiver; transparent-retry at seam-2 post-stream; steer at seam-1 pre-request); production `Engine` migration in progress | `crates/agent/src/{tools,memory,callback,executor}/`, `crates/agent-runtime/src/{tools/framework_adapter,callback_bridge,session_history}.rs`, `crates/agent-runtime/src/engine/host_executor.rs` |

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
let mut registry = codesmith_providers::default_registry(); // compiled-in providers
registry.register(Arc::new(AcmeFactory));                   // add/replace
let client = registry.build(&cfg)?;                          // never names a concrete type
```
