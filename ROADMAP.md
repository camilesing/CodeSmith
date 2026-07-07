# Roadmap — pluggable framework core

Durable record of work **deferred** from the foundation slice. The foundation
slice (commits on `feat/pluggable-framework-core`) establishes the pattern:
core abstractions in `codesmith-agent`, a registry wired into `build_engine`,
and a `codesmith-providers` crate with one sample (`mock`) provider behind
Cargo features. Everything below extends that slice.

Each item carries enough detail (file references, coupling notes) to be picked
up directly. See [`ARCHITECTURE.md`](./ARCHITECTURE.md) for the layering.

---

## 进度（2026-07 检查点）

**方向调整：** 框架层改为接入 [rig-core](https://crates.io/crates/rig-core) 作为
`LlmClient` 的可替换实现（用户决策："减少框架层代码维护量"），而非 §A 原计划的
"搬运手写客户端"。rig 自带 HTTP/SSE/重试，所以 §B 的多数解耦替换（B1 logging
facade、B2 `retry_status`、B4 `prompt_runtime`）对 rig adapter 不再适用 ——
`codesmith-providers` 只依赖 `codesmith-agent`，零 TUI 耦合（§C6 依赖审计已通过）。
仅 B3（`ApiProvider` → `ProviderKind`）仍相关。

**已完成（本检查点，`feat/pluggable-framework-core`）：**
- providers crate 的 rig adapter：`RigLlmClient<C, S>` 实现 `LlmClient`，委托 rig
  `CompletionClient`；含 `convert`/`stream`/`shaper`/`fim_translate`。
- 四家工厂 + 特性门控：`openai` / `anthropic` / `deepseek` / `openai-compat`（×13
  家）。对应原 §C1/C2/C3/C5。
- Anthropic `cache_control` 透传已对照 rig 源码验证（`system_message` 返回 `None`
  → rig 命名 `system` 字段空-drop → shaper 经 `additional_params` 注入的 `system`
  是唯一字段，per-block `cache_control` 完整保留）。
- 全 Lego 特性矩阵零警告编译（每家单独 / 组合 / `--no-default-features` / 默认
  `mock`）；10 个测试通过；工具链钉 1.90.0 stable。

**下一聚焦工作：** §D1（TUI `resolve_llm_client` 切到 `default_registry`）+ 退役
`crates/tui/src/client.{rs,chat.rs,anthropic.rs}`。**需重新规划**：rig adapter 与
手写客户端有行为对齐缺口 —— `list_models` 默认返空、cache-warmup / prompt-inspection
是 TUI 专属逻辑（非 `LlmClient` 方法）、DeepSeek `reasoning_effort` 回放策略住在客户端
消息构建器里（`build_chat_messages_with_reasoning` / `should_replay_reasoning_content`）。
原 §A 的耦合图仍可用于定位搬迁点。

**进度（2026-07-07 §D1 + 行为对齐落地，`feat/pluggable-framework-core`）：**

用户决策"激进：所有非 DeepSeek 切 rig"——OpenAI + 13 家 openai-compat + Anthropic 全部走
`codesmith_providers::default_registry()` 的 rig 工厂；DeepSeek 家族暂留 tui-local
`DeepSeekProviderFactory`（`DeepSeekClient`），待回放协议桥接后退役。

- **§D1 落地（部分）**：`resolve_llm_client` 改为先 `default_registry()` 再按需覆盖
  DeepSeek 工厂。路由测试 `resolve_llm_client_routes_anthropic_to_rig_factory` 通过
  `health_check`（rig 默认 `Ok(true)` 不探活 vs `DeepSeekClient` 探 `/models`）证明
  anthropic 走的是 rig 工厂。
- **§A2 落地**：`crates/tui/src/client/anthropic.rs` 删除（~940 行死代码——latent bug：
  anthropic 用户原本命中错误端点）；Anthropic 改由 rig `AnthropicFactory` 承载，
  `cache_control` 透传已对照 rig 源码验证。
- **§C1/C2/C3 落地**：`openai` / `anthropic` / `deepseek` / `openai-compat`（×13 家）
  特性全在 `crates/providers/Cargo.toml`；tui 启用 `openai`+`anthropic`+`openai-compat`。
  全 Lego 矩阵零警告（8 组合：no-default / mock / 单家 / 组合 / 全开）；providers crate
  37 个单测通过。
- **行为对齐桥接**（替换原 §A 的"搬运"路径）：
  - `crates/providers/src/rig_adapter/reasoning.rs`：移植 DeepSeek thinking-mode 启发式
    + effort 表（`requires_reasoning_content` / `should_replay_reasoning_content` /
    `apply_reasoning_effort`），按 `&str` provider 名键控——刻意不引 `ApiProvider`，
    维持 §C6（providers 仅依赖 `codesmith-agent`）。10 个单测。
  - `RequestShaper::shape_messages`：strip `reasoning_content`（#1542）+ tool-call 回合
    缺 reasoning 时注入 `(reasoning omitted)` 占位（#1739/#1694）。7 个单测。
  - `RequestShaper::shape_max_tokens`：xiaomi-mimo 的 `max_completion_tokens` 改名
    （非 clamp）。`req.thinking` 在 live 代码恒为 None（仅已删的 AnthropicClient 设置），
    故 `apply_reasoning_effort` 是 effort 字段的唯一权威，丢弃 verbatim 透传是安全修复。
- **§C6 维持**：providers crate 仍只依赖 `codesmith-agent`；reasoning 模块用 `&str` 键
  规避了 `ApiProvider` 依赖边（§B3 降级为低优先）。

**下一聚焦工作：**
- **DeepSeek `reasoning_content` 回放桥接**：把 `DeepSeekClient` 的
  `build_chat_messages_with_reasoning` 回放协议（effort→thinking 字段、reasoning_content
  序列化）完整迁入 rig DeepSeek 工厂，使其能双向 round-trip，然后删
  `crates/tui/src/client.{rs,chat.rs}` + tui-local `DeepSeekProviderFactory`——完成 §A1 +
  全量 §D1。`shape_messages` 已铺好 strip/占位的地基，缺的是 rig DeepSeek 工厂的完整接线。
- **Gap E 流式回放**：`reasoning::is_reasoning_model_for_stream` 当前 `#[allow(dead_code)]`，
  rig 流式路径的 reasoning 检测未接线（rig 流式 reasoning 形态不同）；接线补齐。
- **§D2**（config 选自定义 provider）、**§E**（agent executor / tool 抽象）维持原计划。

**进度（2026-07-07 §A1 + 全量 §D1 落地，DeepSeek 切 rig，`DeepSeekClient` 退役，`feat/pluggable-framework-core`）：**

上一检查点的"下一聚焦工作"已完成。经对照 rig-core 0.39.0 源码核实：rig 的
OpenAI/DeepSeek compat 层**原生**把 `AssistantContent::Reasoning` 序列化为
`reasoning_content` 线字段（`TryFrom<OneOrMany<AssistantContent>> for Vec<Message>` 提取
Reasoning），故回放桥接在 adapter 层即已完成——无需把 `build_chat_messages_with_reasoning`
的回放协议搬进 rig 工厂，只需把 DeepSeek 切到 rig。

- **§A1 落地（退役而非搬运）**：tui 启用 codesmith-providers 的 `deepseek` 特性；
  `crates/tui/src/core/engine.rs` 删除 tui-local `DeepSeekProviderFactory`，
  `resolve_llm_client` 改为 `pub(crate)`，构造中性 `ProviderConfig` 后委托
  `default_registry().build()`，DeepSeek 与其他家族一致走 rig 工厂。`DeepSeekClient` 线
  客户端**整体退役**：`client.rs` 3183→329 行、`client/chat.rs` 3490→1427 行（-4979 行）。
  仅保留 inspect/warmup 根（`inspect_prompt_for_request` / `build_cache_warmup_request` /
  `CacheWarmupKey` / `PromptInspection`）及其传递依赖（`PromptBuilder` /
  `inspect_wire_request` / `build_chat_messages_with_reasoning` / `stable_system_prompt` /
  `sha256_hex` 等），供 `ui.rs` 缓存预热与 `commands/debug.rs` inspect 子命令使用。
- **全量 §D1 落地**：所有 LLM 流量（OpenAI + 13 家 openai-compat + Anthropic + DeepSeek）
  统一经 `default_registry()` 的 rig 工厂；tui 不再持有任何 provider 工厂。12 处
  `DeepSeekClient::new` 调用点全部迁至 `resolve_llm_client`（acp/mcp/config/ui/main/
  tests/subagent/seam_manager）。
- **Gap E 关闭**：`crates/providers/src/rig_adapter/reasoning.rs` 移除
  `is_reasoning_model_for_stream`（rig 流式 compat 层原生把 `delta.reasoning_content` 路由
  到 `ReasoningDelta`，无需 model 级门控）；tui `client/chat.rs` 的同名死代码一并删除。
- **`list_models` 桥接**：`RigLlmClient` 增加 `list_models` trait 覆写——当 `http` 字段为
  `Some`（仅 DeepSeek 工厂传入）时 GET `{base_url}/models`，解析 `{data:[{id}]}` 后
  sort+dedup；其余家族返空（与非 DeepSeek 一致）。

**已知行为差异 / 回归（需后续评估）：**
- **`health_check` 不探活**：rig adapter 未覆写 `health_check`，沿用 trait 默认 `Ok(true)`，
  不再像 `DeepSeekClient` 那样探 `/models`。路由测试正是用 `health_check()==Ok(true)` 反证
  anthropic 走 rig。副作用：`test_api_connectivity` 等健康检查恒报健康；`list_models` 仍真探。
- **`caller` 字段丢失**：旧线构建器在 tool-call JSON 注入 `caller`（`caller_type`/身份，供
  代理路由用，`client/chat.rs:867`）；rig 不发送该字段。若代理依赖它，需经 `RequestShaper`
  补回。
- **模型列表无 owner 分组**：`run_models` 现收到 `Vec<String>`（纯 ID），旧 `AvailableModel`
  携带的 `owned_by`（DeepSeek 自有 vs 第三方）分组显示已移除。
- **线层压缩退役**：`DeepSeekClient` 线构建器的第二轮 dedup/截断
  （`compact_tool_result_for_wire` 等）随线客户端删除；运行期 capture-time 压缩
  （`compact_tool_result_for_context` / `micro_compact_messages`）现为唯一压缩点
  （效率而非正确性差异）。

**残留重复 / 后续清理：**
- `requires_reasoning_content` / `should_replay_reasoning_content` /
  `has_deepseek_r_series_marker` 仍同时存在于 `providers/src/rig_adapter/reasoning.rs` 与
  `tui/src/client/chat.rs`（后者是 inspect/warmup 的传递依赖）；后续抽到 `codesmith-agent`
  共享，使 reasoning.rs 成为唯一源。
- `crates/agent` / `crates/agent-runtime` 的 `LlmClient` trait 文档仍以 `DeepSeekClient`
  为具体实现示例（已退役，属陈旧文档，不影响编译）。

**下一聚焦工作：** §D2（config 选自定义 provider）、§E（agent executor / tool 抽象）维持
原计划。B3（`ApiProvider` → `ProviderKind`）降级为低优先（rig adapter 按 `&str` provider
名键控已规避该边）。

---

## §A — Provider extraction (bulk migration)

Move the production LLM clients out of the `codesmith-tui` binary into
`codesmith-providers`, behind Cargo features. The coupling map below was
gathered from `crates/tui/src/client/` (binary-only crate; the client module
is carved out of `main.rs`).

### A1 — Extract the OpenAI-compatible client
- Move `crates/tui/src/client.rs` (`DeepSeekClient`, ~1190 lines) +
  `crates/tui/src/client/chat.rs` (~2380 lines) into
  `crates/providers/src/openai_compat/`.
- `DeepSeekClient::new(config: &Config)` cannot move with the struct (orphan
  rule: a tui-local inherent `impl` can't live on a `codesmith-providers`
  type). The 12 call sites (`ui.rs`, `main.rs`, `mcp_server.rs`,
  `acp_server.rs`, `commands/config.rs`, `core/engine/tests.rs`,
  `tools/subagent/tests.rs`) switch to `resolve_llm_client(&config)` or
  `from_parts(...)`. `from_parts` already exists (Step 2).
- Re-export the moved type from tui (`pub use codesmith_providers::...`) only
  if a shim is needed; prefer routing through the registry.

### A2 — Extract the Anthropic client
- Move `crates/tui/src/client/anthropic.rs` (`AnthropicClient`, ~940 lines)
  into `crates/providers/src/anthropic/` behind the `anthropic` feature.
- Same orphan-rule constraint on its `new(config: &Config)`.

### A3 — Lift the shared client helpers
- These are used by **both** clients today (defined in `client.rs`, imported
  via `use super::{...}` by `anthropic.rs`): `ERROR_BODY_MAX_BYTES`,
  `SSE_BACKPRESSURE_*` consts, `acquire_stream_buffer`/`release_stream_buffer`
  (backed by a `OnceLock<Mutex<Vec<Vec<u8>>>>` buffer pool),
  `add_extra_root_certs`, `bounded_error_text`, `force_http1_from_env`,
  `validate_base_url_security`.
- Move them into `crates/providers/src/common/` so both extracted clients
  share one copy.

### A4 — De-duplicate the per-client helpers
- `TokenBucket`, `build_default_headers`, `build_http_client`,
  `retry_reason_label` are duplicated across `client.rs` and `anthropic.rs`.
  Unify where the semantics match; keep separate where they differ
  (Anthropic auth headers, `anthropic_messages_url`).

---

## §B — Decoupling substitutions

These are the seams that keep `codesmith-providers` free of TUI/host coupling.
They are blockers for A1/A2 being "clean" (the code can move first with the
deps still wired, then these substitutions remove the wires).

### B1 — Logging facade (blocks clean A1)
- `crate::logging` (`crates/tui/src/logging.rs`) is a **binary-local module**,
  not a crate: verbose-gated `eprintln!` with `crate::palette` colors. Client
  code calls only `logging::info` / `logging::warn`.
- Not a trivial `tracing` swap (changes output destination + drops palette
- coupling). Options: (a) add a `codesmith_agent::log` facade (info/warn via a
  settable sink; tui's `logging.rs` installs the sink at startup — preserves
  behavior); (b) swap to `tracing` and accept the behavior change.
- Recommended: (a). Land the facade in `codesmith-agent`, point the extracted
  clients at it.

### B2 — `retry_status` → outcome callback (blocks clean A1/A2)
- `crate::retry_status` (`codesmith-agent-runtime/src/retry_status.rs`) is a
  process-wide `OnceLock<Mutex<RetryState>>` singleton. The clients call
  `start` / `succeeded` / `failed` / `clear` in `send_with_retry`
  (`client.rs:719`, `anthropic.rs:193`).
- The per-attempt `on_retry` hook already exists (`ProviderConfig.on_retry`,
  wired through `with_retry`'s `RetryCallback` at `client.rs:747`). What's
  still global-only are the **terminal** calls (`succeeded`/`failed`/`clear`)
  on the `RetryResult` outcome — they can't ride the per-attempt callback.
- Extend `ProviderConfig` with an outcome callback (or a `RetryHooks` struct)
  so the extracted clients stop touching the global singleton; the host (tui)
  injects a hook that drives the footer banner.

### B3 — `ApiProvider` → `ProviderKind`
- `ApiProvider` (`codesmith-agent-runtime/src/config_types.rs:200`, 17
  variants) diverges from `ProviderKind` (`codesmith-config/src/lib.rs:73`, 16
  variants): `ApiProvider::DeepseekCN` has no `ProviderKind` peer (the
  `deepseek-cn` aliases collapse onto `Deepseek`).
- The OpenAI-compat client branches on `ApiProvider` in only two places
  (`chat.rs:80` `apply_provider_token_limit` — XiaomiMimo; `chat.rs:1915`
  `provider_accepts_reasoning_content` — 9-variant allowlist). Everywhere else
  it's stored/forwarded (`provider_name` returns `api_provider.as_str()`).
- Resolve the `DeepseekCN` alias (fold into `Deepseek`, or add it to
  `ProviderKind`), then switch the client + factory to `ProviderKind` so
  `codesmith-providers` doesn't depend on `codesmith-agent-runtime`'s
  `config_types`.

### B4 — `prompt_runtime` location
- `crate::prompt_runtime` is `pub use codesmith_agent_runtime::prompt_runtime::*`
  (canonical home: `crates/agent-runtime/src/prompt_runtime.rs`). Only
  `chat.rs` uses it: `parse_rendered_sections`, `system_prompt_to_text`,
  `PromptCachePolicy`, `PromptSectionStability` — shallow (4 items, pure
  functions + small enums) but load-bearing for prompt-cache keying.
- Either (a) move `prompt_runtime` into `codesmith-agent` core (cleaner
  layering — providers depend only on core), or (b) accept the
  `codesmith-providers → codesmith-agent-runtime` dep. Recommended: (a) once
  B1/B2 also land, so providers depend solely on `codesmith-agent`.

---

## §C — Cargo feature expansion

### C1 — `openai-compat` feature
- Gate the A1 extraction behind `openai-compat` in `crates/providers/Cargo.toml`.
- Add `reqwest`, `sha2`, `tokio` (streaming), `serde_json` deps under the
  feature.

### C2 — `anthropic` feature
- Gate the A2 extraction behind `anthropic`.

### C3 — Per-provider sub-features
- Consider splitting the OpenAI-compatible family (deepseek, nvidia-nim,
  openrouter, …) into sub-features or a single `openai-compat` feature with
  runtime config — decide based on binary-size / compile-time goals.

### C4 — Lego recipe in CI
- Add a CI job: `cargo +1.90.0 build -p codesmith-providers --no-default-features`
  (provider-less build) and `--features mock` / `--features openai-compat`
  (à la carte). Today only `mock` exists; extend as C1/C2 land.

### C5 — Feature-gated `default_registry` growth
- `crates/providers/src/lib.rs::default_registry` already shadows-registers
  each enabled provider. As features land, add their shadow blocks (or a
  single `any(...)`-gated block).

### C6 — Workspace dep audit
- Once providers no longer need `codesmith-agent-runtime` (post-B4), drop that
  dep edge to enforce the layering in the build graph.

---

## §D — Host integration

### D1 — TUI registers `codesmith-providers` factories
- `resolve_llm_client` (`crates/tui/src/core/engine.rs`) currently builds a
  fresh registry with only the tui-local `DeepSeekProviderFactory`. Extend it
  to seed from `codesmith_providers::default_registry()` (under a tui feature
  gate) so compiled-in providers are available, then register the
  tui-local `DeepSeekProviderFactory` on top (it captures `ApiProvider`, so it
  can't live in `codesmith-providers` until B3).

### D2 — Config escape hatch for custom providers
- `ApiProvider` has no `Mock`/custom variant, so `provider = "mock"` can't be
  selected via config today. Add a config path for custom provider ids (e.g. a
  `provider = "custom:<id>"` form, or a `[[providers.custom]]` table) that
  maps to `ProviderId::Custom`, so a user can select any registered provider —
  the pi-mono "freely replace" UX.

---

## §E — Framework core growth (LangChain parity)

The provider seam is the first LangChain analog. These extend the core toward
a fuller agent framework.

### E1 — Agent executor loop
- A LangChain `AgentExecutor` analog: a loop that drives
  `LlmClient::create_message_stream` → tool calls → feed results back, with
  step/max-iteration caps. Today this lives tangled in `codesmith-agent-runtime`
  `Engine`; extract a reusable `AgentExecutor` trait + default impl in
  `codesmith-agent`.

### E2 — Tool / message abstractions in core
- Promote the tool-call protocol (`Tool`, `ToolUse`, `ToolResult` shapes in
  `models.rs`) into a `codesmith_agent::tools` module with a `Tool` trait
  (name, schema, async run), so providers and hosts share one tool contract.

### E3 — Memory / callback abstractions
- LangChain `Memory` + `Callbacks` analogs: a `ChatHistory` trait and a
  `Callback` trait (on_llm_start / on_llm_end / on_tool_*) in core, so the
  executor is observable and stateful without host coupling.

### E4 — Declarative config + lazy loading
- pi-mono loads `models.json` and lazily constructs providers. Add a
  declarative `providers.toml` (id, factory feature, base_url, model) consumed
  by `default_registry`, with lazy construction so an unused provider never
  pays a build cost.
