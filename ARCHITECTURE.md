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
| Agent executor loop, tool/memory abstractions (LangChain parity) | ⏳ deferred | ROADMAP §E |

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
