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
  for absorbing the production `Engine`'s guardrails slice by slice — **six**
  are now absorbed: **loop-guard** (block the 3rd identical call, warn/halt on
  3/8 consecutive failures) at its per-tool / post-tool seams, **LSP flush**
  (collect diagnostics per successful edit, flush them as a user message before
  the next request) at its per-tool / per-step-pre-request seams,
  **transparent-retry** (re-issue the request when the stream dies mid-flight
  before any content commits, up to 3 times; reset the budget on a healthy
  round) at its per-step post-stream seam, **steer** (drain queued user
  inputs as `user` messages before the next request) at its per-step
  pre-request seam, **approval** (gate write/code-exec tools behind user
  permission: emit `ApprovalRequired` + block on the decision channel by wire
  tool id; denied ⇒ `permission_denied` error, tool skipped) at its per-tool
  seam, and **compaction** (micro-compact stale tool results past the 32KB
  cache trigger without an LLM call, then auto-compact via an LLM summary when
  `should_compact` passes; both wholesale-replace via `clear()`+`push()`) at
  its per-step pre-request seam. The LSP accumulator, the steer receiver, the
  approval receiver, and the compaction probe are the
  **interior-mutability slices**: `LspProbe.pending` is
  `Arc<std::sync::Mutex<Vec<DiagnosticBlock>>>` (LSP: lock never held across an
  `await`, matching `CallbackBridge`) and `steer` is
  `Option<Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>>` — `tokio::sync::Mutex`
  (not `std`) so the guard may cross the blocking `recv().await` in the subagent
  blocking hold's `biased select!` steer arm (same rationale as `approval`; the
  pre-request `try_recv` drain is non-blocking and uncontended — single consumer —
  so the tokio mutex is a no-cost upgrade there); both interior-mutable because
  `AgentExecutor::run` is `&self` while the accumulator mutates on collect/flush
  and `try_recv`/`recv` take `&mut self`, persisting across `run` calls so
  diagnostics from an edit on a turn ending via `MaxSteps` surface on the next
  turn's first flush, and a steer queued between turns is picked up on the
  next turn's first drain. `approval` uses a `tokio::sync::Mutex`
  (`Option<Arc<tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>>>`,
  because the guard must cross the blocking `recv().await`; a std mutex guard
  isn't `Send`). `compaction` carries
  `micro_state: Arc<std::sync::Mutex<MicroCompactState>>` and
  `circuit_breaker: Arc<std::sync::Mutex<CompactionCircuitBreaker>>` (no lock
  crosses an `await` — messages are cloned out before the async
  `compact_messages_safe` call), persisting across `run` calls so a failed
  compaction on turn N still trips the breaker on turn N+1 (matching
  `Engine.micro_compact_state` / `.compaction_circuit_breaker`).
  transparent-retry reuses the local-state pattern
  (a per-run `u32` counter, matching loop-guard). Guardrail status surfaces
  over the host's `Event` channel (`event_tx`), not the `Callback`.
  `StopReason` (`NoToolCalls` / `MaxSteps` / `Error`) is the terminal outcome.

What is here (§E cutover done): the production `Engine` guardrails (formerly
in the now-deleted `turn_loop.rs`, retired `handle_deepseek_turn`) absorbed
into `HostAgentExecutor` — the three
host→framework bridges are all landed (`ToolSpecAdapter`, `CallbackBridge`,
`SessionChatHistory`), and the host-side `HostAgentExecutor` runs the bare
LLM↔tool loop over them with **six guardrails absorbed**: **loop-guard** at
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
`Option<Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>>` — `tokio::sync::Mutex`
(not `std`) so the guard may cross the blocking `recv().await` in the subagent
blocking hold's `biased select!` steer arm (same rationale as `approval`; the
pre-request `try_recv` drain is non-blocking and uncontended); persists across
`run` calls so a steer
queued between turns is picked up next turn) — **approval** at its per-tool
seam (gate write/code-exec tools: emit `ApprovalRequired` + block on the
decision channel by wire tool id; denied ⇒ `permission_denied` error; the
a `tokio::sync::Mutex` guardrail because the guard must cross `recv().await`
(steer shares this rationale for its subagent-blocking-hold arm); static approval
derivation from `Tool::capabilities`, per-input
override + sandbox elevation deferred to wire-in) — and **compaction** at its
per-step pre-request seam (micro-compact stale tool results past the 32KB
cache trigger without an LLM call, then auto-compact via an LLM summary when
`should_compact` passes; both wholesale-replace the transcript via `clear()`+
`push()`; `CompactionProbe` carries `std::sync::Mutex` micro-state + circuit
breaker that persist across `run` calls; summary-prompt merge absorbed ✅
(slice 25a §E), attachment reinject absorbed ✅ (slice 25b §E), post-compact
cleanup absorbed ✅ (slice 25c §E) — see the `host_executor.rs` module doc; only
enhancements + working-set pins remain deferred to wire-in). Its four seams (per-step
pre-request / post-stream / per-tool / post-tool) have since grown the
remaining guardrails too (see the `host_executor.rs` module doc for the full
set), and `handle_deepseek_turn` retired in the slice 20 §E cutover.
loop-guard proved `&self` + local state suffices for self-contained
guardrails; LSP flush proves the `Arc<Mutex<…>>` shape for guardrails needing
shared mutable state (steer adopts the same shape but `tokio::sync::Mutex`, not
`std` — see above);
transparent-retry proves the seam-2 post-stream shape (local counter + the
`accumulate_stream` `Err` signal); approval proves the seam-3 per-tool shape
with a blocking `recv().await` (`tokio::sync::Mutex`); compaction proves the
seam-1 pre-request wholesale-replace shape (clone-then-`compact_messages_safe`,
`clear()`+`push()` apply) with a cross-`run` circuit breaker. **Known gaps in
the LSP flush (by design):**
`apply_patch` path derivation is deferred (needs
`HostServices::preflight_apply_patch_paths`, unreachable from `agent-runtime`
without the heavy host trait); the synthetic flush message carries no `<turn_meta>` enrichment (the
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
`!cancelled`) is absorbed ✅ — Checkpoints B/C/D wired (see the `host_executor.rs`
module doc); the bounded budget (`MAX_STREAM_RETRIES = 3`) can't loop forever. Streaming deltas
(`MessageDelta`/`ThinkingDelta`) will keep flowing over the `Event` channel
directly (no `Callback` method) once an inline stream reducer replaces
`accumulate_stream`. E4 (declarative `providers.toml` + lazy loading) has
landed — slice 43 shipped the schema/loader in `codesmith-config`, slice 44
wired `default_registry` to the bundled `providers.toml` (externalizing the
`COMPAT_KINDS` catalog) with a `OnceLock` cache, and slice 45 populated the
`base_url`/`model` columns and made the factories consume them as a fallback
when the host passes an empty `ProviderConfig` value (so the manifest is a
complete per-provider default source). Two follow-ups are deferred (tracked in
ROADMAP §E4, slice 51): the resolver chain still falls back to the hardcoded
`DEFAULT_*` constants rather than the manifest (env override augment —
cross-layer-unreachable per §C6), and flash/kimi-code model variants stay
host-side (no manifest entry). The framework traits are validated
against an inline mock LLM + mock tool (see `crates/agent/src/executor/mod.rs`
tests) — no `codesmith-providers` dependency required, mirroring the provider
foundation slice's `mock` sample. The `ToolSpec` adapter is additionally
validated by driving a real `ToolSpec` through the framework executor
end-to-end (see `crates/agent-runtime/src/tools/framework_adapter.rs` tests),
and the `CallbackBridge` is validated by driving a tool-call roundtrip through
the executor that lights up both a mock `Event` channel and a mock `HookHost`
(see `crates/agent-runtime/src/callback_bridge.rs` tests).

The `host_executor.rs` module doc carries the complete set of §E `Known gaps
(by design)` across nine areas — LSP flush, system-prompt refresh, thinking-only,
transparent-retry, approval, compaction, capacity, early-tool-start, and
subagent. This narrative elaborates the four most load-bearing (LSP flush /
transparent-retry / approval / compaction); for the remaining five
(system-prompt refresh / thinking-only / capacity / early-tool-start / subagent
— each with its own deferred-to-wire-in items), see the module doc directly
rather than transcribing them here (avoids line-drift on each future slice).

## The extension system (§F)

§F builds the extension system on top of the §E framework-core traits. The
same three-layer split applies:

- **Contract** (`codesmith-agent::extension`): host-agnostic traits an
  extension author implements — `Extension` (the factory), `ExtensionApi`
  (the imperative registration surface), `ExtensionContext` /
  `ExtensionCommandContext` (read-mostly host state + stale-context guard),
  `ExtensionEvent` (`#[non_exhaustive]` minimal 6-variant set), `Handler`
  (observer), `ToolDefinition` / `CommandDefinition` (contribution
  contracts). The extension traits use `#[async_trait]` (unlike §E's manual
  `Pin<Box<dyn Future>>`) because they face extension authors in external
  crates where the macro is markedly friendlier.
- **Runtime** (`codesmith-extensions`): `ExtensionRunner` (event fan-out
  best-effort per §8.3, `Arc<AtomicU64>` stale-context guard per §7.3,
  two-phase stub→real `ExtensionApi`), `inventory`-based static discovery
  (`discover_static`), `EventBus` skeleton (impl is §F3), install-source
  traits (impls are §F5).
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
  reload working; install/uninstall stub "phase 2").

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

Slice 1 (§F1) lands the minimal contract + runtime + adapters + host wiring
+ sample. Deferred to §F2–§F8: the full ~30-event lifecycle +
cancel/transform/block chains, `EventBus` impl, `registerProvider`,
`registerShortcut`/`registerFlag`/renderers, dylib loading (phase 2),
install-source impls, embed API. Hot-load is permanently out (spec §2.4);
install + reload only.

## What is wired today (foundation slice + §D1 parity bridge)

| Concern | Status | Where |
|---|---|---|
| Core abstractions (`LlmClient`, `ProviderFactory`, `ProviderRegistry`) | ✅ done | `crates/agent/src/{llm_client,provider}/` |
| Registry in the real engine loop | ✅ done | `crates/tui/src/core/engine.rs` `resolve_llm_client` |
| TUI-local `DeepSeekProviderFactory` retired — rig `DeepSeekFactory` (via `default_registry()`) replaces it (§A1) | ✅ done — tui holds no provider factory | deleted from `crates/tui/src/core/engine.rs` |
| `DeepSeekClient` retired — rig `RigLlmClient` replaces it (§A1); `from_parts` deleted with the client | ✅ done | `crates/tui/src/client.rs` deleted (slice 41) |
| `codesmith-providers` crate + `mock` provider + Cargo features | ✅ done | `crates/providers/` |
| rig adapter `RigLlmClient<C,S>` impls `LlmClient` | ✅ done | `crates/providers/src/rig_adapter/` |
| Four rig-backed factories (`openai` / `anthropic` / `deepseek` / `openai-compat` ×13) | ✅ done — catalog now declarative (`providers.toml`, §E4); `base_url`/`model` populated + consumed as manifest-default fallback (§E4 slice 45); follow-ups (env override augment + flash/kimi-code variant sinking) deferred — tracked in ROADMAP §E4 (slice 51) | `crates/providers/src/{openai,anthropic,deepseek,openai_compat}.rs`, `crates/providers/providers.toml` |
| `resolve_llm_client` seeds from `default_registry()` for all providers | ✅ done (§D1 partial → §A1 full cutover — DeepSeek moved off the tui-local factory onto rig) | `crates/tui/src/core/engine.rs` |
| `AnthropicClient` retired — rig `AnthropicFactory` replaces it (§A2) | ✅ done | `crates/tui/src/client/anthropic.rs` deleted |
| Parity bridge: reasoning heuristics + `shape_messages` / `shape_max_tokens` | ✅ done | `crates/providers/src/rig_adapter/{reasoning,shaper}.rs` |
| Extract `DeepSeekClient` into `codesmith-providers` (retire tui-local factory) | ✅ done (superseded — retired, not extracted) — `DeepSeekClient` retired via the rig adapter; the replay-bridge blocker was found unnecessary (rig's compat layer natively serializes `AssistantContent::Reasoning` as `reasoning_content`); tui `client.rs`/`chat.rs` deleted (slice 41), inspect/warmup migrated to `codesmith-agent-runtime` `prompt_inspect`, reasoning predicates + `sha256_hex` deduped (slice 42) | ROADMAP §A1 |
| Decoupling substitutions (B3 `ApiProvider`→`ProviderKind`) | ✅ done — `DeepseekCN` folded onto `Deepseek` (slice 52); `&str`-keying was the §C6 decoupling path | ROADMAP §B |
| Host selects providers via config (e.g. `provider = "mock"` / custom id) | ✅ done (9d47942c) — `custom_provider` selector + `[[providers.custom]]` table; §D2 slice 46 closed the residual polish — `--custom-provider <id>` CLI flag (env-forwarded to the TUI) + per-entry `config set/get/unset providers.custom.<id>.<field>` (find-or-create by id); the bare `provider = "<id>"` form stays **by-design rejected** (see 9d47942c — cascades the closed `ProviderKind` enum through config + overrides + env + every match arm) | ROADMAP §D2 |
| Agent executor loop, tool/memory abstractions (LangChain parity) | ✅ framework-core traits landed (E1/E2/E3); `ToolSpec`→`Tool` adapter landed (§E); `Event`/`HookHost`→`Callback` bridge landed (§E); `Session`→`ChatHistory` bridge landed (§E); `HostAgentExecutor` is the live production path (slice 20 cutover — `handle_send_message` routes through it, `handle_deepseek_turn` deleted); all guardrails absorbed across slices 11–40 (loop-guard + LSP flush + transparent-retry + steer + approval + compaction + capacity + subagent + early-tool-start/parallel-dispatch + thinking-only) via `event_tx`; interior-mutability `Arc<std::sync::Mutex<…>>` on `LspProbe` + `CompactionProbe` micro-state/breaker, `tokio::sync::Mutex` on steer + approval receivers (both cross `recv().await` in the subagent blocking hold's `biased select!`); transparent-retry at seam-2 post-stream; steer + compaction at seam-1 pre-request; approval at seam-3 per-tool; production `Engine` migration done | `crates/agent/src/{tools,memory,callback,executor}/`, `crates/agent-runtime/src/{tools/framework_adapter,callback_bridge,session_history}.rs`, `crates/agent-runtime/src/engine/host_executor.rs` |
| Extension system (§F1 foundational core) | ✅ done (slice 1 §F1) — minimal 6-event contract (`codesmith-agent::extension`) + runtime (`codesmith-extensions`: `ExtensionRunner` + stub→real `ExtensionApi` + `inventory` discovery + `EventBus` skeleton + install-source traits) + adapter (`ExtensionToolSpecAdapter`) + `HostAgentExecutor` 4-seam emits (TurnStart/ToolCall/ToolResult/TurnEnd) + `build_extension_runtime()` + `ExtensionStateStore` + `/extension` command group (list/info/enable/disable/status/reload working; install/uninstall stub "phase 2") + in-tree `scratchpad` sample; full lifecycle + `EventBus` impl + dylib + install-source impls deferred to §F2–§F8; hot-load permanently out | `crates/agent/src/extension.rs`, `crates/extensions/`, `crates/agent-runtime/src/tools/extension.rs`, `crates/agent-runtime/src/engine/{mod.rs,host_executor.rs}`, `crates/tui/src/{extension_state.rs,commands/extension_commands.rs,core/engine.rs,tui/ui.rs}` |
| Extension system docs | ✅ done (slice 1 §F1) | `docs/EXTENSIONS.md` |

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
