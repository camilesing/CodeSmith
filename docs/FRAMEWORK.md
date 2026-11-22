# The Framework Layer, Standalone

CodeSmith's framework group — the 12 crates behind the dependency boundary
enforced by `scripts/check-framework-deps.py` — is a reusable agent
framework in its own right, not internal plumbing of the CLI. This doc is
the standalone user guide: what the LangChain-style surface is, how to run
an agent with nothing but `codesmith-agent`, where the everything-is-plugin
surface lives, and what is deliberately not built yet.

Layering in one line: `codesmith-agent` is the `langchain-core` analog
(contracts + reference loop, no concrete providers); `codesmith-providers`
and every implementation crate are the partner packages; the CLI is one
host among possible ones.

## The LangChain analogy map

| LangChain | CodeSmith | Where |
|---|---|---|
| `BaseChatModel` | `LlmClient` (dyn-safe, streaming-first) | `codesmith-agent` `llm_client` |
| provider packages | `ProviderFactory` / `ProviderRegistry`; concrete clients in `codesmith-providers` | `codesmith-agent` `provider` |
| `BaseTool` / `@tool` | `Tool` + `ToolSet` | `codesmith-agent` `tools` |
| `Memory` / chat history | `ChatHistory` + `VecChatHistory` | `codesmith-agent` `memory` |
| `Callbacks` | `Callback` (+ `StopReason`, `StreamDelta`) | `codesmith-agent` `callback` |
| `AgentExecutor` | `AgentExecutor` + reference `DefaultAgentExecutor` | `codesmith-agent` `executor` |
| `ChatPromptTemplate` | `ChatPromptTemplate` (`{var}` roles + history placeholder) | `codesmith-agent` `prompt_template` |
| `BaseOutputParser` | `OutputParser` + `JsonOutputParser` | `codesmith-agent` `output_parser` |
| `with_structured_output` | `structured_output::<T>()` (schema-forced tool + validation retry) | `codesmith-agent` `structured` |
| `Document` / `BaseRetriever` / `Embeddings` / `VectorStore` | `Document` / `Retriever` / `Embeddings` / `VectorStore` + `InMemoryVectorStore` | `codesmith-agent` `retrieval` |
| model catalog / profiles | `ModelRegistry` | `codesmith-agent` `models.rs` |
| durable state, sessions | three-zone `AppendLog`, `SessionJournal`, `SessionStore` | `codesmith-session` |
| context engineering | prompt assembly, prefix cache, compaction strategies | `codesmith-prompts`, `agent-runtime` |
| MCP client | full pool, 4 transports | `codesmith-mcp-client` |

Not ported (deliberate): the LCEL `Runnable` composition layer. The
executor + tool loop + callback seam already carries that responsibility;
a composition algebra with no in-tree consumer would be dead surface.

## Running an agent with the framework alone

```
cargo run -p codesmith-agent --example standalone_agent
```

`crates/agent/examples/standalone_agent.rs` builds a complete agent —
prompt template, in-memory retrieval behind a `Tool`, the reference
executor over `VecChatHistory` with a recording `Callback`, and a typed
`structured_output` exchange — from `codesmith-agent` only, against a
scripted offline `LlmClient`. The example is the executable definition of
"the framework layer stands alone".

A real deployment swaps the scripted client for an `LlmClient`
implementation: use `codesmith-providers` (one feature per backend), or
implement the trait / register a `ProviderFactory` in your own crate —
that is the partner-package slot.

## Everything-is-plugin: the registration surface

The Cordis-style property — capabilities arrive as registrations, the
kernel stays a selection chokepoint — is spread over two surfaces:

- **Extension registrations** (any tier: Rhai mod, builtin Rhai, Rust
  extension, installed dylib) via `ExtensionApi` — see
  [EXTENSIONS.md](EXTENSIONS.md) and [MODS.md](MODS.md): tools, commands,
  event handlers (32 event kinds × 3 dispatch modes), prompt sections,
  skills, providers, message projections, guards, tool replacements,
  compaction strategies, script loops (`register_loop`).
- **Registry registrations** (crates compiled into the process): the
  `TurnExecutorRegistry` below, `ProviderRegistry`, model catalog.

### The turn-executor seam (host tier + dylib tier)

The loop a turn runs is a *selection*, resolved per turn from the
process-shared `TurnExecutorRegistry`
(`agent-runtime/src/engine/executor_composition.rs`; config
`turn_executor`, default `builtin`). The contract types
(`TurnDeps`/`TurnExecutorFactory`/`BuiltTurn`/`TurnHarvest`) are public,
and there are two registration tiers:

- **Host tier** — a crate compiled into the process registers at startup,
  owning the RAII guard:

  ```rust,ignore
  use codesmith_agent_runtime::engine::{shared_turn_executors, TurnExecutorFactory};

  let guard = shared_turn_executors()
      .register(std::sync::Arc::new(MyLoopFactory))
      .expect("first registration is never refused");
  // config: turn_executor = "my-loop"; drop(guard) un-registers.
  ```

  The executable proof lives in
  `crates/extensions-fixture-dylib/tests/turn_executor_seam.rs`.

- **Dylib tier** — an installed Rust dylib contributes a from-scratch loop
  through `ExtensionApi::register_turn_executor` (the carrier is
  type-erased because a dylib cannot write host-side registry statics —
  its copy of any static is private to the dlopen'd image). The engine
  adopts the contribution onto the registry at the next turn's assembly,
  generation-stamped; a reload's op-loop sweep removes it **before** the
  two-phase dylib unload, so the registry never holds a vtable into
  unmapped memory. End-to-end proof:
  `crates/extensions/src/runner.rs` test
  `fixture_dylib_contributes_a_generation_bound_turn_executor`.

Replacement of an id with turns in flight is refused loudly
(`turn_lease`); registration takes effect at the next turn's assembly.

## Known limitations & iteration roadmap

- **Script mods cannot ship an executor** on this seam — by design, they
  use `register_loop` (`turn_executor = "rhai"`).
- One `turn_executor` selection for every agent (no per-agent-type
  selection yet).
- **Retrieval is contracts + in-memory default only** — no vector-db
  integrations, loaders, or splitters in-tree; `IndexServiceApi` (the code
  indexer) is a separate, older seam.
- **Three tool registration surfaces coexist** (framework `ToolSet`,
  `codesmith-tools` value layer, production `ToolSpec`/`ToolRegistry` in
  `agent-runtime`); unifying them is the standing ROADMAP §E slice.
- **Runtime mod state lives in its own store, by design** — activation
  consent, enablement, and KV persist through `ModStateStore`
  (`mod_state` file + immediate reload), *not* by writing back to the
  hand-authored `config.toml` (Cordis round-trips its YAML because config
  is its only source of truth; CodeSmith deliberately splits declarative
  config from runtime state, and config identifiers change only through
  an explicit migration).
- **No publish boundary yet**: the framework group is workspace-internal.
  Separately versioned publication (a literal `langchain-core`-style
  package split, or a repo extraction) is a founder decision, not a code
  gap.
- Optional, undecided: a minimal Runnable-style composition layer if a
  real consumer appears.
