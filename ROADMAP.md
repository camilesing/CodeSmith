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
- ~~`crates/agent` / `crates/agent-runtime` 的 `LlmClient` trait 文档仍以 `DeepSeekClient`
  为具体实现示例（已退役，属陈旧文档，不影响编译）~~ → §D2 已清理（trait 文档改为中性
  措辞，不再以任何具体客户端为示例）。

**下一聚焦工作：** §D2（config 选自定义 provider）、§E（agent executor / tool 抽象）维持
原计划。B3（`ApiProvider` → `ProviderKind`）降级为低优先（rig adapter 按 `&str` provider
名键控已规避该边）。

---

**进度（2026-07-07 §D2 落地，自定义 provider 逃生舱，`feat/pluggable-framework-core`）：**

§D2 把"自由替换实现"的 UX 补齐为 pi-mono 平行——用户可在 config 注册一个非内置
OpenAI-compat provider 并按 id 选中，无需改源码、无需新 Cargo feature。本轮范围仅 §D2 +
陈旧文档清理（§E 多切片跨 crate，延后）。

- **schema（双层）**：
  - `codesmith-config`：`ConfigToml.custom_provider: Option<String>` 选中字段 +
    `ProvidersToml.custom: Vec<CustomProviderToml>`（`id`/`api_key`/`base_url`/`model`/
    `auth_mode`/`http_headers`）。`get_value("providers.custom")` 把数组包进
    `{custom = [...]}` 再序列化（根级裸数组是非法 TOML）；`set_value` 对
    `providers.custom*` 一律 bail 并指向手改文件；`is_sensitive_config_key` 把
    `providers.custom` 标为敏感（`config list` 脱敏）。4 个单测。
  - tui 运行期 `Config`：`CustomProviderConfig` 镜像 on-disk 结构；`custom_provider()`
    取选中 id（trim + 过滤空），`custom_provider_entry()` 按 id 取条目（first wins）。
- **字段选择**：用专用 `custom_provider: Option<String>` 选中字段，而非裸
  `provider = "acme"`。后者会把 `ConfigToml.provider: ProviderKind` 闭合枚举 +
  `ResolvedRuntimeOverrides`/`CliRuntimeOverrides`/`EnvRuntimeOverrides`（全 `ProviderKind`）
  + 所有 `match ProviderKind` 臂的级联改动强加给 config 层，破坏闭合枚举校验和分层
  （config 不能返回 `ProviderId`——agent 依赖 config，反之不行）。raw 字符串选中 +
  TUI host 在 `resolve_llm_client` 映射到 `ProviderId::Custom(id)` 是最小切口。
- **接线**：`resolve_llm_client` 顶部加 custom 分支——`custom_provider()` 为 `Some` 时
  `ProviderId::Custom(id)`，否则 `ProviderId::from(api_provider().as_str())`。中性
  `ProviderConfig` 字段（api_key/base_url/default_model/http_headers）复用既有 accessor，
  后者已为 custom 路径读 `[[providers.custom]]` 条目；仅 `provider` id 不同。
- **accessor custom 分支**：`default_model`（entry.model → root default_text_model →
  `"deepseek-v4-pro"` 通用兜底，verbatim 透传，不做 DeepSeek 命名规整）；`deepseek_base_url`
  （entry.base_url → root base_url，无 DeepSeek 默认 URL）；`deepseek_api_key`
  （entry.api_key → root api_key → localhost 空 → bail 带 custom 专属错误，无 per-id env
  槽）；`http_headers`（entry 头 merge 于 root 之上）。`validate`：custom 优先，拒内置名
  碰撞、要求匹配条目，并用 `custom_provider().is_none()` 守卫内置 provider +
  default_text_model 校验。`normalize_model_config` 对 custom 跳过 root 规整。
  `merge_config` / `merge_providers_config` 合并 custom 字段。
- **陈旧文档清理**：`crates/agent/src/llm_client/mod.rs` 的 `LlmClient` trait 文档移除全部
  6 处 `DeepSeekClient` 引用，改为中性措辞（"a provider that overrides this" /
  "rig-backed today"）。`crates/agent-runtime` 无引用（核验通过）。
- **安全**：`merge_project_overrides` 刻意不合并 `custom_provider` 与 `providers.custom`
  （不可信 repo overlay——凭据/端点/provider 选择），与既有 policy 一致。
- **CLI 平价**：`config set/get/unset/list custom_provider` 经 `ConfigToml` 既有方法自动可用；
  `providers.custom` 为只读（手改文件）。

**已知 v1 限制：**
- **能力矩阵 Deepseek 兜底**：custom provider 经 `api_provider()` 落到 Deepseek，故 context
  window / max_output / thinking 预算用 DeepSeek 默认。仅诊断用（`config_types.rs:340-343`
  注明 "model metadata for diagnostics and CI policy…Normal turns use a separate, more
  conservative request cap"）。请求塑形不受影响（rig adapter 按 `&str` provider 名键控）。
- **`auth_mode` 未接线**：`CustomProviderConfig.auth_mode` 存储但无 custom accessor 消费
  （仅 Moonshot `kimi_oauth` / 顶层 DeepSeek 用）；v1 不暴露该旋钮。
- **`MockProviderFactory`**：`ProviderId::from("mock")` = `Custom("mock")`，故
  `custom_provider = "mock"` 在 mock 编译进时可解析；tui 默认不编 mock（正确行为）。

**后续 deferred（非本轮）：**
- `--custom-provider` CLI flag、`config set providers.custom.*` per-entry 写入、裸
  `provider = "acme"` 形式、`ApiProvider::Custom` 臂、per-id env 槽。

**下一聚焦工作：** §E（agent executor / tool 抽象）维持原计划。§D2 deferred 项（CLI flag /
per-entry 写入 / 裸形式）低优先。B3（`ApiProvider` → `ProviderKind`）仍低优先。

---

**进度（2026-07-08 §E foundation 落地，框架核心 agent 抽象，`feat/pluggable-framework-core`）：**

§E 的第一个切片落地——在 `codesmith-agent`（CORE crate）新增 LangChain-parity 的四个
host-agnostic trait + 一个参考执行器，镜像 provider foundation slice 的"落地抽象 + 一个
sample、后续再接真引擎"模式。本轮范围仅 §E 的 E1/E2/E3（纯新增模块，零既有代码改动）；
E4（声明式 `providers.toml` + 懒构造）与生产 `Engine` 迁移延后。

- **E2 — `codesmith_agent::tools`**（`crates/agent/src/tools/mod.rs`）：`Tool` trait
  （LangChain `BaseTool` analog）——`name`/`description`/`input_schema`（默认
  `{"type":"object"}`）/`capabilities`（默认空）/`run(input) -> boxed Future`。**host-agnostic**：
  每个 impl 自持依赖、`run` 只收 parsed input，刻意不带 fat `ToolContext`（那住在
  `agent-runtime::tools::spec`，后续用 adapter 桥接）。`ToolSet`（`HashMap<String,
  Arc<dyn Tool>>`）+ `to_api_tools()` 把可执行 tool 转成 wire `models::Tool`。leaf 类型
  （`ToolResult`/`ToolError`/`ToolCapability`/`ApprovalRequirement`）复用 `codesmith-tools`
  并 re-export，避免第三份拷贝。3 个单测。
- **E3 — `codesmith_agent::memory`**（`memory/mod.rs`）：`ChatHistory` trait
  （LangChain `Memory` analog：`messages`/`push`/`clear`/`len`）+ `VecChatHistory` 默认实现。
  2 个单测。compaction 仍留 `agent-runtime::Session`/`compaction`，host 用 `Session` 背书
  `ChatHistory` 即可让执行器看到已压缩的消息列。
- **E3 — `codesmith_agent::callback`**（`callback/mod.rs`）：`Callback` trait
  （LangChain `Callbacks` analog：`on_llm_start`/`on_llm_end`/`on_tool_start`/`on_tool_end`/
  `on_step`/`on_complete`，全默认 no-op）+ `StopReason`（`NoToolCalls`/`MaxSteps`/`Error`）+
  `NoopCallback` + `CallbackSet`（fan-out，零拷贝转发）。单一 `'a` 把 `&self` 与各 `&` 参数
  绑一起，impl 可按引用转发 request/response。2 个单测。
- **E1 — `codesmith_agent::executor`**（`executor/mod.rs`）：`AgentExecutor` trait
  （`run(&mut dyn ChatHistory, user_text) -> Result<StopReason>`）+ `DefaultAgentExecutor`
  参考实现（LLM↔tool 循环 + `max_steps` 上限）+ `AgentExecutorConfig`。循环：push 用户消息 →
  建 `MessageRequest`（含 `to_api_tools()`）→ `on_llm_start` → `create_message_stream` →
  `accumulate_stream` 把 `StreamEvent` 归约成 `Vec<ContentBlock>` + `stop_reason` → `on_llm_end`
  → push assistant 消息 → 抽 `ToolUse` → 无则 `NoToolCalls` 收尾；否则逐个 `on_tool_start` →
  `Tool::run` / `NotAvailable` → `on_tool_end` → push `ToolResult`（`role:"user"`）→ `on_step`
  → 超过 `max_steps` 返 `MaxSteps`。`accumulate_stream` 处理 `ContentBlockStart`/`Delta`/`Stop`
  （text/thinking/tool_use，`InputJsonDelta` 累积 + `ContentBlockStop` 解析，缺 delta 时用
  start `input` 兜底）+ `MessageDelta`/`MessageStop`；无 early-start/transparent-retry/steer
  （生产护栏，延后）。5 个单测（inline `MockLlmClient` + `EchoTool` + `RecordingCallback`，
  不依赖 `codesmith-providers`）。
- **新依赖边**：`codesmith-agent → codesmith-tools`（cycle-free：`tools → protocol →
  {serde, serde_json}`，无回指 `agent`/`config`）。`crates/agent/src/lib.rs` 声明 4 个新模块
  并扩展 crate doc。async 风格沿用 `LlmClient` 的手动 `Pin<Box<dyn Future + Send + '_>>`，
  不引 `async-trait` 直依。
- **ARCHITECTURE.md**：新增"The framework-core agent seam (§E)"小节（含 ASCII 流程图）+
  "What is wired today"表 §E 行 `⏳ deferred` → `✅ framework-core traits landed`。

**验证：** `cargo +1.90.0 build -p codesmith-agent` 零警告；`cargo test -p codesmith-agent`
79 passed（含 12 个新单测）；`cargo build -p codesmith-providers` 与 `cargo build
--workspace` 全绿（纯新增，无既有调用点改动；tui 的 143 个 warning 均为既有死代码，与本轮无关）。

**后续 deferred（非本轮，后续 §E 切片）：**
- 生产 `Engine`/`turn_loop.rs`（~2400 行）迁移到 `AgentExecutor`/`Tool`/`ChatHistory`/
  `Callback`——"接真引擎"步，类比对 provider 的 §D1。
- `ToolSpec`+`ToolContext`（`agent-runtime::tools::spec`）→ 框架 `Tool` 的 adapter（捕获
  context 的桥接 struct，住 `agent-runtime`）。
- `Callback` ↔ 既有 `mpsc::Sender<Event>` 推送通道 / `HookHost` shell-command 钩子桥接。
- **E4**（声明式 `providers.toml` + `default_registry` 懒构造）维持原计划。

**下一聚焦工作：** §E 后续切片（生产 `Engine` 迁移 / `ToolSpec` adapter / `Callback` 桥接）或
E4。§D2 deferred 项、B3 仍低优先。

---

**进度（2026-07-08 §E ToolSpec→Tool adapter 落地，框架核心 tool 桥接，`feat/pluggable-framework-core`）：**

§E 的第二个切片落地——把生产 `ToolSpec`+`ToolContext`（`codesmith-agent-runtime`）桥接到框架核心
`Tool`（`codesmith-agent`），对应 ROADMAP §E "ToolSpec adapter" deferred 项。本轮纯新增（一个 adapter 模块 +
registry 一个方法），零既有调用点改动；production `Engine`/`turn_loop` 迁移仍 deferred（"接真引擎"步）。

- **`ToolSpecAdapter`**（`crates/agent-runtime/src/tools/framework_adapter.rs`）：持有 `Arc<dyn ToolSpec>` +
  `Arc<ToolContext>`（共享，clone 仅 refcount bump），`impl codesmith_agent::tools::Tool`——`name`/`description`/
  `input_schema`/`capabilities` 转发 spec；`run(input)` 把两个 Arc clone 进 `'static` future，委托
  `spec.execute(input, &context).await`。leaf 类型（`ToolResult`/`ToolError`/`ToolCapability`）经 `codesmith-tools`
  已共享，零翻译；deref-coercion 把 `&Arc<ToolContext>` → `&ToolContext`。
- **`ToolRegistry::to_framework_tool_set`**（`crates/agent-runtime/src/tools/registry.rs`）：把注册表里每个
  `ToolSpec` 包成 adapter，共享一个 `Arc<ToolContext>`（clone 一次），产出 `codesmith_agent::tools::ToolSet`。
  wire 定义由 `ToolSet::to_api_tools` 从转发的 `name`/`description`/`input_schema` 重建——同
  `codesmith_agent::models::Tool` 类型（`crate::models` 经 `agent-runtime/src/lib.rs:24` re-export 即
  `codesmith_agent::models`），零转换。
- **刻意不桥接（by design）**：`ToolSpec` 的 approval/parallelism/destructive/interactive/defer-loading 元数据
  在框架 `Tool` 无对应面（`capabilities` 仅 advisory），仍由 host 的 `ToolDispatcher`/`ToolMetadata` 承载。
  adapter 只是 executor 的 `run` 路径桥接，**非 `ToolDispatcher` 替代**——与 `tools/mod.rs` 模块文档的"刻意不带
  fat `ToolContext`"设计一致。
- **验证**：3 个单测——(1) 元数据转发 + `run` 委托（`EchoSpec` 把 `context.workspace` 路径戳进结果，证明 context
  流过）；(2) adapter coerce 成 `Arc<dyn Tool>` 注册进 `ToolSet` + `to_api_tools` 重建 wire def；(3) **executor 集成**：
  mock LLM 驱动 `DefaultAgentExecutor` 经 adapter 跑 tool-call roundtrip，回灌的 `ToolResult` 带 captured context 的
  workspace 路径——证明真实 `ToolSpec` 经 adapter 在框架 executor 循环里双向跑通。`cargo build -p
  codesmith-agent-runtime` 零新警告；`cargo test -p codesmith-agent-runtime` 999 passed（含 3 个新）；`cargo build
  --workspace` 全绿（tui 143 warning 均既有死代码）。
- **无新依赖边**：`agent-runtime` 已依赖 `codesmith-agent`（`Cargo.toml:14`）并 re-export `models`
  （`lib.rs:24`），故本切片零 Cargo 改动。

**下一聚焦工作：**
- **Callback 桥接**：把 `mpsc::Sender<Event>` + `HookHost` 桥接到框架 `Callback`（`crates/agent/src/callback/`）。
  已知映射缺口：`on_llm_start`（无精确 event，`TurnStarted` 只带 turn_id）/`on_llm_end` content（`MessageComplete`
  只带 index，content 在 engine 内不在 wire）/`on_step`（`Event` 无 step 变体）；streaming delta（`MessageDelta`/
  `ThinkingDelta`）无法经 `Callback` 路由，Event 通道仍需保留。是 Engine 迁移的另一个前置。
- **生产 `Engine`/`turn_loop` 迁移**：`handle_deepseek_turn`（`turn_loop.rs:239-2721`，~2483 行）迁到
  `AgentExecutor`——需 ToolSpec adapter（已就位）+ Callback 桥接先就位；guardrail（compaction/capacity/approval/
  early-tool-start/steer/transparent-retry/subagent/LSP/cycle/loop-guard）在 `DefaultAgentExecutor` 不存在，需增量
  迁移或作为 host 前后置逻辑保留。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-08 §E Callback 桥接落地，框架核心 callback 桥接，`feat/pluggable-framework-core`）：**

§E 的第三个切片落地——把生产 host 的两条观测通道（`mpsc::Sender<Event>` UI 推送 +
`HookHost` shell-command 钩子）桥接到框架核心 `Callback`（`codesmith-agent`），对应 ROADMAP
§E "Callback 桥接" deferred 项，也是 `Engine`/`turn_loop` 迁移的两个前置之一（另一个
`ToolSpec`→`Tool` adapter 已落地）。本轮纯新增（一个 adapter 模块），零既有调用点改动；
production `Engine`/`turn_loop` 迁移仍 deferred（"接真引擎"步）。

- **`CallbackBridge`**（`crates/agent-runtime/src/callback_bridge.rs`）：`impl
  codesmith_agent::callback::Callback`，持有 `Option<mpsc::Sender<Event>>` +
  `Option<Arc<dyn HookHost>>` + turn 级 `HookContext` 模板 + `Arc<Mutex<BridgeState>>`
  （合成 id + 暂存 input 的 LIFO 栈）。`on_tool_start`/`on_tool_end` **双发**：既推
  `Event::ToolCallStarted`/`ToolCallComplete` 到 UI 通道，又
  `HookHost::execute(ToolCallBefore/After, &ctx)` 触发 shell 钩子；`has_hooks_for_event`
  门控与生产 `execute_pre/post_tool_hook` 一致，`HookContext` 由模板 clone + per-call 填
  `tool_name`/`tool_args`/`tool_result`，镜像 `build_tool_hook_context`。
- **合成 tool-call id**：框架 `Tool::run`/`on_tool_start` 契约是 id-less 的（只有 parsed
  input），不携带 wire `ToolUse.id`。bridge 合成 `bridge-{n}` id 并用 LIFO 栈配对
  start↔end，使 UI 仍能关联；同时把 start 暂存的 `input` 回放到 `ToolCallAfter` 的
  `tool_args`——框架 `on_tool_end(name, result)` 签名不含 input，但生产 post-hook 需要
  `tool_args`，pending 栈补上该缺口。**不改**已落地的 §E `Callback` trait 签名（扩 id 会
  波及 executor + 全部 §E 测试，超出本轮）。
- **刻意部分桥接（by design）**：`on_llm_start`/`on_llm_end`/`on_step`/`on_complete` 不覆写，
  沿用 trait 默认 no-op——见模块 doc 的 "Bridged vs. documented gaps" 表。原因：`on_llm_start`
  无精确 event（`TurnStarted` 只带 turn_id 且由 engine caller 而非 executor 发）；
  `on_llm_end` content 不在线（`MessageComplete` 只带 block index，由 stream-reduction 代码
  拥有）；`on_step` 无 event 变体；`on_complete` 的 `TurnComplete` 携带 `Callback` 没有的
  `usage`/`tool_catalog`/`base_url`，由 engine caller 在 executor 返回后发，bridge 不重复。
  流式 delta（`MessageDelta`/`ThinkingDelta`）无 `Callback` 方法，**Event 通道保留**，由
  stream-reduction 代码直发——bridge 是叠加而非替代。
- **类型统一**：`Event` 用 `codesmith_tools::{ToolError, ToolResult}`（`events.rs:19`），与
  框架 `Callback`/`Tool` re-export（`tools/mod.rs:35`）同型，故
  `Event::ToolCallComplete { result: result.clone() }` 零翻译直过——与 ToolSpec adapter 一致。
- **验证**：3 个单测——(1) 单元：`on_tool_start`/`on_tool_end` 双发到 mock `mpsc` 通道 +
  `RecordingHookHost`（test `HookHost` impl），断言 `ToolCallStarted`/`ToolCallComplete` id
  配对 + name/input/result，`ToolCallBefore`/`After` 的 `tool_name`/`tool_args`/`tool_result`/
  `session_id`（template 流过）；(2) `hooks: None` 仍发 event；(3) **executor 集成**：mock LLM
  驱动 `DefaultAgentExecutor` 跑 tool-call roundtrip，`CallbackBridge` 作 `Arc<dyn Callback>`
  ——单一 seam 同时点亮 UI event 通道与 shell 钩子。`cargo build -p codesmith-agent-runtime`
  零新警告；`cargo test -p codesmith-agent-runtime` 1002 passed（含 3 新，原 999）；
  `cargo test -p codesmith-agent` 79 passed；`cargo build --workspace` 全绿（tui 143 warning 均
  既有死代码，与本轮无关）。
- **无新依赖边**：`agent-runtime` 已依赖 `codesmith-agent`，`Event` + `HookHost` 在树内，故
  本切片零 Cargo 改动（与 ToolSpec adapter 切片一致）。

**下一聚焦工作：**
- **生产 `Engine`/`turn_loop` 迁移**（§E "接真引擎"步）：`handle_deepseek_turn`
  （`turn_loop.rs:239-2672`，~2434 行）迁到 `AgentExecutor`。两个前置现已就位：`ToolSpec`→`Tool`
  adapter + `CallbackBridge`。迁移需 per-turn 构造 `CallbackBridge`（注入 turn 级 `HookContext`
  模板 + `tx_event` + `Arc<dyn HookHost>`）交给 executor，并把 inline `tx_event.send`/
  `execute_pre/post_tool_hook` 调用替换为 executor 回调驱动。guardrail（compaction/capacity/
  approval/early-tool-start/steer/transparent-retry/subagent/LSP/cycle/loop-guard）在
  `DefaultAgentExecutor` 不存在，需增量迁移或作为 host 前后置逻辑保留；stream-reduction 的
  `MessageDelta`/`ThinkingDelta` 直发 `tx_event` 保留（不经 `Callback`）。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项（CLI flag / per-entry 写入 / 裸形式）、
  B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-08 §E `SessionChatHistory` 桥 + `HostAgentExecutor` 骨架落地，三桥完成，`feat/pluggable-framework-core`）：**

§E 的第四个切片落地——补齐第三个、也是最后一个 host→framework 桥（`Session` → `ChatHistory`），并落地承载
生产 turn loop 迁移的 `HostAgentExecutor` 骨架（裸 LLM↔tool 循环，无 guardrail）。两个前置桥
（`ToolSpecAdapter`、`CallbackBridge`）已就位；本切片完成三桥组合证明，并确立"循环住在 host executor"
的结构，为后续逐个吸收 guardrail 铺路。本轮纯新增（两个模块 + 核心一处 visibility 放宽），零既有调用点
改动；生产路径 `handle_deepseek_turn` 不受影响。

- **`SessionChatHistory`**（`crates/agent-runtime/src/session_history.rs`，crate-root `pub mod`，镜像
  `callback_bridge`）：`impl ChatHistory for SessionChatHistory<'a>`，持 `&'a mut Session`，四方法纯委托
  `session.messages` / `session.add_message`（后者即 `push`，零行为差异）。`Session: Send + Sync` 经编译
  验证（`&mut Session` 满足 `ChatHistory: Send + Sync` 的 trait object bound，无需回退到
  `&mut Vec<Message>` 预案）。`push` 刻意不做 working_set 观察——那是 host guardrail 的职责，非 memory
  trait（与 `codesmith_agent::memory` 模块文档一致）。2 个单测。
- **`HostAgentExecutor`**（`crates/agent-runtime/src/engine/host_executor.rs`，`engine/` 下 `pub mod`）：
  `impl AgentExecutor for HostAgentExecutor`，持 `LlmClientHandle` + `Arc<ToolSet>` + `Arc<dyn Callback>` +
  `AgentExecutorConfig`。`run_inner` **镜像 `DefaultAgentExecutor::run_inner`** 的裸循环，复用核心
  `codesmith_agent::executor::accumulate_stream`（pub 放宽后）做 stream 归约。循环标注四个 guardrail
  插入点（per-step pre-request / post-stream / per-tool / post-tool），后续切片增量填入。模块文档显式
  声明：未接入 `handle_send_message`，生产 `handle_deepseek_turn` 仍是 live path；stream delta
  （`MessageDelta`/`ThinkingDelta`）将来由 inline 归约直发 `tx_event`（不经 `Callback`）。3 个单测。
- **核心 visibility 放宽**：`crates/agent/src/executor/mod.rs` 的 `accumulate_stream` 由私有 `async fn` 改
  `pub async fn`——非行为变更，使 host executor 复用 ~100 行归约器而非复制。核心 `accumulate_stream` 作为
  可复用 helper 暴露（简单/测试用；生产将来改 inline 归约时核心此函数仍留给简单路径）。
- **三桥组合证明**（headline `host_executor_drives_full_bridge_trio`）：`ToolRegistry` 注册真
  `EchoSpec`（`impl ToolSpec`，把 `context.workspace` 戳进结果）→ `to_framework_tool_set()` 得 `ToolSet`；
  真 `Session` → `SessionChatHistory`；`CallbackBridge{ mock mpsc, RecordingHookHost, hook_template }` 作
  `Arc<dyn Callback>`；`MockLlm` 两轮（text+tool_use(echo) / text+end_turn）。断言 `StopReason::NoToolCalls`、
  `sess.messages.len()==4`、`ToolResult` content 以 captured workspace 路径开头（证明 `ToolSpec` 经
  `ToolSpecAdapter` 流过 + context 捕获）、mock mpsc 收到 `ToolCallStarted`+`ToolCallComplete`（id 配对）、
  `RecordingHookHost` 收到 `ToolCallBefore`+`ToolCallAfter`（`tool_name`/`session_id`/`tool_result`/
  `tool_success` 齐全）——三桥在框架 executor 循环里端到端组合跑通。另两个小测：
  `missing_tool_records_error_result`（未注册 tool → `NotAvailable` 错误 `ToolResult`，行为对齐核心
  executor）、`exhausts_steps`（`max_steps:2` → `MaxSteps`）。
- **无新依赖边**：`agent-runtime` 已依赖 `codesmith-agent`；`tempfile`/`async-trait`/`futures-util` 既有；
  零 Cargo 改动。
- **验证**：`cargo +1.90.0 build -p codesmith-agent` 零警告；`cargo build -p codesmith-agent-runtime` 零新
  警告；`cargo test -p codesmith-agent` 79 passed；`cargo test -p codesmith-agent-runtime --lib` 1007 passed
  （含 5 新，原 1002）；`cargo build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- **guardrail 逐个吸收**：把 `handle_deepseek_turn` 的 10 个 guardrail 增量迁入 `HostAgentExecutor` 的四个
  插入点。优先级建议——先迁自包含、本地状态者（loop-guard、LSP flush），再迁需 `Engine` 可变状态者
  （compaction、capacity、approval、steer、transparent-retry、early-tool-start、subagent）。`&self` vs
  `&mut self` 阻抗在首个需 `Engine` 可变状态的 guardrail 切片解决（interior-mutability handles：
  `Arc<Mutex<...>>` / 通道）。
- **`HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役**：所有 guardrail 吸收后，`handle_send_message`
  改用 `HostAgentExecutor`，删 `handle_deepseek_turn`。
- stream delta inline 归约 / `on_llm_*` 桥接 / wire tool-call id 透传——后续切片。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项（CLI flag / per-entry 写入 / 裸形式）、
  B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-09 §E loop-guard 吸收落地，首个 guardrail 进 HostAgentExecutor，`feat/pluggable-framework-core`）：**

§E 的第五个切片落地——把生产 `handle_deepseek_turn` 的 10 个 guardrail 中的第一个（loop-guard）吸收进
`HostAgentExecutor`。loop-guard 是最干净的入口：`LoopGuard`（`engine/loop_guard.rs`）是纯数据结构
（`call_counts`/`failure_counts` 两个 HashMap），无 Engine 可变状态、无 `&mut self` 阻抗，是热身切片。
本轮纯增量（`host_executor.rs` 一个文件 + 文档），零既有调用点改动；生产路径 `handle_deepseek_turn` 不受影响。

- **`event_tx` 注入**：`HostAgentExecutor` 新增 `event_tx: Option<mpsc::Sender<Event>>` 字段 + 构造器参数 +
  `emit_status` helper。guardrail 状态 surfacing 走 host 的 `Event` 通道，**不**经框架 `Callback`（§E 明确
  不改 `Callback` trait 签名；guardrail 是 host 侧关注，`Callback` 是框架 loop 的 tool 生命周期钩子）。这是
  后续 steer/LSP/capacity/subagent 也会复用的基础注入。
- **seam (3) per-tool `record_attempt`**：`on_tool_start` 后、`tool.run` 前插 `LoopGuard::record_attempt`——
  第 3 次相同（name+args）调用被 block，回灌 `ToolResult::error(...).with_metadata({"loop_guard":
  "identical_tool_call"})`（镜像 `turn_loop::loop_guard_block_tool_result`，本轮内联、注明轻微重复、后续清理），
  `is_error=true`，仍发 `on_tool_end`（与既有 `NotAvailable` 路径一致）。blocked call 不调 `record_outcome`
  （guard 干预非执行 outcome，不计入失败 halt）。
- **seam (3) per-tool `record_outcome`**：`tool.run` 后调 `LoopGuard::record_outcome(name, success)`——
  `Warn`（3 次连续失败）→ `emit_status`；`Halt`（8 次）→ 暂存 per-step `loop_guard_halt`。
- **seam (4) post-tool halt**：tool 循环后 `loop_guard_halt` 有值则 `emit_status` + `on_complete(Error(msg))` +
  返 `StopReason::Error(msg)`（语义贴切「run 中止带原因」；不新加 `Halt` variant，避免动核心 enum）。
- **`&self` 维持**：`LoopGuard` 是 `run_inner` 局部变量（跨单次 run 内 loop 迭代存活，匹配 turn_loop:281），
  `Sender::send` 取 `&self`——本切片是「本地状态 guardrail 无需 interior-mutability」的证明。首个需 Engine
  可变状态的 guardrail（LSP 的 `pending_lsp_blocks` / steer 的 `rx_steer`）才引入 `Arc<Mutex<...>>`/通道。
- **3 个新单测**：`loop_guard_blocks_third_identical_call`（3 次相同 echo 调用，第 3 次 block、echo 真跑 2 次、
  block 消息 + is_error）、`loop_guard_warns_at_three_failures`（ghost tool + 变参避开 block，第 3 次失败
  `Event::status` "failed 3 consecutive times"）、`loop_guard_halts_after_eight_failures`（8 次失败 →
  `StopReason::Error` "failed 8 consecutive times" + halt status 事件）。既有 3 测试构造器调用改传 `event_tx: None`。

**已知设计取舍：**
- **blocked call 仍发 `on_tool_start`/`on_tool_end`**：与 `NotAvailable` 路径一致；生产中 guard-blocked call
  是否经 pre/post tool hook 未逐一核对，本切片选框架一致性优先。
- **halt = `StopReason::Error`**：携带 halt 消息；若后续需区分「guardrail halt」与「运行错误」可再加 `Halt` variant。
- **`block_tool_result` 轻微重复**：与 `turn_loop::loop_guard_block_tool_result` 同实现；后续可 lift 进
  `loop_guard` 模块作单一源（与 reasoning.rs 去重同性质）。

**下一聚焦工作：**
- **下一个 guardrail**：建议 **transparent-retry**（seam 2 post-stream；本地计数器 + `cancel_token`/`tx_event`/
  `client`，单 seam 为主）或 **LSP flush**（seam 1 pre-request；需 `pending_lsp_blocks`，是首个引入
  interior-mutability 的候选）。loop-guard 已证明 `&self` + 局部状态模式可行。
- 其余 guardrail（compaction/capacity/approval/steer/early-tool-start/subagent/cycle）+ stream delta inline
  归约 / `on_llm_*` 桥接 / wire tool-call id 透传 / `HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役——
  后续切片。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-09 §E LSP flush 吸收落地，首个 interior-mutability 切片，`feat/pluggable-framework-core`）：**

§E 的第六个切片落地——把生产 `handle_deepseek_turn` 的 10 个 guardrail 中的第二个（LSP diagnostics flush）吸收进
`HostAgentExecutor`。这是首个需要 `Engine` 可变状态（`pending_lsp_blocks`）的 guardrail，因此首次引入
interior-mutability（`Arc<std::sync::Mutex<Vec<DiagnosticBlock>>>`），是继 loop-guard（本地状态热身）之后的
「需共享可变状态 guardrail」的形状证明。本轮纯增量（`host_executor.rs` + `lsp_hooks.rs` 两文件 + 文档），零既有
调用点改动；生产路径 `handle_deepseek_turn` 不受影响。

- **`LspProbe` collaborator**：新增 `LspProbe { manager: Arc<dyn LspManagerApi>, workspace: PathBuf,
  pending: Arc<std::sync::Mutex<Vec<DiagnosticBlock>>> }`，作为 `HostAgentExecutor` 的 `lsp: Option<LspProbe>` 字段
  （构造器新增一个参数，非-LSP embed/测试传 `None`——6 个既有测试构造器改传 `None`）。`pending` 是
  interior-mutability 句柄：`AgentExecutor::run` 是 `&self`，而累加器在 collect 时 push、flush 时 `mem::take`
  ——锁从不在 `await` 跨点持有（匹配 `CallbackBridge` 先例）。`pending` 跨 `run()` 调用持久（匹配生产
  `Engine.pending_lsp_blocks` 字段语义——以 `MaxSteps` 结束的 turn 其 edit 的诊断在下一 turn 首 flush 浮现）；
  一个 per-`run_inner` 局部 `Vec` 无法跨 run 持久，故必须是 executor 字段上的 `Arc<Mutex<…>>`。
- **最小注入而非整 `HostServices`**：注入 `Option<Arc<dyn LspManagerApi>>`（2 方法 trait：`config() -> &LspConfig`
  sync、`diagnostics_for(&self, &Path, u64) -> Option<DiagnosticBlock>` async，Send+Sync），**不**注入完整
  `HostServices`（~13 方法 + 子 trait，fake 代价过高）。`preflight_apply_patch` 从 agent-runtime 不可达（循环依赖：
  `codesmith-tool-impls` 依赖 `codesmith-agent-runtime`）——故 apply_patch 路径推导延后（见已知取舍）。
- **seam (3) per-tool `collect_lsp_diagnostics`**（async）：在 `loop_guard.record_outcome` 块之后、`ToolResult` push
  之前插；gate `!blocked && result.is_ok() && r.success`（镜像生产 `output.success && tool_was_executed`）。master
  switch `probe.manager.config().enabled` 先判；路径推导经共享 helper `edit_file_paths`（仅 edit_file/write_file），
  join workspace 得绝对路径，`await diagnostics_for(&absolute, 0)`，push 进 `probe.pending`。edit_seq=0（仅 log 关联；
  生产用 turn_counter）。
- **seam (1) per-step pre-request `flush_pending_lsp_diagnostics`**（**SYNC**——内部无 await）：`mem::take` pending，
  `render_lsp_blocks` 渲染，push 纯 user text `Message`（`ChatHistory::push`，落进真实 Session transcript）。插在
  `run_inner` 循环顶 `step >= max_steps` bail 之后、`tools.to_api_tools()`/`MessageRequest` snapshot 之前——使诊断
  消息进入模型所见请求（镜像 `turn_loop.rs:494` before:501）。
- **`lsp_hooks.rs` 共享 helper**：新增 `pub fn edit_file_paths(&Value) -> Vec<PathBuf>`，重构
  `edited_paths_for_tool` 的 `edit_file|write_file` 臂调它（去重，零行为变化；executor 与生产共享单一源）；
  apply_patch 臂不动。
- **6 个新测试**：`lsp_collect_then_flush_feeds_model`（edit_file run → FakeLsp 返 ERROR block → history 出现含
  `<diagnostics` 的第二条 user 消息且在 call2 请求前；MockLlm 捕获的 call2 请求含它）、`lsp_disabled_skips_collect`
  （`config().enabled=false` → 无诊断消息、`FakeLsp.calls` 空）、`lsp_skips_non_edit_tool`（echo 工具 → 无路径推导 →
  无诊断）、`lsp_skips_failed_edit`（EditSpec 返 `Ok(success:false)` → 不 collect）、`lsp_apply_patch_paths_deferred`
  （钉住 apply_patch 当前不收集，记录缺口）、`lsp_cross_turn_persistence_via_shared_state`（**interior-mutability
  证明**：max_steps:1；run1 edit → MaxSteps 留 pending 非空；run2 在**同一 executor** + 新 Session → 其首 pre-request
  flush 把 run1 残留 pending 排进 run2 history；assert run2 首请求含 run1 edit 的诊断——per-`run_inner` 局部 Vec
  做不到，证 `Arc<Mutex<Vec>>` 跨 run 持久）。
- **test doubles 扩展**：`MockLlm` 加 `requests: Mutex<Vec<Vec<Message>>>` + `requests()` 记录每次请求消息（增量，
  既有测试不受影响）；新增 `FakeLsp`（`LspManagerApi` impl，`returning(block)`/`disabled()` 构造器返 `Arc<Self>`，
  `calls()` 记 `(PathBuf, u64)` 探针）、`EditSpec`/`FailingEditSpec`（ToolSpec impls）、`error_diag_block` +
  `has_diagnostics_msg` helper。

**验证：** `cargo build -p codesmith-agent-runtime` 零新 warning；`cargo test -p codesmith-agent-runtime --lib` 1016
通过（0 失败、2 ignored）；`cargo test -p codesmith-agent-runtime --lib host_executor` 12 通过（6 既有 + 6 新 LSP）；
`cargo test -p codesmith-agent --lib` 79 通过；tui `edited_paths_for_*`（6）+ `parse_patch_paths`（1）helper 重构后
全 7 通过；`cargo build --workspace` 绿（tui 143 warning 均既有死代码，与本轮无关）。

**已知设计取舍（本轮缺口，by design）：**
- **apply_patch 路径推导延后**：需 `HostServices::preflight_apply_patch_paths`（从 agent-runtime 不可达，循环依赖）；
  本轮 apply_patch 在 collect 中返空 Vec，live `handle_deepseek_turn` 仍覆盖；待 executor 接真 `HostServices` 或
  注入 resolver-closure 时补。
- **合成 flush 消息无 `<turn_meta>`**：`user_text_message_with_turn_metadata` 仅读 session+config，但框架 executor 路径
  全无 turn_meta（跨切 host 侧富化，延后到自己的切片）——故 flush 用纯 user text 消息。
- **合成 push 无 `emit_session_updated`**：与 executor 其余消息 push 一致；UI surfacing 延后到 wire-in 步。

**下一聚焦工作：**
- **下一个 guardrail**：建议 **transparent-retry**（seam 2 post-stream；本地重试计数器 + `cancel_token`/`tx_event`/
  `client`，单 seam 为主）——loop-guard + LSP flush 已落地，transparent-retry 是下一个自包含候选。或
  **steer/capacity**（需更多 `Engine` 可变状态，沿用本轮 `Arc<Mutex<…>>` 形状）。
- **apply_patch 路径推导**延后（待 `HostServices` 可达或注入 resolver-closure）。
- 其余 guardrail（compaction/approval/early-tool-start/subagent/cycle）+ stream delta inline 归约 / `on_llm_*` 桥接 /
  wire tool-call id 透传 / `HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役——后续切片。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-10 §E transparent-retry 吸收落地，首个 seam-2 guardrail，`feat/pluggable-framework-core`）：**

§E 的第七个切片落地——把生产 `handle_deepseek_turn` 的 10 个 guardrail 中的第三个（transparent stream-retry）吸收进
`HostAgentExecutor`。这是首个 seam-2（post-stream）guardrail，也是继 loop-guard（本地状态热身）、LSP flush（interior-mutability）之后
回到「本地状态」形状的切片——重试计数器是 per-run `u32`，沿用 loop-guard 的 `&self` + 局部变量模式。本轮纯增量
（`host_executor.rs` 一个文件 + 文档），零既有调用点改动；生产路径 `handle_deepseek_turn` 不受影响。

- **吸收的是外层重试（seam 2 post-stream）**：生产有**两层**重试——内层（`transparent_stream_retries`，MAX=2）在流消费循环内，
  当流在产出任何内容前报错时立即重发 `create_message_stream`；外层（`stream_retry_attempts`，MAX=3）在流消费完之后，当「流带错误死亡且
  无任何可操作内容」时 `continue` 重跑整轮。内层重试与 inline 流归约（deferred）耦合，本轮不碰；本轮吸收的是外层。由于
  `accumulate_stream` 在首个 erroring 流项即 `?` 返回 `Err` 并丢弃已累积的块，一个 `Err` 即等价于「无可操作内容已提交」——外层重试
  天然覆盖了 `accumulate_stream` 路径下的两层语义（内层在 inline 归约切片落地后才有意义）。
- **`stream_with_transparent_retry`**（`host_executor.rs`，seam (2)）：`on_llm_start` 后、`on_llm_end` 前的 helper，循环
  `create_message_stream(request.clone())` + `accumulate_stream`。`Ok` → 重置预算、返回内容；`Err`（mid-flight 流死亡）→ 预算内
  （`< MAX_STREAM_RETRIES = 3`）则 `emit_status("Connection interrupted; retrying (n/3)")` 后 `continue` 重发，预算耗尽则传播 `Err`。
  重试对 `Callback` **透明**：`on_llm_start`/`on_llm_end` 每步只各发一次，唯一 surfacing 是 `Status` 事件（镜像生产的静默重发）。
  `request.clone()` 复用 wire `MessageRequest: Clone`（生产 `turn_loop.rs:613` 同款 clone）。
- **预算跨步持久 + 健康轮重置**：`stream_retry_attempts: u32` 是 `run_inner` 局部变量，声明在 `loop_guard`/`step` 旁（匹配生产
  `turn_loop.rs:292` 的 per-turn 计数器）；`stream_with_transparent_retry` 在 `Ok` 路径把它重置为 0（镜像 `turn_loop.rs:1186`），故一个坏步
  的预算不带到下一步——headline 测试 `transparent_retry_resets_budget_across_steps` 用「两步各重试一次、两 status 均为 1/3」证之
  （若不重置，第二步首重试会是 2/3）。
- **`MockRound` test double**：`MockLlm` 从 `Vec<Vec<StreamEvent>>` 升级为 `Vec<MockRound>`（`Events(Vec<StreamEvent>)` | `StreamErr(String)`）。
  `StreamErr` 让流产出单个 `Err` 项 → `accumulate_stream` 返回 `Err`（模拟 mid-flight 流死亡，#103「stream died with nothing」）。
  `MockLlm::new`（既有签名）把每个 `Vec<StreamEvent>` 包成 `Events`，故 12 个既有测试零改动；新测试用 `with_rounds` 注入 `StreamErr`。
  `MockLlm` 现持 `Arc`（测试持一份 `.clone()` 句柄调 `requests()` 数 `create_message_stream` 次数，另一份经 unsized coercion 喂 executor）。
- **刻意部分桥接（by design）**：
  - **`accumulate_stream` bail-on-error**：核心 reducer 在首个 erroring 流项即 `?` 返回 `Err` 并丢弃部分块，故 `Err` 恒为「无可操作内容」。
    这使本切片在生产本应 ship 部分内容时（生产 inline 跟踪 `any_content_received`、用户已见输出则不重试）仍重试。因部分内容已丢，
    重试是唯一恢复路径；double-billing（provider 计了部分 output 的费）是 provider 侧而非用户可见。inline 流归约（后续切片替换
    `accumulate_stream` 调用）闭合该缺口。
  - **pre-stream 连接错误不重试**：`create_message_stream` 返回 `Err`（连接拒绝 / auth / context-length）径直 `?` 硬失败。生产把这类当
    context-recovery / 硬失败（独立 guardrail，deferred）；只 mid-flight 流错误重试。
  - **无 cancel-token 短路**：生产 `should_transparently_retry_stream` 查 `!cancelled` 在取消的 turn 上中止重试。本 executor 尚不持
    `CancellationToken`；有界预算（`MAX_STREAM_RETRIES = 3`）防死循环，故取消的 turn 至多浪费 3 次快速重试后失败。短路在 wire-in 步
    （executor 接真 `Engine` 的 cancel token 时）接入。刻意不做本轮——为守「单 seam 为主」，避免 cancel_token 字段 + 12 处测试构造器改动的
    churn（cancel threading 是 host 接入关注，非 seam-2 重试机制本身）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零新 warning；`cargo test -p codesmith-agent-runtime --lib host_executor` 16 通过
（12 既有 + 4 新 transparent-retry）；`cargo test -p codesmith-agent-runtime --lib` 1020 通过（0 失败、2 ignored，原 1016 +4）；
`cargo test -p codesmith-agent --lib` 79 通过；`cargo build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- **下一个 guardrail**：建议 **steer** 或 **capacity**——两者都需更多 `Engine` 可变状态，沿用 LSP flush 的 `Arc<Mutex<…>>` 形状
  （steer 的 `rx_steer`、capacity 的 token 预算）。loop-guard + transparent-retry 已证本地状态 seam 可行，LSP flush 已证共享可变状态 seam 可行。
- 或 **early-tool-start**（seam 2，在流归约中检测 tool_use 起始并提前 dispatch——需 inline 流归约，与 transparent-retry 的 inline 归约依赖同源）。
- **inline 流归约**（替换 `accumulate_stream` 调用）：闭合 transparent-retry 的 `accumulate_stream` bail-on-error 缺口 + 接通 stream delta
  （`MessageDelta`/`ThinkingDelta`）直发 `tx_event` + early-tool-start。是多个 guardrail 的共同前置。
- **cancel-token 注入**：transparent-retry 短路 + loop 顶取消检查，在 wire-in 步或单独小切片接入。
- 其余 guardrail（compaction/approval/subagent/cycle）+ `on_llm_*` 桥接 / wire tool-call id 透传 / `HostAgentExecutor` 接入 +
  `handle_deepseek_turn` 退役——后续切片。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-10 §E steer 吸收落地，第四个 guardrail，seam-1 pre-request，`feat/pluggable-framework-core`）：**

§E 的第八个切片落地——把生产 `handle_deepseek_turn` 的 10 个 guardrail 中的第四个（steer 输入排空）吸收进
`HostAgentExecutor`。steer 让用户在 in-flight turn 中注入额外文本输入；生产在 `turn_loop.rs:300-317`（loop 顶部、
LLM 请求之前）以 `try_recv` 非阻塞排空 `rx_steer`，每条 steer trim→skip-empty→push `user` 消息→发 status，使模型
在本步请求中看到。本切片吸收的是**pre-request 排空（seam 1）**——生产另有三个流生命周期相关的次级排空点
（mid-stream buffer、post-stream resume、subagent hold 阻塞 `recv`），需 inline 流归约 / subagent 支持，延后。
本轮纯增量（`host_executor.rs` 一个文件 + 文档），零既有调用点改动；生产路径 `handle_deepseek_turn` 不受影响。

- **`steer` 字段**：`HostAgentExecutor` 新增 `steer: Option<Arc<std::sync::Mutex<mpsc::Receiver<String>>>>`
  字段 + 构造器参数（非-steer embed/测试传 `None`——16 个既有测试构造器改传 `None`）。**interior-mutability**：
  `AgentExecutor::run` 是 `&self`，而 `mpsc::Receiver::try_recv` 取 `&mut self`——与 LSP flush 的
  `LspProbe.pending: Arc<Mutex<Vec<DiagnosticBlock>>>` 同形。锁仅在同步 `try_recv` 时持有（不跨 `await`，
  匹配 `CallbackBridge` 先例）。receiver 跨 `run` 调用持久（匹配生产 `Engine.rx_steer` 字段语义——
  turn 间排入的 steer 在下一 turn 首 pre-request 排空被取出）。
- **`drain_steers`（seam 1 pre-request）**：`on_llm_start` 前、`flush_pending_lsp_diagnostics` 前插（匹配生产顺序：
  steer 在 loop 顶 `turn_loop.rs:300`，LSP flush 在 `turn_loop.rs:494`）。`try_recv` 循环：取 `&self.steer`，
  `Arc<Mutex>` 锁内 `try_recv`（`Ok` → trim → skip-empty → `history.push(user text)` → `emit_status`
  `"Steer input accepted: {summarize_text(120)}"`；`Err(Empty/Disconnected)` → break）。`summarize_text`
  经 `use super::summarize_text` 引入（`engine/mod.rs` 的私有 `use` binding 对后裔模块可见）。
- **刻意部分桥接（by design）**：
  - **不调 `working_set.observe_user_message`**：`ChatHistory` trait 不暴露 working set（host 侧关注，延后到
    wire-in 步）；steer 消息是纯 `user` text，与 LSP flush 的合成消息一致。
  - **无 `<turn_meta>` 富化**：生产用 `user_text_message_with_turn_metadata` 包 steer（date/model/working_set/
    skills），但框架 executor 路径全无 turn_meta（与 LSP flush 同缺口，延后到自己的切片）。
  - **三个次级排空点延后**：mid-stream buffer（`turn_loop.rs:683/721`，流消费循环内 `try_recv` 缓冲
    `pending_steers`）、post-stream resume（`turn_loop.rs:1297`，无 tool calls 时 drain `pending_steers` 并
    `continue`）、subagent hold 阻塞 `recv`（`turn_loop.rs:1347`，`biased select!` 等 subagent 完成 / steer）。
    三者分别需 inline 流归约 / subagent 支持，延后。本切片只吸收 pre-request 排空——steer 在 turn 开始前排入
    或在步间排入（下一步首 drain 取出）即被覆盖。
- **5 个新测试**：`steer_drain_injects_queued_input_before_request`（2 条 steer 预排队→transcript 出现 2 条
  user 消息、且模型唯一请求含这两条）、`steer_none_is_noop`（`steer: None`→无额外消息、NoToolCalls）、
  `steer_skips_empty_and_whitespace`（空/纯空白字符串→全 skip、无额外消息）、`steer_emits_status_per_accepted_input`
  （2 条 steer→2 条 `"Steer input accepted"` status）、`steer_picks_up_input_queued_between_runs`（**receiver
  持久证明**：run1 无 steer 干净收尾→turn 间排入 1 条 steer→run2 在同一 executor + 新 Session 首 pre-request
  drain 取出该 steer→transcript 出现、且 run2 请求含它——per-run 局部 receiver 做不到，证 `Arc<Mutex<Receiver>>`
  跨 run 持久）。`steer_channel()` helper 封装 `mpsc::channel::<String>(64)` + `Arc::new(Mutex::new(rx))`。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零新 warning（build 0 warning；test build 9 warning 均既有，
与本轮无关）；`cargo test -p codesmith-agent-runtime --lib host_executor` 21 通过（16 既有 + 5 新 steer）；
`cargo test -p codesmith-agent-runtime --lib` 1025 通过（0 失败、2 ignored，原 1020 +5）；`cargo test -p codesmith-agent --lib`
79 通过；`cargo build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**已知设计取舍（本轮缺口，by design）：**
- **三个次级排空点延后**：见上"刻意部分桥接"——需 inline 流归约 / subagent 支持。
- **无 `working_set.observe_user_message` / 无 `<turn_meta>`**：与 LSP flush 同缺口，延后到 wire-in / turn_meta 切片。
- **无 cancel-token stale-drain**：生产在 `handle_send_message` 开头（`mod.rs:1013-1014`）`while self.rx_steer.try_recv().is_ok() {}`
  排空前 turn 残留 steer；本 executor 无 `CancellationToken` 也无 stale-drain——turn 间残留 steer 会泄漏到下一 turn
  首 drain（被当作新输入注入）。生产靠 stale-drain 在 turn 开始前清空；短路在 wire-in 步接入。

**下一聚焦工作：**
- **下一个 guardrail**：建议 **capacity**（seam 1 pre-request + seam 4 post-tool；`CapacityController` 默认 disabled，
  硬 token-budget preflight 无条件运行——需 `api_provider` + token 计数 + `recover_context_overflow` 级联，
  是迄今最重 guardrail，可能需拆多切片）或 **approval**（seam 3 per-tool；审批门控，本地状态为主）。
  steer 已证 `Arc<Mutex<Receiver>>` seam-1 可行，loop-guard/transparent-retry 已证本地状态可行。
- **inline 流归约**（替换 `accumulate_stream` 调用）：闭合 transparent-retry 的 `accumulate_stream` bail-on-error 缺口
  + 接通 stream delta（`MessageDelta`/`ThinkingDelta`）直发 `tx_event` + early-tool-start + steer mid-stream buffer。
  是多个 guardrail / 次级排空点的共同前置。
- **cancel-token 注入**：transparent-retry 短路 + steer stale-drain + loop 顶取消检查，在 wire-in 步或单独小切片接入。
- 其余 guardrail（compaction/approval/subagent/cycle）+ `on_llm_*` 桥接 / wire tool-call id 透传 /
  `HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役——后续切片。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-10 §E approval 吸收落地，第五个 guardrail，seam-3 per-tool，`feat/pluggable-framework-core`）：**

§E 的第九个切片落地——把生产 `handle_deepseek_turn` 的 10 个 guardrail 中的第五个（工具调用用户审批）吸收进
`HostAgentExecutor`。审批把写文件 / 代码执行类工具挡在用户许可之后：运行此类工具前，执行器先发
`Event::ApprovalRequired`（携带两个指纹 key 供 host 做 approve-for-session / deny-exact 去重，以及写工具的模型
intent summary），再阻塞在审批决策 channel 上按 wire tool id 配对（stale id 丢弃）——镜像 `handle_deepseek_turn`
的 per-tool 审批流（`turn_loop.rs:2283-2371`）。拒绝时工具不运行，回灌 `permission_denied` 错误让模型反应
（turn 继续）。本轮纯增量（`host_executor.rs` 一个文件 + 文档），零既有调用点行为改动；生产路径
`handle_deepseek_turn` 不受影响。

- **`approval` 字段**：`HostAgentExecutor` 新增 `approval: Option<Arc<tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>>>`
  字段 + 构造器参数（非-审批 embed/测试传 `None`——21 个既有测试构造器改传第 8 个 `None`）。**首个用 `tokio::sync::Mutex`
  的 guardrail**（steer/LSP 用 `std::sync::Mutex`）：审批需**阻塞**在 `recv().await`，guard 必须跨 `await`——std mutex
  guard 非 `Send` 不行；`tokio::sync::Mutex` 的 guard 在 receiver `Send` 时亦 `Send`。锁仅单消费者（本执行器审批路径）
  持有，无竞争。receiver 跨 `run` 调用持久（匹配生产 `Engine.rx_approval`）。
- **`requires_approval(caps)`**（free fn）：`ToolCapability` 含 `RequiresApproval | ExecutesCode | WritesFiles` 即需审批——
  镜像 `ToolSpec::approval_requirement()` 默认推导（`ExecutesCode`→Required、`WritesFiles`→Suggest，皆 `!=Auto`）+
  显式 `RequiresApproval`。从框架 `Tool::capabilities()` 可达的最忠实静态近似。
- **`approval_intent_summary(content)`**（free fn）：合并本步 assistant `Text` 块，截断 2000 字符——镜像
  `turn_loop::approval_intent_summary`（本地轻微重复，后续 lift，同 `block_tool_result` 去重性质）。
- **`request_approval`（seam 3 per-tool）**：返回 `Ok(())`（放行：无需审批 / 无 channel / 已批准）或 `Err(denial_msg)`（拒绝）。
  逻辑：无 channel 或工具不需审批 → `Ok(())`；否则经 `event_tx` 发 `Event::ApprovalRequired { id, tool_name, description:
  tool.description(), input, approval_key, approval_grouping_key, intent_summary(只读工具 None) }`；再 `rx.lock().await` +
  `guard.recv().await` 按 id 配对（`Approved`→Ok、`Denied`→Err、`RetryWithPolicy`→Ok、`None`→Err("channel closed")）。
- **`run_inner` 接线**：在 `content.into_iter()` 前算 `intent_summary`；在 `AttemptDecision::Proceed` 臂内、`tool.run` 前包审批门：
  `Ok(()) → tool.run(input)`，`Err(denial) → Err(ToolError::permission_denied(denial))`。顺序保持 `on_tool_start` → loop-guard
  `record_attempt` →（若 Proceed）审批门 → `tool.run` → `on_tool_end`。拒绝调用仍发 `on_tool_end` + `record_outcome(success=false)`
  + push `is_error:true` ToolResult（与 `NotAvailable`/loop-guard-block 路径一致）。
- **刻意部分桥接（by design）**：
  - **无 cancel-token race**：生产 `await_tool_approval` select `cancel_token.cancelled()` 以在取消的 turn 上脱出审批等待；
    本执行器无 `CancellationToken`，阻塞到匹配决策到达或 channel 关闭。延后到 wire-in（同 transparent-retry/steer）。
  - **静态推导审批**：用 `Tool::capabilities()`，无 per-input 动态覆盖（`ToolSpec::approval_requirement_for_input`，如
    `exec_shell rm`→Required vs `ls`→Auto）；框架 `Tool` trait 刻意不带该面（§E 设计注记）。延后到 wire-in 接 `ToolDispatcher`。
  - **`RetryWithPolicy` 视为 `Approved`**：sandbox 提权需 `ToolDispatcher::execute` + `sandbox_override`（host 重建提权
    context），框架 `Tool::run` 路径不携带（`ToolSpecAdapter` 用固定 `ToolContext`）。执行器以未变 context 运行；提权延后。
- **test doubles**：`approval_channel()` helper（镜像 `steer_channel()`，capacity 64，`tokio::sync::Mutex`）；新增 `WriteSpec`
  （`impl ToolSpec`，声明 `WritesFiles`，回显 `path`，返成功）。6 个新测试：`approval_approved_runs_tool`（批准→运行、
  ToolResult `wrote:/tmp/x` 成功）、`approval_denied_skips_tool_with_permission_error`（拒绝→不运行、`is_error`、content 含
  "denied"）、`approval_none_skips_gating`（无 channel→不门控、工具直接运行）、`approval_readonly_tool_skips_gating`
  （EchoSpec(ReadOnly) + channel 有但不推决策；若门控误触发则 `recv()` 阻塞→2s 超时失败证之）、
  `approval_emits_approval_required_event`（event channel 收到 `ApprovalRequired`：id/tool_name/description/approval_key/
  approval_grouping_key/intent_summary 齐全）、`approval_retry_with_policy_treated_as_approved`（RetryWithPolicy→运行、
  content `wrote:/tmp/x`）。决策预先推入有界 channel（无需并发 task）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零新 warning；`cargo test -p codesmith-agent-runtime --lib host_executor`
27 通过（21 既有 + 6 新 approval）；`cargo test -p codesmith-agent-runtime --lib` 1031 通过（0 失败、2 ignored，原 1025 +6）；
`cargo test -p codesmith-agent --lib` 79 通过；`cargo build --workspace` 全绿（tui warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- **下一个 guardrail**：建议 **capacity**（seam 1 pre-request + seam 4 post-tool；硬 token-budget preflight 无条件运行——需
  `api_provider` + token 计数 + `recover_context_overflow` 级联，是迄今最重 guardrail，可能需拆多切片，建议先单独吸收硬
  preflight 而非 off-by-default 的软 `CapacityController`）或 **compaction**（seam 1；与 capacity 强耦合——capacity 的
  overflow 恢复即 compaction 级联，两者可能需一起规划）。approval 已证 `tokio::sync::Mutex` + 阻塞 `recv()` seam-3 可行。
- **inline 流归约**（替换 `accumulate_stream` 调用）：闭合 transparent-retry 的 `accumulate_stream` bail-on-error 缺口
  + 接通 stream delta（`MessageDelta`/`ThinkingDelta`）直发 `tx_event` + early-tool-start + steer mid-stream buffer。
  是多个 guardrail / 次级排空点的共同前置。
- **cancel-token 注入**：transparent-retry 短路 + steer stale-drain + approval 审批等待脱出 + loop 顶取消检查，在 wire-in
  步或单独小切片接入。
- 其余 guardrail（compaction/capacity/subagent/cycle）+ `on_llm_*` 桥接 / wire tool-call id 透传 /
  `HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役——后续切片。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-10 §E compaction 吸收落地，第六个 guardrail，seam-1 pre-request，`feat/pluggable-framework-core`）：**

§E 的第十个切片落地——把生产 `handle_deepseek_turn` 的 10 个 guardrail 中的第六个（上下文 compaction）吸收进
`HostAgentExecutor`。compaction 把 transcript 压在模型上下文窗内：每步顶部（steer 排空之后、LSP flush 之前）跑两段收缩，
镜像 `handle_deepseek_turn` 的 pre-request compaction（`turn_loop.rs:378-440`）——(a) **micro-compaction**：累计 tool-result
字节触 32KB cache trigger 即 `micro_compact_messages` 把旧 tool result 改写为 cleared placeholder（无 LLM 调用）；
(b) **auto-compaction**：`should_compact` 过阈（keep-recent 窗外的可摘要消息够多）即 `compact_messages_safe` 调 LLM 出摘要、
替换 transcript。两段皆经 `ChatHistory::clear()` + `push()` loop 整体替换（trait 无 bulk replace——复用其原语，同 "不动核心
trait" 先例）。本轮纯增量（`host_executor.rs` 一个文件 + 文档），零既有调用点行为改动；生产路径 `handle_deepseek_turn` 不受影响。
选 compaction 先于 capacity：capacity 的 `recover_context_overflow` 恢复级联复用 `compact_messages_safe`/`micro_compact`，
compaction 机制是 capacity 的前置；且 compaction 只 seam-1（capacity 需 seam 1+4+reactive seam 2，更重）。

- **`CompactionProbe` collaborator**（新 `pub struct`，镜像 `LspProbe` 的 interior-mutability 模式）：`config: CompactionConfig` +
  `workspace: PathBuf` + `micro_state: Arc<std::sync::Mutex<MicroCompactState>>` + `circuit_breaker: Arc<std::sync::Mutex<CompactionCircuitBreaker>>`。
  `std::sync::Mutex`（同 steer/LSP——锁不跨 `await`：调用 async `compact_messages_safe` 前先 `history.messages().to_vec()` 克隆出
  消息释放借用）。因 `Mutex` 在执行器结构体上，breaker / micro_state 跨 `run` 调用持久（匹配生产 `Engine.micro_compact_state` /
  `.compaction_circuit_breaker` 字段——turn N 失败的 compaction 仍使 turn N+1 的 breaker 被跳闸）。breaker 跳闸（连续 3 次失败）
  抑制后续 compaction 尝试。`CompactionProbe::new(config, workspace)` 构造；`#[cfg(test)]` 暴露 `breaker()` 供持久性测试断言。
- **`compaction` 字段**：`HostAgentExecutor` 新增 `compaction: Option<CompactionProbe>` 字段 + 第 9 个构造器参数（非-compaction
  embed/测试传 `None`——27 个既有测试构造器改传第 9 个 `None`）。
- **`run_compaction`（seam 1 pre-request）**（`async fn run_compaction(&self, client, history)`）：`None` / `!config.enabled` → return；
  breaker `should_attempt()` 不过 → return（被节流）；micro 阶段：`should_trigger_micro_compact(messages, &state, false)` → 克隆消息、
  `micro_compact_messages(&mut, &mut state)`、清出字节 >0 则 `clear()`+push loop 应用；auto 阶段：`should_compact(history.messages(),
  &config, Some(&workspace), None, None)` 不过 → return（镜像生产门控——`compact_messages_safe` 内部**不**在阈下早返，无此门会
  对 in-budget transcript 白调一次 LLM）；过则克隆消息（释放借用后再 await）、`compact_messages_safe(client.as_ref(), &msgs, &config,
  Some(&workspace), None, None, None)` → Ok(result)：`clear()`+push `result.messages`、`record_success()`、发 status；
  Err(e)：`record_failure()`、发 status（非 transient 错误即时返回自 `compact_messages_safe`——测试无 backoff）。
- **`run_inner` 接线**：在 `drain_steers` 之后、`flush_pending_lsp_diagnostics` 之前插 `self.run_compaction(&client, history).await;`
  （生产顺序：steer→compaction→…→LSP flush→request）。更新 seam-1 注释（compaction 已吸收；capacity/cycle 仍 "to come"）。
- **刻意部分桥接（by design）**：
  - **summary-prompt merge 丢弃**：`compact_messages_safe` 算出 `CompactionResult.summary_prompt`（卷起的摘要，本应 `merge_compaction_summary`
    折入 system prompt），但框架 `ChatHistory` 无 system-prompt setter（执行器 system prompt 是静态 `config.system`）——算出即丢。
    LLM 仍能在卷起的 transcript 体里看到摘要，缺的只是 system-prompt 重注入。延后到 wire-in 接 `Session`（其 system prompt 可变）。
  - **attachment reinject 延后**：生产 `reinject_compaction_attachments` 重插被压掉的 plan/todos/subagents/read-file 快照（host 耦合，
    `session.plans`/`.todos`/sub-agent state）；框架 `ChatHistory` 不携带。延后到 wire-in（同 LSP 的 `apply_patch` 路径延迟性质）。
  - **post-compact cleanup 延后**：生产 `post_compact_cleanup` 在 compaction 后强重建 working set + 重置 per-file cycle state
    （transcript 已变，working set 源已 stale）；working set/cycle state host 耦合，不经 `ChatHistory`——延后 wire-in。
  - **enhancements 传 `None`**：生产 `build_compaction_enhancements` 供 PreCompact hooks + session-memory-first 摘要 seed；框架
    `Callback`/`ChatHistory` 暂不带其面，`compact_messages_safe` 以 `enhancements=None` 调。随 PreCompact hook 切片接入。
  - **working-set pins/paths 传 `None`**：生产把 `external_pins`/`external_working_set_paths`（host 派生 working set）喂给
    `should_compact`/`compact_messages_safe` 以保 pinned 文件；执行器无 working set 派生，两者皆 `None`（用内部派生路径，同
    `recover_context_overflow` 的 forced path）。随 working-set 切片接入。
  - **无 `emit_session_updated`**：同 LSP flush 的合成 push，`clear()`+`push()` 替换不经 `ChatHistory` 路径发 session-updated UI 事件。
  - **测试无 backoff**：失败只记 breaker + 发 status（无 sleep）；生产在重试前加指数 backoff。非 transient 错误即时返回自
    `compact_messages_safe`，故 breaker 连续失败跳闸（3）即节流。backoff 延后 wire-in。
- **test doubles**：`MockLlm` 扩展 3 字段（`compaction_reply: Mutex<Option<MessageResponse>>` + `compaction_error: Mutex<Option<String>>` +
  `compaction_calls: Mutex<u32>`）+ 3 方法（`with_compaction_summary(self, &str)`、`with_compaction_error(self, &str)`、`compaction_calls() -> u32`）；
  `create_message` 增计数 + 返 canned reply/error（原 `bail!` 行为保留为 reply=None 时）。测试 helper：`compaction_config_low_threshold()`
  （阈 100，触 auto-compact）、`compaction_config_high_threshold()`（967000，不触）、`compaction_config_disabled()`（`enabled:false`）、
  `seed_text_messages(sess, n)`、`seed_large_file_read(sess)`（>32KB 触 micro trigger）。6 个新测试：`compaction_none_is_noop`（`None`→无变、
  NoToolCalls）、`compaction_disabled_skips_even_when_over_threshold`（`enabled:false`→无变）、`micro_compact_clears_old_tool_results`
  （大 tool result 触 32KB→清为 placeholder、`compaction_calls()==0` 无 LLM）、`auto_compact_summarizes_when_over_threshold`（12 消息+阈 100→
  `should_compact` 过→mock "SUMMARY"→transcript 收缩、`compaction_calls()==1`）、`compaction_circuit_breaker_records_failure`（mock Err→
  `consecutive_failures()==1`、status 发）、`compaction_cross_turn_circuit_breaker_persistence`（interior-mutability 证：run1 breaker=1、
  run2 同执行器仍=1，跨 `run` 持久）。共 33 个 host_executor 测试通过（27 既有 + 6 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零新 warning；`cargo test -p codesmith-agent-runtime --lib host_executor`
33 通过（27 既有 + 6 新 compaction）；`cargo test -p codesmith-agent-runtime --lib` 1037 通过（0 失败、2 ignored，原 1031 +6）；
`cargo test -p codesmith-agent --lib` 79 通过；`cargo build --workspace` 全绿（tui warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- **下一个 guardrail**：建议 **capacity**（seam 1 pre-request + seam 4 post-tool；硬 token-budget preflight 无条件运行——需
  `api_provider` + token 计数 + `recover_context_overflow` 级联，是迄今最重 guardrail，可能需拆多切片，建议先单独吸收硬
  preflight 而非 off-by-default 的软 `CapacityController`）。compaction 已证 `std::sync::Mutex` + 克隆-再-await seam-1 可行，
  且 capacity 的 overflow 恢复级联可直接复用已吸收的 `compact_messages_safe`/`micro_compact`/breaker 机制（本切片的前置价值兑现）。
- **inline 流归约**（替换 `accumulate_stream` 调用）：闭合 transparent-retry 的 `accumulate_stream` bail-on-error 缺口
  + 接通 stream delta（`MessageDelta`/`ThinkingDelta`）直发 `tx_event` + early-tool-start + steer mid-stream buffer。
  是多个 guardrail / 次级排空点的共同前置。
- **cancel-token 注入**：transparent-retry 短路 + steer stale-drain + approval 审批等待脱出 + loop 顶取消检查，在 wire-in
  步或单独小切片接入。
- **compaction 闭合项**：summary-prompt merge / attachment reinject / post-compact cleanup / enhancements / working-set pins /
  `emit_session_updated` 随 wire-in 切片接入（`Session` 接通后 system prompt 可变 + working set 派生可达）。
- 其余 guardrail（capacity/subagent/cycle）+ `on_llm_*` 桥接 / wire tool-call id 透传 /
  `HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役——后续切片。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-11 §E capacity 吸收落地，第七个 guardrail，seam-1 pre-request，`feat/pluggable-framework-core`）：**

§E 的第十一个切片落地——把生产 `handle_deepseek_turn` 的 10 个 guardrail 中的第七个（硬 token-budget preflight + 紧急恢复）吸收进
`HostAgentExecutor`。capacity 是迄今最重的 guardrail：生产 `recover_context_overflow`（`engine/mod.rs:1670-1893`）是三阶段级联
（responsive compact → forced full compaction → hard trim），且有 preflight（seam 1）+ reactive（seam 2）+ post-tool checkpoint
（seam 4）三个接入点。本切片吸收的是 **seam 1 preflight（Gate B，always-on 硬预算检查）+ 简化的恢复级联**——reactive seam-2 路径
（provider context-length rejection → recovery）和 opt-in `CapacityController`（Gate A，off by default since v0.8.11）延后。
本轮纯增量（`host_executor.rs` 一个文件 + 文档），零既有调用点行为改动；生产路径 `handle_deepseek_turn` 不受影响。

- **`CapacityProbe` collaborator**（新 `pub struct`，镜像 `CompactionProbe` 但**无状态**）：`api_provider: ApiProvider` +
  `model: String`（budget 计算用，`context_input_budget_for_provider`）+ `compaction_config: CompactionConfig`（forced compaction 路径
  clone + force 后用）+ `workspace: PathBuf`（`compact_messages_safe` 路径规范化用）。**无 interior-mutability**——per-run 恢复计数器
  （`context_recovery_attempts: u8`）是 `run_inner` 局部变量（匹配 transparent-retry 的 `stream_retry_attempts` 模式），probe 本身不持状态。
  `CompactionProbe` 的 `micro_state` / `circuit_breaker` 在 capacity 路径不复用——recovery 的 micro-compact 用 local
  `MicroCompactState::default()`（best-effort），因 preflight 跑在 `run_compaction` 之后（同一步），persistent-state micro-compact 已跑过。
- **`capacity` 字段**：`HostAgentExecutor` 新增 `capacity: Option<CapacityProbe>` 字段 + 第 10 个构造器参数（非-capacity embed/测试传
  `None`——33 个既有测试构造器改传第 10 个 `None`）。
- **`run_capacity_preflight`（seam 1 pre-request）**（`async fn`）：`run_compaction` 之后、`flush_pending_lsp_diagnostics` 之前插
  （匹配生产顺序：compaction → capacity → LSP flush → request）。`None` probe / `None` budget（unknown model）→ `Proceed`；
  `estimate_input_tokens_conservative(history.messages(), system)` 超 `context_input_budget_for_provider(probe.api_provider, &probe.model)`
  → 检查 `context_recovery_attempts >= MAX_CONTEXT_RECOVERY_ATTEMPTS`（2）→ `Fail`（hard fail turn）；否则调 `recover_context_overflow` →
  success: `RetryStep`（`continue`，重启 step 使 request snapshot 拾起 compacted transcript）；fail: `Proceed`（fall through，请求照发——
  镜像生产 `recover_context_overflow` 返 false 时不 `continue`）。
- **`recover_context_overflow`**（`async fn`，简化三阶段级联）：
  - **Phase 1 — best-effort micro-compact**（无 API call）：local `MicroCompactState::default()` + `micro_compact_messages`。cleared > 0 且
    under budget → return true（复用既有 `micro_compact_messages`，零新依赖）。
  - **Phase 2 — forced full LLM compaction**：clone `probe.compaction_config`，`enabled = true`、
    `token_threshold = min(existing, target_budget - 1).max(1)`、`auto_floor_tokens = 0`（bypass cache-preservation floor——在硬上限处必须释放预算，
    镜像 `mod.rs:1813-1816`）。`compact_messages_safe` 调用；`Ok` → `clear()` + `push()` loop 替换 transcript（`summary_prompt` 丢弃——同
    compaction slice 的 gap）；`Err` → emit status，fall through to hard trim。
  - **Phase 3 — hard trim**：clone messages → while `len > MIN_RECENT_MESSAGES_TO_KEEP && estimate > budget`: `remove(0)` → `clear()` + `push()` loop。
  - Return `true` only if `after_tokens <= target_budget && (after_tokens < before_tokens || after_count < before_count)`。
- **`run_inner` 接线**：新增 `let mut context_recovery_attempts: u8 = 0;`（near `stream_retry_attempts`）；seam 1 preflight match（Proceed/RetryStep/Fail）；
  healthy stream 后 `context_recovery_attempts = 0`（镜像 `turn_loop.rs:617` reset on successful stream start）。
- **刻意部分桥接（by design）**：
  - **responsive compact cascade（Phase 1）延后**：生产的 `recover_context_overflow` 跑四步 responsive cascade（micro → partial-from →
    partial-up-to → full）；partial compaction 只摘要 transcript 切片（保 prefix cache）——是优化非正确性路径。本切片跳到 forced full
    compaction + hard trim（更激进但总是恢复——hard trim 是终极兜底）。responsive cascade 随 inline 流归约切片接入（partial compaction 需
    responsive state machine，`Session`-internal）。
  - **reactive seam-2 路径延后**：生产也在 provider 拒绝 context-length error 时触发 recovery（`turn_loop.rs:620-633`）。本执行器的
    `stream_with_transparent_retry` 经 `?` 传播流错误，不检查 `is_context_length_error_message`；reactive recovery 随 inline 流归约切片接入
    （需 error message 供分类）。
  - **opt-in `CapacityController`（Gate A）延后**：off-by-default 软控制器（`run_capacity_pre_request_checkpoint` /
    `run_capacity_post_tool_checkpoint` / `run_capacity_error_escalation_checkpoint`）未吸收；只吸收 always-on 硬 preflight（Gate B）。
    Gate A 需完整 `CapacityController` 状态机（slack window / recent tool+ref counts / model priors）——独立 opt-in 切片。
  - **cancel-token 短路延后**：生产在 `!cancelled` 时中止 recovery；本执行器无 `CancellationToken`。有界 `MAX_CONTEXT_RECOVERY_ATTEMPTS`（2）防死循环；
    短路在 wire-in 步接入。
  - **同 compaction 的 recovery gaps**：`recover_context_overflow` 调 `compact_messages_safe` 时同样缺 `merge_compaction_summary` /
    `reinject_compaction_attachments` / `post_compact_cleanup` / `enhancements` / working-set pins/paths（见 "Known gaps in compaction"）。
- **test doubles 扩展**：新增 `capacity_probe(api_provider, model)` helper（构造 `CapacityProbe` 用 `CompactionConfig::default()` + test workspace）。
  6 个新测试：`capacity_none_is_noop`（`None`→NoToolCalls、`compaction_calls()==0`）、`capacity_within_budget_proceeds`（小 transcript→Proceed、
  `compaction_calls()==0`）、`capacity_over_budget_recovers_via_compaction`（40 条 ~200 chars 消息超 Ollama/llama2 budget 3072→forced compaction→
  `compaction_calls()==1`、history 收缩）、`capacity_over_budget_recovers_via_hard_trim`（compaction 失败→hard trim→recovery succeeds、
  `compaction_calls()==1`、`history.len()<42`）、`capacity_micro_compact_clears_tool_results_in_recovery`（>32KB file_read tool result→recovery 的
  micro-compact 清为 placeholder→`compaction_calls()==0` 无 LLM）、`capacity_recovery_fails_proceeds_with_request`（10 条 10K-char 消息→
  compaction 失败、hard trim 受 `MIN_RECENT_MESSAGES_TO_KEEP` 限制无法降至 budget 下→recovery fails→Proceed→mock 返回→NoToolCalls、
  `compaction_calls()==1`）。共 39 个 host_executor 测试通过（33 既有 + 6 新 capacity）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零新 warning；`cargo test -p codesmith-agent-runtime --lib host_executor` 39 通过
（33 既有 + 6 新 capacity）；`cargo test -p codesmith-agent-runtime --lib` 1043 通过（0 失败、2 ignored，原 1037 +6）；
`cargo test -p codesmith-agent --lib` 79 通过；`cargo build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- **下一个 guardrail**：剩余 guardrail 是 **early-tool-start**（seam 2，在流归约中检测 tool_use 起始并提前 dispatch——需 inline 流归约）、
  **subagent**（seam 3，sub-agent handoff/hold）、**cycle**（seam 1/4，per-file cycle state）。建议优先 **inline 流归约**（替换 `accumulate_stream`
  调用）——它是多个 guardrail / 次级排空点（steer mid-stream buffer / early-tool-start / transparent-retry 的 `accumulate_stream` bail-on-error
  缺口 / reactive capacity recovery）的共同前置。capacity 已证 `CapacityProbe` 无状态 seam-1 可行，且 recovery 级联直接复用已吸收的
  `compact_messages_safe` / `micro_compact_messages` / `estimate_input_tokens_conservative` 机制（compaction slice 的前置价值继续兑现）。
- **reactive capacity recovery**（seam 2）：provider context-length rejection → `recover_context_overflow`——随 inline 流归约接入
  （需 error message 供 `is_context_length_error_message` 分类）。
- **opt-in `CapacityController`**（Gate A + seam 4 post-tool checkpoint + error-escalation）：独立 opt-in 切片，需完整 `CapacityController` 状态机。
- **cancel-token 注入**：transparent-retry 短路 + steer stale-drain + approval 审批等待脱出 + capacity recovery 短路 + loop 顶取消检查，
  在 wire-in 步或单独小切片接入。
- **compaction 闭合项**：summary-prompt merge / attachment reinject / post-compact cleanup / enhancements / working-set pins /
  `emit_session_updated` 随 wire-in 切片接入（`Session` 接通后 system prompt 可变 + working set 派生可达）。同样适用于 capacity recovery。
- **`HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役**：所有 guardrail 吸收后，`handle_send_message` 改用 `HostAgentExecutor`，删
  `handle_deepseek_turn`。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-11 §E inline 流归约落地，闭合 bail-on-error 缺口，`feat/pluggable-framework-core`）：**

§E 的第十二个切片落地——把 `HostAgentExecutor` 的 `accumulate_stream` 调用替换为内联流归约器 `reduce_stream`，使流式 delta（text/thinking）实时发到 `Callback::on_stream_delta`（新 CORE trait 方法），并跟踪 `any_content_received`
闭合 transparent-retry 的 bail-on-error 缺口（流死后部分内容不再丢弃——partial surfaced 不 retry，empty 才 retry）。这是多个 guardrail / 次级排空点的共同前置：early-tool-start（在流中检测 tool_use 起始）、steer mid-stream buffer、
reactive capacity recovery（需 error message 供 `is_context_length_error_message` 分类）的前置。本轮跨 3 文件 2 crate（CORE `Callback` trait + `CallbackBridge` + `HostAgentExecutor`），纯增量（CORE trait 新方法有默认 no-op——
所有既有 `Callback` impl 不受影响）；生产路径 `handle_deepseek_turn` 不受影响。

- **CORE `StreamDelta` + `on_stream_delta`**（`codesmith-agent/src/callback/mod.rs`）：新增 `pub enum StreamDelta { Text { index, content }, Thinking { index, content } }`——UI-relevant 流 delta（tool-input JSON delta 不在此——它 assemble 进
  `ContentBlock::ToolUse`，非用户可见直到 `on_llm_end`）。`Callback` trait 新增 `on_stream_delta(&self, delta: &StreamDelta)` 方法（默认 no-op，匹配既有 6 方法的 `noop()` 模式）。`CallbackSet` 扇出已补上。向后兼容：所有默认 no-op，
  既有 `NoopCallback` / `CallbackBridge` / `RecordingCallback` 不受影响（不 override 即 no-op）。
- **`CallbackBridge::on_stream_delta`**（`codesmith-agent-runtime/src/callback_bridge.rs`）：map `StreamDelta::Text` → `Event::MessageDelta { index, content }`、`StreamDelta::Thinking` → `Event::ThinkingDelta { index, content }`，发到 `tx` channel。
  模块文档更新：bridged-vs-gap 表新增 `on_stream_delta` 行，"streaming deltas have no Callback method" 段改为"now flow through `on_stream_delta`"。block-lifecycle 事件（`MessageStarted`/`ThinkingStarted`/`ThinkingComplete`/`MessageComplete`）
  尚未桥接——`reduce_stream` 不在 `ContentBlockStart`/`ContentBlockStop` 时合成它们（延后到 early-tool-start 切片，需 tool catalog 校验 input 后才 announce）。
- **`reduce_stream` 内联归约器**（`host_executor.rs`）：替换 `accumulate_stream(stream).await` 调用。accumulation 逻辑镜像 CORE `accumulate_stream`（`BTreeMap<u32, BlockBuild>` keyed by wire block index；text/thinking delta append 到
  block buffer；tool-input JSON delta buffer 后 final `serde_json::from_str`）。关键差异：每个 text/thinking delta 在 buffer 前**也**发到 `self.callback.on_stream_delta()`——host UI 实时流式（不再等整个 stream buffer 完才显示）。
  `any_content_received` 在首个非-`MessageStart` event 翻转——cross from "stream not yet productive"（eligible for transparent retry）into "model has billed for output"（must surface），镜像 `turn_loop.rs:770-772`。
- **`StreamReduceOutcome` 三态枚举**（替换 CORE `accumulate_stream` 的 binary `Result<(Vec<ContentBlock>, Option<String>)>`）：
  - `Complete` — stream clean 完成（`MessageStop` 或 stream 结束无 error）。
  - `Partial` — stream 产了 content（text/thinking/tool delta 到达）后 mid-flight 死。partial content surfaced（不 retry），镜像生产 `any_content_received` guard（`streaming.rs:81-87` 的 `should_transparently_retry_stream`）。
  - `Empty` — stream 死前无任何 content（只有 `MessageStart` 或什么都没有）。safe to retry transparently。
- **`stream_with_transparent_retry` 更新**：`reduce_stream` 返回三态 → `Complete`/`Partial` 均 `return Ok(...)`（surface content，reset budget），仅 `Empty` retry（budget ≤ `MAX_STREAM_RETRIES` = 3）。这闭合了 bail-on-error 缺口：
  旧 `accumulate_stream` 在首个 erroring item bail 并丢弃 partial blocks → executor retry 即使生产会 ship partial content（它 track `any_content_received` inline 跳过 retry）。内联归约器现在做同样的区分。`stream_with_transparent_retry`
  签名不变（`&self` 方法可直接用 `self.callback`）——`run_inner` 调用点零改动。
- **`BlockBuild` + `finalize_blocks`**（private）：`BlockBuild` enum 镜像 CORE `accumulate_stream` 的 local enum（`Text(String)` / `Thinking(String)` / `ToolUse { id, name, input_buf, start_input, caller: Option<ToolCaller> }`）。
  `finalize_blocks(BTreeMap) → Vec<ContentBlock>` extracted 为独立 fn，供 clean completion 和 mid-flight error 两路径复用。
- **刻意部分桥接（by design）**：
  - **block-lifecycle 事件延后**：`MessageStarted`/`ThinkingStarted`/`ThinkingComplete`/`MessageComplete` 不在 `reduce_stream` 合成——生产在 `ContentBlockStart`/`ContentBlockStop` 时发它们。延后到 early-tool-start 切片
    （`ToolCallStarted` 需 tool catalog 校验 input 后才 announce；block-lifecycle 可同期补上）。
  - **inner mid-flight retry 不复制**：生产在 event loop 内部（no content received 时）reset stream（`turn_loop.rs:775-834`）；本执行器用更简单的外层 retry（re-call `create_message_stream`）。两者对 retry 决策功能等价；
    inner retry 的优势是避免多余 `MessageStart` round-trip（仅 latency-sensitive 生产路径 relevant）。
  - **reactive capacity recovery 延后**：`stream_with_transparent_retry` 仍用 `?` 传播 pre-stream error（不检查 `is_context_length_error_message`）。内联归约器已就位（前置价值兑现——它 surfaces error message 供分类），
    但 error-classification + recovery plumbing（route context-length errors to `recover_context_overflow`）是 immediate next sub-slice。
  - **cancel-token 短路延后**：同 transparent-retry 的既有 gap。
- **test doubles 扩展**：`MockRound` 新增 `EventsThenErr(Vec<StreamEvent>, String)` variant（stream 产 events 后 trailing `Err`——simulates mid-flight death *after* content）。`DeltaRecorder` callback（records `on_stream_delta` calls）。
  `thinking_block(idx, body)` helper（mirrors `text_block`，builds `ContentBlockStart::Thinking` + `ThinkingDelta` + `ContentBlockStop`）。4 个新测试：
  `stream_emits_text_deltas_to_callback`（两 text block → 2 `StreamDelta::Text` delta，index/content 匹配）、`stream_emits_thinking_deltas_to_callback`（thinking + text block → `StreamDelta::Thinking` + `StreamDelta::Text`）、
  `stream_partial_content_surfaces_without_retry`（`EventsThenErr` with text_block("partial answer") + Err → 1 request only（no retry）、partial text in history、partial-content status）、`stream_deltas_flow_through_callback_bridge`
  （end-to-end：executor → `CallbackBridge` → `Event::ThinkingDelta` + `Event::MessageDelta` on Event channel）。

**验证：** `cargo +1.90.0 build -p codesmith-agent` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；
`cargo test -p codesmith-agent --lib` 79 通过；`cargo test -p codesmith-agent-runtime --lib` 1049 通过（原 1043 +6：4 host_executor + 2 callback_bridge，0 失败、2 ignored）；
`cargo test -p codesmith-agent-runtime --lib host_executor` 43 通过（39 既有 + 4 新）；`cargo test -p codesmith-agent-runtime --lib callback_bridge` 5 通过（3 既有 + 2 新）；
`cargo build --workspace` 全绿（tui 143 warning 均既有死代码）。

**下一聚焦工作：**
- **reactive capacity recovery**（seam 2）：provider context-length rejection → `recover_context_overflow`。内联归约器已就位（surfaces error message 供 `is_context_length_error_message` 分类），但 `stream_with_transparent_retry` 仍用 `?`
  传播 pre-stream error。下一 sub-slice：在 `stream_with_transparent_retry` 的 `create_message_stream` error 路径检查 `is_context_length_error_message` + 调 `recover_context_overflow`（需 history/system/context_recovery_attempts——
  可能需拆 `stream_with_transparent_retry` 使 pre-stream error 可分类传播回 `run_inner` 处理，或把必要参数传入）。
- **early-tool-start**（seam 2）：在 `reduce_stream` 中检测 `ContentBlockStart::ToolUse` → `ContentBlockStop` 后，preflight 校验 + `tokio::spawn` 提前 dispatch read-only tool（需 tool catalog + approval + loop-guard——需 `ToolDispatcher` 接通，
  可能随 wire-in 切片）。block-lifecycle 事件（`MessageStarted`/`ThinkingStarted` 等）可同期补上（在 `reduce_stream` 的 `ContentBlockStart`/`ContentBlockStop` 分支发 `on_stream_delta` 之外的 lifecycle 事件——需 CORE `Callback` 新方法
  或扩展 `StreamDelta`）。
- **subagent**（seam 3，sub-agent handoff/hold）、**cycle**（seam 1/4，per-file cycle state）：后续 guardrail。
- **opt-in `CapacityController`**（Gate A + seam 4 post-tool checkpoint + error-escalation）：独立 opt-in 切片。
- **cancel-token 注入**：transparent-retry 短路 + steer stale-drain + approval 审批等待脱出 + capacity recovery 短路 + loop 顶取消检查，在 wire-in 步或单独小切片接入。
- **compaction 闭合项**：summary-prompt merge / attachment reinject / post-compact cleanup / enhancements / working-set pins / `emit_session_updated` 随 wire-in 切片接入。同样适用于 capacity recovery。
- **`HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役**：所有 guardrail 吸收后，`handle_send_message` 改用 `HostAgentExecutor`，删 `handle_deepseek_turn`。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-11 §E reactive capacity recovery 落地，seam-2 前置闭合，`feat/pluggable-framework-core`）：**

§E 的第十三个切片落地——把生产 `handle_deepseek_turn` 的 reactive context-length recovery（seam 2）吸收进 `HostAgentExecutor`。当 LLM provider 在流开起前以 context-length 错误拒绝请求时，执行器先用 `is_context_length_error_message` 分类该错误，命中即跑
`recover_context_overflow`（已吸收的 capacity 恢复级联），成功则重启 step 使请求快照拾起压缩后的 transcript（镜像 `turn_loop.rs:620-633`）；非 context-length 错误或恢复失败则硬失败。这是 §E inline 流归约切片（第十二切片）落地的"前置价值兑现"——
内联归约器已 surface 错误消息供分类，本切片接通 error-classification + recovery plumbing。本轮纯增量（`host_executor.rs` 一个文件 + 文档），零既有调用点行为改动；
生产路径 `handle_deepseek_turn` 不受影响。

- **`StreamRoundOutcome` 三态**（新 enum）：替换 `stream_with_transparent_retry` 的 binary `Result<(Vec<ContentBlock>, Option<String>)>`。`Content { content, stop_reason }`——流产出内容（clean completion 或 partial surfacing）；
`RecoveredContextOverflow`——pre-stream context-length 拒绝经 emergency compaction 恢复成功，signal 调用者 `continue` 重启 step（请求快照拾起压缩后的 transcript，镜像 `turn_loop.rs:631-632`）。
- **`try_recover_context_overflow` helper**（新 `async fn`，`&self` 方法）：分类 + 预算 + 恢复。门控序列（全 `&&`，任一 miss 返 `false` 使 caller 硬失败）：
  `self.capacity` 为 `Some`（probe 存在）→ `is_context_length_error_message(error_message)`（非 context-length 错误不恢复）→
  `*context_recovery_attempts < MAX_CONTEXT_RECOVERY_ATTEMPTS`（2，预算耗尽则发 status 并硬失败）→ `context_input_budget_for_provider(probe.api_provider, &probe.model)` 为 `Some`（未知模型无 budget 则硬失败）→
  `recover_context_overflow(client, history, system, target_budget, "provider context-length rejection").await`（复用已吸收的三阶段级联：micro-compact → forced full LLM compaction → hard trim）。
  成功则 `*context_recovery_attempts = saturating_add(1)`（镜像 `turn_loop.rs:631`）。budget reset 不在此处——在 `stream_with_transparent_retry` 的 stream-open Ok 臂（镜像 `turn_loop.rs:617`）。
- **`stream_with_transparent_retry` 重构**：签名 +3 参数（`context_recovery_attempts: &mut u8` / `history: &mut dyn ChatHistory` / `system: Option<&SystemPrompt>`）+ 返回 `Result<StreamRoundOutcome>`。
  把 `client.create_message_stream(request.clone()).await?` 改为 `match`：`Ok(s)` → `*context_recovery_attempts = 0`（stream 开起即 provider 接受请求，context 无恙，镜像 `turn_loop.rs:617`）+ 取 stream；
  `Err(e)` → `try_recover_context_overflow(...)` 成功则 `*stream_retry_attempts = 0`（fresh step）+ 返 `RecoveredContextOverflow`，否则 `Err`。三个 `reduce_stream` 臂的 `return Ok((content, stop_reason))` 改为 `return Ok(StreamRoundOutcome::Content { content, stop_reason })`。
- **`run_inner` 调用点**：传 +3 参数（`history` / `system.as_ref()` 借用，与 `run_capacity_preflight` 同 pattern；reborrow 在调用返回后释放，不与后续 `history.push` 冲突）。
  `Ok(StreamRoundOutcome::Content { content, stop_reason })` → `(content, stop_reason)`；`Ok(RecoveredContextOverflow)` → `continue`（重启 step）；`Err(e)` → `return Err(e)`。
  移除旧 `Ok(result) => context_recovery_attempts = 0`（现由 stream-open Ok 臂负责，更贴 production 的 `turn_loop.rs:617` 重置时机——stream 开起即重置，而非整轮 Ok 后）。
- **reset 语义变更（行为对齐，非 break）**：旧代码在整轮 Ok（Complete/Partial）后重置 `context_recovery_attempts`；新代码在 stream 开起（`create_message_stream` Ok）即重置。两者对 Complete/Partial 等价；对 Empty→retry 路径，新代码在首次 stream 开起即重置（匹配 production 617），旧代码不重置——这是对 production 行为的更忠实现，既有透明重试测试不受影响（它们不触发 capacity recovery，`context_recovery_attempts` 恒 0）。
- **刻意部分桥接（by design）**：
  - **reset-on-stream-open vs reset-on-round-Ok**：见上"reset 语义变更"——选择贴 production 的 stream-open 重置。
  - **第二个 reactive recovery 几乎必失败**：首次 compaction 后 transcript 收缩为 summary + recent tail（~5 消息），再压缩唯一的较旧 summary 消息是 no-op（无 shrinkage ⇒ `recover_context_overflow` 返 `false`）。故 `MAX_CONTEXT_RECOVERY_ATTEMPTS`（2）cap 是安全网，preflight 路径比 reactive 路径更可能触达——匹配 production 的防御性 `MAX = 2`。
  - **非 context-length pre-stream 错误硬失败**：connection / auth / timeout 错误径直 `Err`（production 把这类当 hard-fail / context-recovery 的独立 guardrail；本执行器不重试、不恢复，直接硬失败）。reactive 路径只覆盖 context-length 拒绝。
  - **cancel-token 短路延后**：production 在 `!cancelled` 时中止 recovery；本执行器无 `CancellationToken`。有界 `MAX_CONTEXT_RECOVERY_ATTEMPTS`（2）防死循环；短路在 wire-in 步接入（同 transparent-retry/steer/approval/capacity preflight 的既有 gap）。
  - **同 compaction/capacity 的 recovery gaps**：`recover_context_overflow` 调 `compact_messages_safe` 时同样缺 `merge_compaction_summary` / `reinject_compaction_attachments` / `post_compact_cleanup` / `enhancements` / working-set pins/paths（见 "Known gaps in compaction"）。
- **test doubles 扩展**：`MockRound` 新增 `StreamOpenErr(String)` variant——`create_message_stream` 本身返 `Err`（stream 不开起，模拟 pre-stream provider 拒绝），与 `StreamErr`（stream 开起后产出 mid-flight `Err` item，驱动透明重试）区分；request 仍在 match 前 push 进 `requests()`（故 `requests().len()` 仍记录该失败调用）。
  5 个新测试：`reactive_recovery_recovers_on_context_length_error`（10 条 seed 消息 ≪ 3072 budget → preflight 不触 → `StreamOpenErr(ctx_msg)` → 恢复（compaction_calls==1）→ `RecoveredContextOverflow` → continue → `end_call` → NoToolCalls；requests().len()==2；history 收缩），
  `reactive_recovery_non_context_length_error_hard_fails`（`StreamOpenErr("Connection timed out")` → Timeout 分类非 context-length → `Err`；compaction_calls==0），
  `reactive_recovery_failed_hard_fails`（`StreamOpenErr(ctx_msg)` + `with_compaction_error` → 恢复尝试失败（compaction error + transcript 已在本地 estimate 之下无法 trim）→ `Err`；compaction_calls==1），
  `reactive_recovery_without_capacity_probe_hard_fails`（无 capacity probe → `try_recover_context_overflow` 返 false → `Err`；compaction_calls==0），
  `reactive_recovery_surfaces_status_events`（event channel 收到含 "compaction" 的 `Event::Status`，证明恢复 surfacing 走 host Event 通道）。共 48 个 host_executor 测试通过（43 既有 + 5 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；`cargo test -p codesmith-agent-runtime --lib host_executor` 48 通过（43 既有 + 5 新 reactive-recovery）；
`cargo test -p codesmith-agent-runtime --lib` 1054 通过（0 失败、2 ignored，原 1049 +5）；`cargo test -p codesmith-agent --lib` 79 通过；
`cargo build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关；agent-runtime test build 10 warning 均既有——9 pre-existing + 1 `mut partial` 属前一 inline-stream-reduction 切片的测试，本轮未触碰——零新 warning）。

**下一聚焦工作：**
- **early-tool-start**（seam 2）：在 `reduce_stream` 中检测 `ContentBlockStart::ToolUse` → `ContentBlockStop` 后，preflight 校验 + `tokio::spawn` 提前 dispatch read-only tool。需 tool catalog + approval + loop-guard——可能随 wire-in 切片（需 `ToolDispatcher` 接通）。block-lifecycle 事件（`MessageStarted`/`ThinkingStarted` 等）可同期补上（在 `reduce_stream` 的 `ContentBlockStart`/`ContentBlockStop` 分支发 lifecycle 事件——需 CORE `Callback` 新方法或扩展 `StreamDelta`）。inline 流归约 + reactive recovery 均已就位。
- **subagent**（seam 3，sub-agent handoff/hold）、**cycle**（seam 1/4，per-file cycle state）：后续 guardrail。
- **opt-in `CapacityController`**（Gate A + seam 4 post-tool checkpoint + error-escalation）：独立 opt-in 切片，需完整 `CapacityController` 状态机。
- **cancel-token 注入**：transparent-retry 短路 + steer stale-drain + approval 审批等待脱出 + capacity recovery 短路（preflight + reactive）+ loop 顶取消检查，在 wire-in 步或单独小切片接入。
- **compaction 闭合项**：summary-prompt merge / attachment reinject / post-compact cleanup / enhancements / working-set pins / `emit_session_updated` 随 wire-in 切片接入。同样适用于 capacity recovery（preflight + reactive）。
- **`HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役**：所有 guardrail 吸收后，`handle_send_message` 改用 `HostAgentExecutor`，删 `handle_deepseek_turn`。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-11 §E block-lifecycle 事件落地，seam-2 流式生命周期，`feat/pluggable-framework-core`）：**

§E 的第十四个切片落地——把生产 `handle_deepseek_turn` 在流归约中合成、但内联归约器（第十二切片）留下为 no-op 的 block-lifecycle 事件接通：`reduce_stream` 现在 `ContentBlockStart`/`ContentBlockStop` 处合成 `MessageStarted`/`ThinkingStarted`/`ThinkingComplete`/`MessageComplete`，经 CORE `StreamDelta`（新增 4 lifecycle variant）→ `Callback::on_stream_delta` → `CallbackBridge` → 同名 `Event` variant 端到端流到 host UI 通道。这是 ROADMAP "early-tool-start" 焦点项的自包含子集——block-lifecycle 是 "可同期补上" 的部分，**不**需 `ToolDispatcher`（early-tool-start 的 speculative dispatch 仍需 `ToolDispatcher`，"可能随 wire-in 切片"），且是 early-tool-start 的前置（生产在 `ContentBlockStop` 发 `ToolCallStarted` 后才做 early dispatch）。本轮跨 3 文件 2 crate（CORE `StreamDelta` + `reduce_stream` + `CallbackBridge`），纯增量（新 `StreamDelta` variant 默认 no-op——所有既有 `Callback` impl 不受影响）；生产路径 `handle_deepseek_turn` 不受影响。

- **CORE `StreamDelta` 扩展**（`crates/agent/src/callback/mod.rs`）：`StreamDelta` 从 2 variant（`Text`/`Thinking`）扩为 6——新增 `MessageStarted { index }` / `ThinkingStarted { index }` / `ThinkingComplete { index }` / `MessageComplete { index }`（各携带 `index: usize`，1:1 映射到既有 `Event` variant）。枚举文档重写为 "two families"（content delta + block-lifecycle marker）。`CallbackSet`/`NoopCallback` 无改动（按引用转发 / 默认 no-op）。`noop_callback_defaults_are_callable` 测试加 2 个 lifecycle variant 调用保覆盖。
- **`reduce_stream` 合成**（`crates/agent-runtime/src/engine/host_executor.rs`）：
  - `ContentBlockStart::Text` 臂：insert 前发 `StreamDelta::MessageStarted { index }`。
  - `ContentBlockStart::Thinking` 臂：insert 前发 `StreamDelta::ThinkingStarted { index }`。
  - `ContentBlockStop { index }` 臂（原 no-op `{}`）：bind `index`，`blocks.get(&index)` 查块类型（**不 remove**——块留到 `finalize_blocks`）：`Thinking` → `ThinkingComplete`、`Text` → `MessageComplete`、`ToolUse` → 无 lifecycle（deferred to early-tool-start）。
  - `reduce_stream` doc 更新：lifecycle 已就位（text/thinking）；`ToolCallStarted` for tool blocks 仍 deferred。
- **`CallbackBridge::on_stream_delta`**（`callback_bridge.rs`）：match 扩 4 臂——`MessageStarted`→`Event::MessageStarted`、`ThinkingStarted`→`Event::ThinkingStarted`、`ThinkingComplete`→`Event::ThinkingComplete`、`MessageComplete`→`Event::MessageComplete`（既有 `Event` variant 全已存在，`events.rs:34-69`，各携带 `index: usize`）。模块 doc gap 表 `on_stream_delta` 行 + "Block-lifecycle events … not yet bridged" 段更新为已桥接；`ToolCallStarted` for tool blocks 仍是 deferred gap。
- **既有测试调整**：`stream_emits_text_deltas_to_callback` + `stream_emits_thinking_deltas_to_callback` 原用位置断言 `deltas[0]`/`deltas[1]`——lifecycle 事件现在交错插入，`deltas[0]` 变为 `MessageStarted`/`ThinkingStarted`。改为 filter-by-variant（`.filter_map` 取 `Text`/`Thinking` content delta），与既有 `stream_deltas_flow_through_callback_bridge` 的 filter 风格一致。零行为变化，只调整断言策略。
- **2 个新测试**：`stream_emits_block_lifecycle_events`（thinking block(0) + text block(1) → `DeltaRecorder` 捕获完整交错序列 `ThinkingStarted(0)` → `Thinking("pondering")` → `ThinkingComplete(0)` → `MessageStarted(1)` → `Text("answer")` → `MessageComplete(1)`，用 `delta_tags` helper 渲染为可读 tag 串断言顺序 + index）、`stream_lifecycle_events_flow_through_callback_bridge`（端到端：executor → `CallbackBridge` → `Event::ThinkingStarted{0}`/`ThinkingComplete{0}`/`MessageStarted{1}`/`MessageComplete{1}` 在 Event channel，且 `ThinkingComplete(0)` 先于 `MessageStarted(1)` 证块序）。共 50 个 host_executor 测试通过（48 既有 + 2 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime`（含 `--tests`）零新 warning（10 warning 均既有，与本轮无关）；`cargo test -p codesmith-agent --lib` 79 通过；`cargo test -p codesmith-agent-runtime --lib host_executor` 50 通过（48 既有 + 2 新）；`cargo test -p codesmith-agent-runtime --lib callback_bridge` 5 通过（模块测试不变，2 个 host_executor 的 `*_callback_bridge` 测试含在 host_executor 计数）；`cargo test -p codesmith-agent-runtime --lib` 1056 通过（0 失败、2 ignored，原 1054 +2；1 个 MCP streamable-http 测试在并发全量跑时偶发失败、隔离/重跑通过，与本轮无关）；`cargo build --workspace` 全绿（tui 143 warning 均既有死代码）。

**已知设计取舍（本轮缺口，by design）：**
- **`ToolCallStarted` for tool blocks 延后**：生产在 `ContentBlockStop`（tool 块）解析 input 后发 `Event::ToolCallStarted { id, name, input }`（wire tool id）。`CallbackBridge` 现在 `on_tool_start`（execute time）发 `ToolCallStarted`（合成 `bridge-{n}` id）。移到 stream-time 需重构 bridge 避免重复 + 透传 wire tool id——与 early-tool-start 耦合，同切片接入。
- **`MessageComplete` 在 `ContentBlockStop` per-block发**：生产 post-stream、gated by `pending_message_complete`（只发一次）。本切片每 text 块 `ContentBlockStop` 发一次；多 text 块 → 多 `MessageComplete`。可接受——`Event::MessageComplete { index }` 携带 index 供 UI 关联。
- **mid-flight 死亡的块无 `MessageComplete`**：未到 `ContentBlockStop` 的块不发 complete——匹配生产（只 complete 到达 Stop 的块）。

**下一聚焦工作：**
- **early-tool-start**（seam 2）：在 `reduce_stream` 中检测 `ContentBlockStart::ToolUse` → `ContentBlockStop` 后，preflight 校验 + `tokio::spawn` 提前 dispatch read-only tool。需 tool catalog + approval + loop-guard——可能随 wire-in 切片（需 `ToolDispatcher` 接通，或用框架 `Tool::capabilities()` 做静态近似 + loop-guard 线程化）。block-lifecycle 已就位（`ToolCallStarted` 的 stream-time 合成 + bridge 去重可在此切片同期补上）。inline 流归约 + reactive recovery + block-lifecycle 均已就位。
- **subagent**（seam 3，sub-agent handoff/hold）、**cycle**（seam 1/4，per-file cycle state）：后续 guardrail。
- **opt-in `CapacityController`**（Gate A + seam 4 post-tool checkpoint + error-escalation）：独立 opt-in 切片，需完整 `CapacityController` 状态机。
- **cancel-token 注入**：transparent-retry 短路 + steer stale-drain + approval 审批等待脱出 + capacity recovery 短路（preflight + reactive）+ loop 顶取消检查，在 wire-in 步或单独小切片接入。
- **compaction 闭合项**：summary-prompt merge / attachment reinject / post-compact cleanup / enhancements / working-set pins / `emit_session_updated` 随 wire-in 切片接入。同样适用于 capacity recovery（preflight + reactive）。
- **`HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役**：所有 guardrail 吸收后，`handle_send_message` 改用 `HostAgentExecutor`，删 `handle_deepseek_turn`。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-11 §E early-tool-start speculative dispatch 落地，seam-2 提前 dispatch，`feat/pluggable-framework-core`）：**

§E 的第十五个切片落地——把生产 `handle_deepseek_turn` 的 early-tool-start（seam 2）吸收进 `HostAgentExecutor`：内联归约器 `reduce_stream` 在 `ContentBlockStop`（tool 块）处 finalize input，若工具 `early_start_safe`（read-only + 无 approval + 无 code-exec/file-write）则 `tokio::spawn` 立即执行，使结果在执行器到达 tool loop 时就绪；tool loop 按 wire id pop 出任务，重验 name+input（模型可能在块关闭后修订 args），命中则 await `JoinHandle` 复用结果而非重跑工具（镜像 `turn_loop` 的 `early_tool_tasks` map + `early_tool_start_safe`，`turn_loop.rs:975-1135` spawn / `1598-1803` reuse）。args 不匹配 / loop-guard block / 审批拒绝 / `NotAvailable` 路径 pop + `Drop`-abort 孤儿任务。map 是 per-step 本地 `HashMap`（非 executor 字段——与 LSP/steer/approval/compaction/capacity 不同，此 guardrail 无跨 step 状态），故构造器签名不变。本轮纯增量（`host_executor.rs` 一个文件 + 模块文档），零既有调用点行为改动；生产路径 `handle_deepseek_turn` 不受影响。

- **`early_start_safe` free fn**（新）：镜像 `turn_loop::early_tool_start_safe` 的最终复合门控——`ReadOnly` present AND none of `{RequiresApproval, ExecutesCode, WritesFiles}`。框架 `Tool` trait 仅暴露 `capabilities()`，故这是**静态近似**：production 额外查 `metadata.is_read_only && supports_parallel && !interactive && validate_input().is_ok() && approval_requirement_for(...) == Auto` + tool-catalog allowlist（not-MCP / not-code-exec / not-tool-search），这些 per-input / per-metadata 面不可从框架 `Tool` 触达，延后到 wire-in 步（§E design note，同 `requires_approval` gap）。`Network`/`Sandboxable` 不 disqualify（read-only network fetch 可提前起）。
- **`EarlyToolTask` struct + `Drop` abort**（新）：`{ name, input, handle: Option<JoinHandle<Result<ToolResult, ToolError>>> }`。`handle` 用 `Option` 包裹使 reuse 路径可 `Option::take` 出来 `.await`（实现 `Drop` 的类型不能让字段被 move out）。`Drop` abort `JoinHandle`——孤儿任务（未存活进 `tool_uses` 的块 / args 不匹配 / blocked / denied / NotAvailable）永不泄漏后台任务；abort 已完成任务是 no-op，故 reuse 路径的 await-then-drop 安全。
- **`finalize_tool_input` helper**（新，extracted）：`input_buf`（streamed `InputJsonDelta` 片段）→ fallback `start_input`（`ContentBlockStart::ToolUse` 携带）→ fallback 空 object，同 CORE `accumulate_stream` 尾部逻辑。提取使 `finalize_blocks`（stream end）与 early-start spawn（`ContentBlockStop`，mid-stream）finalize 完全一致。
- **`reduce_stream` ContentBlockStop tool 分支**（原 no-op `{}`，现 spawn）：finalize input → `self.tools.get(name)` → `early_start_safe(&tool.capabilities())` → `Arc::clone(tool)` + move 进 `tokio::spawn(async move { tool.run(input).await })`（Arc cheap refcount bump；owned by async block 故 borrow `'static`）→ `early_tasks.insert(id, EarlyToolTask { ... })`。spawn 立即返回（非阻塞），流继续消费。签名 +`early_tasks: &mut HashMap<String, EarlyToolTask>` 参数。
- **`stream_with_transparent_retry` 签名**：+`early_tasks` 参数，透传给 `reduce_stream(stream, early_tasks)`。
- **`run_inner` tool loop 复用/abort**：per-step 声明 `let mut early_tasks: HashMap<String, EarlyToolTask> = HashMap::new();`（stream 前），传 `&mut early_tasks` 给 stream 调用。tool loop：
  - `AttemptDecision::Block`（loop-guard 第 3 次同 call）→ `early_tasks.remove(&id)`（Drop abort）。
  - `Proceed` + approval `Ok(())` → `match early_tasks.remove(&id)`：`Some(mut early) if early.name == name && early.input == input` → `handle.take().expect(...)` + `handle.await`（Ok→result，Err(join_err)→`ToolError::execution_failed("Early tool execution task failed: ...")`）；`Some(_revised)`（args 改）→ `tool.run(input.clone()).await`（dropped `EarlyToolTask` Drop abort 孤儿）；`None` → `tool.run(input.clone()).await`。
  - approval `Err(denial)` → `early_tasks.remove(&id)`（防御性——early-start-safe 工具不需审批，此路径本无任务）。
  - `NotAvailable`（无注册工具）→ `early_tasks.remove(&id)`（防御性——`reduce_stream` 只为注册工具 spawn）。
  - tool loop 后 `early_tasks.clear()`（防御性 abort 残留）。
  - `continue` 路径（capacity `RetryStep` / reactive `RecoveredContextOverflow`）stream 未开起或死前无 content ⇒ 无 `ContentBlockStop` ⇒ map 为空，drop 不泄漏。
- **6 个新测试 + 4 个 test double**：`SignalingSpec`（read-only，execute 时 `Notify::notify_one`——证明**流期间** dispatch）、`CountingSpec`（read-only，`AtomicU32` 计数——证明 reuse count==1）、`PanickingSpec`（read-only，execute panic——证 JoinError surfaces 为 `execution_failed`）、`CountingWriteSpec`（`WritesFiles`，证非 read-only 不提前 dispatch）。测试：
  - `early_start_safe_allows_readonly` / `early_start_safe_disqualifies_non_readonly`（单元：gate 覆盖 ReadOnly + Network/Sandboxable 放行、空/WritesFiles/ExecutesCode/RequiresApproval 拒绝）。
  - `early_start_dispatches_readonly_tool_during_stream`（executor 跑在 `tokio::spawn` 上，`notify.notified().await` 在 executor 返回前 resolve——唯一解释是 early dispatch）。
  - `early_start_reuses_result_without_re_running`（count==1，证 early task 被 reuse 而非 execute-time 重跑）。
  - `early_start_join_error_surfaces_execution_failed`（panic 的 early task → `JoinHandle` Err → `ToolError::execution_failed`，transcript 记 error result，turn 干净结束 NoToolCalls；`std::panic::set_hook` 抑制 stderr panic 输出）。
  - `early_start_skips_non_readonly_tool`（WritesFiles 工具 count==1，执行器尊重 gate 不 double-execute）。
  共 56 个 host_executor 测试通过（50 既有 + 6 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；`cargo test -p codesmith-agent-runtime --lib host_executor` 56 通过（50 既有 + 6 新 early-tool-start）；`cargo test -p codesmith-agent-runtime --lib` 1061 通过、1 失败（`mcp::tests::streamable_http_stale_session_reconnects_and_retries_tool_call`——既有偶发，隔离重跑通过，与本轮无关）、2 ignored（原 1056 +6 = 1062，-1 flaky = 1061 passed）；`cargo test -p codesmith-agent --lib` 79 通过；`cargo build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**已知设计取舍（本轮缺口，by design）：**
- **`ToolCallStarted` 不在 stream-time 发**：生产在 `ContentBlockStop`（tool 块）解析 input 后发 `Event::ToolCallStarted { id, name, input }`（wire tool id）。`CallbackBridge` 现在在 `on_tool_start`（execute time）发 `ToolCallStarted`（合成 `bridge-{n}` id）。移到 stream-time 会用不同 `bridge-{m}` id 与 execute-time `on_tool_start` 重复发；去重需 trait-surface 改（透传 wire id）或 bridge 层 name+input pairing。execute-time `on_tool_start` 仍是 tool-call-start UI 的单一真源。延后。
- **静态-only 安全门**：`early_start_safe` 仅查 `capabilities()`——per-input `validate_input` / `is_interactive` / `approval_requirement_for` 不可从框架 `Tool` 触达。实际效果：read-only 工具若 per-input 校验会拒 args，仍被 speculative spawn，结果在 execute time 丢弃——浪费工，非正确性 bug。
- **spawn 时不查 loop-guard**：production `early_tool_start_safe` 查 `LoopGuard` 避免为第 3 次同 call 提前起。本执行器 spawn 时不查（避免 `&mut LoopGuard` 线程化过 streaming 路径 + 双计 attempt）。execute-time `record_attempt` 是单一真源：被 loop-guard block 的 speculative task 在 tool loop pop + Drop-abort。浪费工 = 一个被立即 abort 的 read-only 任务（廉价）。
- **无 per-input approval / interactive 检查**：`early_start_safe` 静态排除 `RequiresApproval`-tagged 工具，但 per-input `Required`（如 `exec_shell rm`）不可见。此类工具会被 speculative 起，若 execute-time 审批返 `Required` 则 task 被 pop + Drop-abort。浪费工，非正确性 bug。随 wire-in 步 per-input approval 面接入。
- **无 cancel-token 短路**：取消的 turn 仍为 partial stream 中关闭的 tool 块 spawn 任务；step 末 `early_tasks.clear()`（+ map drop 时）abort 它们。工量被 partial stream 的 tool 块数 bound。cancel-token 在 wire-in 步接入。

**下一聚焦工作：**
- **subagent**（seam 3，sub-agent handoff/hold）、**cycle**（seam 1/4，per-file cycle state）：剩余两个 guardrail（十 guardrail 已吸收八个——compaction/capacity/approval/steer/transparent-retry/early-tool-start/LSP/loop-guard）。
- **opt-in `CapacityController`**（Gate A + seam 4 post-tool checkpoint + error-escalation）：独立 opt-in 切片，需完整 `CapacityController` 状态机。
- **cancel-token 注入**：transparent-retry 短路 + steer stale-drain + approval 审批等待脱出 + capacity recovery 短路（preflight + reactive）+ early-tool-start spawn 短路 + loop 顶取消检查，在 wire-in 步或单独小切片接入。
- **compaction 闭合项**：summary-prompt merge / attachment reinject / post-compact cleanup / enhancements / working-set pins / `emit_session_updated` 随 wire-in 切片接入。同样适用于 capacity recovery（preflight + reactive）。
- **`ToolCallStarted` stream-time 合成 + bridge 去重**：需 `Callback::on_tool_start` 透传 wire id 或 bridge 层 name+input pairing——与 wire-in 耦合，可同期接入。
- **`HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役**：所有 guardrail 吸收后，`handle_send_message` 改用 `HostAgentExecutor`，删 `handle_deepseek_turn`。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-11 §E subagent post-stream completion drain 落地，第九个 guardrail，seam-2 非阻塞，`feat/pluggable-framework-core`）：**

§E 的第十六个切片落地——把生产 `handle_deepseek_turn` 的 sub-agent completion handoff 吸收进
`HostAgentExecutor`。当模型在某步无 tool calls 结束时，执行器 `try_recv` 排空
`rx_subagent_completion`，把已到达的子 agent 完成事件（queued during inference 或 turn 间到达）
作为 `<codesmith:runtime_event kind="subagent_completion">` sentinel user 消息注入 transcript 并
resume turn（而非结束 turn）——镜像 `turn_loop.rs:1317-1397` 的非阻塞 drain + late drain
（`1501-1532`）。这兑现了 `prompts/base.md` 对模型的 sentinel 契约。本轮纯增量（`host_executor.rs` +
`turn_loop.rs` 一处 visibility 放宽），零既有调用点行为改动；生产路径 `handle_deepseek_turn` 不受影响。

- **选型：subagent 先于 cycle。** ROADMAP 列剩余两 guardrail：subagent + cycle。经核实 **cycle
  （`cycle_manager.rs`）不在 `handle_deepseek_turn` 内**——它是 768K-token 检查点重启，跑在
  `handle_send_message` 里 `handle_deepseek_turn` 返回 `Completed` 之后（`engine/mod.rs:1199-1201`），
  是 post-turn / Session 级关注（swap `Session.messages` / `cycle_count` / `cycle_briefings`），不契合
  executor 的四 seam loop 模型，且其状态不经 `ChatHistory` 可达——**deferred 到 wire-in 步**（接
  `handle_send_message` 时自然落地）。subagent 是真正的 in-loop guardrail（seam-2 post-stream，
  `turn_loop.rs:1296-1567`），是退役 `handle_deepseek_turn` 的阻塞项，故本轮吸收之。
- **吸收的是非阻塞 drain；阻塞 hold deferred。** 镜像 steer/capacity 的拆法。生产有两段 drain：
  非阻塞 `try_recv`（`turn_loop.rs:1318` + `1506`）+ 阻塞 hold（`biased select!` over cancel /
  completion `recv().await` / steer `recv().await`，`turn_loop.rs:1330-1368`）。本轮吸收非阻塞 drain；
  阻塞 hold 需 `CancellationToken`（跨 guardrail 既有 gap）+ `SubAgentApi::running_count` + tokio-Mutex
  receiver（steer receiver 须同步迁移），deferred 到 wire-in。executor 无 thinking-only /
  goal-continuation / REPL 分支，故 `tool_uses.is_empty()` 臂**一个 drain 点**同时覆盖生产的主 drain
  + late drain。
- **`subagent` 字段**：`HostAgentExecutor` 新增
  `subagent: Option<Arc<std::sync::Mutex<mpsc::Receiver<SubAgentCompletion>>>>` + 构造器第 11 参数
  （非-subagent embed/测试传 `None`——54 个既有测试构造器各加一个 `None`）。**mutex 选
  `std::sync::Mutex`**（同 steer——`try_recv` 同步、锁不跨 `await`；最接近的同类先例）。forward-compat
  注记：阻塞 hold 切片（future）会把 steer + subagent 两个 receiver 一并迁 `tokio::sync::Mutex`（同
  approval 先例）以支持 `recv().await`。receiver 跨 `run` 调用持久（匹配生产
  `Engine.rx_subagent_completion` 字段——turn 间到达的 completion 在下一 turn 首 drain 取出）。
- **`subagent_completion_runtime_message` → `pub(crate)`**（`turn_loop.rs:2820`）：单一源，避免
  sentinel 格式漂移（同 `summarize_text` / `accumulate_stream` 放宽先例）。executor 直接引用
  `super::turn_loop::subagent_completion_runtime_message`，零格式重复。
- **drain 接线（seam 2）**：`run_inner` 的 `tool_uses.is_empty()` 臂（原直接 return `NoToolCalls`）
  改为先 `probe.lock()` + `try_recv` 循环排空 completion（锁在同步 try_recv 后立即 drop，不跨
  `history.push` 的 await——匹配 steer/LSP 先例）；非空则逐条 `history.push(subagent_completion_runtime_message(&c.payload))`、
  发 `"Resuming turn with N sub-agent completion(s)"` status、`step += 1`（匹配生产 `turn.next_step()`——
  subagent resume 是新 step，非"重试同 step"，故 `max_steps` bound 仍覆盖 completion 链；既有
  capacity/reactive `continue` 是重试同 step 故不增，语义不同）、`continue`。空则（阻塞 hold deferred）
  return `NoToolCalls`。
- **刻意部分桥接（by design / gaps）**：
  - **阻塞 hold deferred**：`should_hold_turn_for_subagents` + `biased select!` 需
    `CancellationToken` + `SubAgentApi::running_count` + tokio-Mutex receiver。故 `subagent` 有值但队列
    空时 turn 立即结束（`NoToolCalls`）而非 hold——子 agent 的 completion 在下一 turn 的 drain 浮现
    而非 mid-turn。随 wire-in 接入。
  - **`ContextPatch` apply deferred**：生产 drain 后 tighten-only apply `auto_approve`/`trust_mode`
    （`turn_loop.rs:1373-1388`）；`ChatHistory` 不暴露这些（host 耦合，同 compaction 的 working-set/
    cycle-state gap），故 patches 丢弃。生产今日 `emit_parent_completion` 硬编码 `context_patch: None`，
    故 defer 是安全 no-op。随 wire-in（`Session` 可达）。
  - **无 `<turn_meta>` 富化**：sentinel 用纯 `subagent_completion_runtime_message`（role `user`，无
    `user_text_message_with_turn_metadata` 包裹），同 steer/LSP flush 既有 gap。
  - **steer post-stream resume deferred**：生产无 tool calls 且 `pending_steers` 非空时也 resume
    （`turn_loop.rs:1297`）；executor 的 steer 是 seam-1 try_recv，post-stream steer 在下一步首 pre-request
    drain 取出——同 steer 切片的三次级排空点 deferral。随阻塞 hold 同期接入（`biased select!` steer 臂）。
- **test doubles**：`subagent_channel()`（`mpsc::channel::<SubAgentCompletion>(64)` +
  `Arc::new(Mutex::new(rx))`，镜像 `steer_channel`）、`completion(summary)` 构造器（payload =
  `summary + sentinel`，`context_patch: None`）、`has_subagent_completion_msg(messages)` 断言 helper。
  4 个新测试：`subagent_none_is_noop`（无 receiver→NoToolCalls、无 sentinel、1 次 stream）、
  `subagent_empty_queue_returns_no_tool_calls`（receiver 空队列→NoToolCalls、无 resume）、
  `subagent_drain_injects_queued_completions_and_resumes`（2 条预排队→transcript 出现 2 条 sentinel user
  消息、第 2 次 stream 请求含两条 sentinel、2 次 stream 后 NoToolCalls、status "Resuming turn with 2
  sub-agent completion(s)"）、`subagent_picks_up_completion_queued_between_runs`（**receiver 持久证明**：
  run1 无 completion 干净收尾→turn 间推 1 条 completion→run2 同 executor + 新 Session 首 drain 取出→
  transcript 出现、run2 第 2 次请求含它——per-run 局部 receiver 做不到，证 `Arc<Mutex<Receiver>>` 跨
  run 持久）。共 60 个 host_executor 测试通过（56 既有 + 4 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime`
（lib）零 warning；`cargo test -p codesmith-agent --lib` 79 通过；`cargo test -p codesmith-agent-runtime --lib
host_executor` 60 通过（56 既有 + 4 新 subagent）；`cargo test -p codesmith-agent-runtime --lib` 1066 通过、
1 失败（`mcp::tests::streamable_http_stale_session_reconnects_and_retries_tool_call`——既有偶发，隔离重跑通过，
与本轮无关）、2 ignored（原 1061 +4 = 1065 passed，+1 因 flaky 计入 failed 故 1066 passed/1 failed）；
test build 10 warning 均既有（零新 warning）；`cargo build --workspace` 全绿（tui 143 warning 均既有死代码，
与本轮无关）。

**下一聚焦工作：**
- **cycle（wire-in 步）**：cycle 是 post-turn / Session 级 checkpoint-restart，不在 turn loop 内；
  随 `HostAgentExecutor` 接入 `handle_send_message` 时落地（`maybe_advance_cycle` 是 turn 返回
  `Completed` 后的 post-turn 步）。本轮已从 host_executor 的 seam-1/seam-4 注释移除"cycle land
  here later"误导，改为"post-turn concern, deferred to wire-in"。
- **subagent 阻塞 hold**（`biased select!` for running children）：需 cancel-token + steer/subagent
  receiver 迁 tokio mutex + `SubAgentApi::running_count`。随 wire-in 或单独小切片。
- **opt-in `CapacityController`**（Gate A + seam 4 post-tool checkpoint + error-escalation）：独立 opt-in
  切片，需完整 `CapacityController` 状态机。
- **cancel-token 注入**：transparent-retry 短路 + steer stale-drain + approval 审批等待脱出 + capacity
  recovery 短路（preflight + reactive）+ early-tool-start spawn 短路 + subagent 阻塞 hold + loop 顶取消检查，
  在 wire-in 步或单独小切片接入。
- **compaction 闭合项**：summary-prompt merge / attachment reinject / post-compact cleanup / enhancements /
  working-set pins / `emit_session_updated` 随 wire-in 切片接入。同样适用于 capacity recovery + subagent
  的 `ContextPatch` apply。
- **`ToolCallStarted` stream-time 合成 + bridge 去重**：需 `Callback::on_tool_start` 透传 wire id 或
  bridge 层 name+input pairing——与 wire-in 耦合，可同期接入。
- **`HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役**：剩余 in-loop guardrail 已吸收（九个——
  compaction/capacity/approval/steer/transparent-retry/early-tool-start/subagent/LSP/loop-guard）；cycle 是
  post-turn 非 in-loop。阻塞 hold + cancel-token + 闭合项就位后即可接入。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

**进度（2026-07-11 §E cancel-token 注入落地，第十个 guardrail，跨切面取消，`feat/pluggable-framework-core`）：**

§E 的第十七个切片落地——把 `CancellationToken` 注入 `HostAgentExecutor`，镜像生产
`handle_deepseek_turn` 的取消检查点：取消的 turn 短路 transparent-retry、脱出 approval 等待、
bound capacity-recovery `continue` 循环，并返回 `StopReason::Interrupted`（而非 `Error`）。本轮
纯增量（一个新字段 + helper + 7 个检查点 + 模块文档 + 7 个新测试），零既有调用点行为改动；
生产路径 `handle_deepseek_turn` 不受影响。起点是上一轮未提交的 `StopReason::Interrupted` 变体
（`crates/agent/src/callback/mod.rs`）。

- **`cancel_token: Option<CancellationToken>` 字段**（构造器第 12 参数——60 个既有测试构造器各加一个
  `None`）。`None` ⇒ `is_cancelled()` 返回 `false`，所有取消检查点全 no-op（embed/测试不需取消时零影响）。
  `Some` 时镜像生产 7 个取消检查点。字段用 `tokio_util::sync::CancellationToken`（workspace 已有依赖，
  feature unification 使 `sync` 可用——既有 5 个文件已用）。
- **`is_cancelled()` helper**：`self.cancel_token.as_ref().map_or(false, |t| t.is_cancelled())`。
- **`StreamRoundOutcome::Interrupted` 变体**：`stream_with_transparent_retry` → `run_inner` 的取消信号
  （区别于 `Err`——取消不传播为错误，而是 `Interrupted`）。
- **7 个取消检查点（10 中 7 个吸收）：**
  - **Checkpoint A（loop-top）**：`run_inner` 的 `loop` 顶、`step >= max_steps` 之前——`is_cancelled()` →
    emit "Request cancelled" + `on_complete(Interrupted)` + return `Interrupted`。bound 所有 `continue` 循环
    （capacity `RetryStep` / reactive `RecoveredContextOverflow` / subagent resume）——取消在 recovery 期间
    落地时在下一步首被捕获。
  - **Checkpoint B（stream-open race）**：`stream_with_transparent_retry` 内 `biased select!` over
    `cancel_token.cancelled()` vs `create_message_stream`——cancel 赢则 return `Interrupted`（流未开就中止）。
    用 `self.cancel_token.clone()`（cheap Arc bump）+ `pending::<()>()` 兜底 `None`，避免 `&self` 借用冲突。
  - **Checkpoint C（transparent-retry `!cancelled`）**：`Empty` 臂内 `is_cancelled()` → return `Interrupted`
    （不 retry），镜像 `should_transparently_retry_stream` 的 `!cancelled` 守卫。
  - **Checkpoint D（post-stream）**：`Complete`/`Partial` 臂内 `is_cancelled()` → return `Interrupted`
    （丢弃已产出的 content——镜像生产 post-stream gate）。
  - **Checkpoint G（post-tool-loop）**：tool 循环后、`loop_guard_halt` 检查之前——`is_cancelled()` →
    `Interrupted`（cancel 优先于 halt，镜像 `turn_loop.rs:2665-2671` cancel 优先于 `turn_error`）。
    工具执行时取消 token（如 `CancelOnCallSpec`）在此被捕获。
  - **Approval cancel race**：`request_approval` 的 `recv().await` 循环内加 `biased select!` over
    `cancel_token.cancelled()` vs `guard.recv()`——cancel 赢则 return `Err("Request cancelled while awaiting
    approval")`（tool error 回灌；Checkpoint G 捕获 cancel）。同 `cancel_token.clone()` + `pending()` 模式。
  - **Steer stale-drain**：`drain_stale_steers()` pub 方法——host 在 `run` 前调用（镜像
    `handle_send_message` 开头的 `while rx_steer.try_recv().is_ok() {}`，`engine/mod.rs:1013-1014`），
    非 `run_inner` 内部（否则会丢弃 host 为当前 turn 排入的 steer——前一轮 stale drain vs 当前 turn
    per-step drain 的语义区分）。
- **deferred（3 个不吸收）：**
  - **Checkpoint E（subagent 阻塞 hold cancel race）**：阻塞 hold 本身 deferred（需
    `SubAgentApi::running_count` + tokio-Mutex receiver）。其 cancel race 属该切片。post-stream drain 的
    cancel 由 Checkpoint A bound（loop-top bound `continue`）。
  - **Checkpoint F（thinking-only status）**：executor 无 thinking-only 分支——N/A。
  - **Early-tool-start spawn short-circuit**：生产 `early_tool_start_safe` 不检查 cancel（by design）。
    bounded by `early_tasks.clear()`/`Drop`。
- **test doubles**：`CancelOnCallSpec`（read-only ToolSpec，execute 时取消 token——证明 Checkpoint G）、
  `MockLlm` 扩展 `cancel_on_stream` 副作用（`create_message_stream` 的 async block 内取消 token——证明
  Checkpoint C/D；cancel 在 async block 内而非 sync part 以确保 Checkpoint B 的 `biased select!` 不先赢）。
  7 个新测试：`cancel_none_is_noop`（无 token→NoToolCalls）、`cancel_pre_cancelled_returns_interrupted`
  （预取消→Checkpoint A→Interrupted、0 次 stream）、`cancel_between_steps_returns_interrupted`（工具执行时
  取消→Checkpoint G→Interrupted、1 次 stream）、`cancel_short_circuits_transparent_retry`（流死空 + mock
  取消→Checkpoint C→不 retry→Interrupted）、`cancel_after_clean_stream_returns_interrupted`（流干净完成 +
  mock 取消→Checkpoint D→content 丢弃→Interrupted、history.len()==1）、`cancel_during_approval_returns_interrupted`
  （approval 阻塞 + 后台 50ms 取消→approval select!→tool error→Checkpoint G→Interrupted）、
  `steer_stale_drain_discards_previous_turn_steers`（排入 2 条 steer→drain_stale_steers()→steer 不出现在
  request/transcript）。共 67 个 host_executor 测试通过（60 既有 + 7 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime`
（lib）零 warning；`cargo test -p codesmith-agent --lib` 79 通过；`cargo test -p codesmith-agent-runtime --lib
host_executor` 67 通过（60 既有 + 7 新 cancel-token）；`cargo test -p codesmith-agent-runtime --lib` 1073
通过、0 失败、2 ignored；`cargo build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- **`HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役**：剩余 in-loop guardrail 已吸收（十个——
  compaction/capacity/approval/steer/transparent-retry/early-tool-start/subagent/LSP/loop-guard/cancel-token）；
  cycle 是 post-turn 非 in-loop。阻塞 hold + 闭合项就位后即可接入。
- **subagent 阻塞 hold**（`biased select!` for running children）：cancel-token 已就位（本轮吸收），仍需
  steer/subagent receiver 迁 tokio mutex + `SubAgentApi::running_count`。随 wire-in 或单独小切片。
- **opt-in `CapacityController`**（Gate A + seam 4 post-tool checkpoint + error-escalation）：独立 opt-in
  切片，需完整 `CapacityController` 状态机。
- **compaction 闭合项**：summary-prompt merge / attachment reinject / post-compact cleanup / enhancements /
  working-set pins / `emit_session_updated` 随 wire-in 切片接入。同样适用于 capacity recovery + subagent
  的 `ContextPatch` apply。
- **`ToolCallStarted` stream-time 合成 + bridge 去重**：需 `Callback::on_tool_start` 透传 wire id 或
  bridge 层 name+input pairing——与 wire-in 耦合，可同期接入。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-11 §E subagent blocking hold 落地，第十一个 guardrail 收尾 + 最后一个 in-loop gap，seam-2 阻塞，`feat/pluggable-framework-core`）：**

§E 的第十八个切片落地——当模型完成一步无工具调用、非阻塞 drain 为空、但子 agent 仍在运行时（`should_hold_turn_for_subagents(0, running_count)`），`run_inner` 在 seam-2（post-stream、drain 之后、inject+resume 之前）发起一个 `biased select!` 阻塞 hold：cancel arm（Checkpoint E）/ completion `recv().await` arm（push + `try_recv` 批量 drain → 注入 sentinel → resume）/ steer `recv().await` arm（trim → push user message → emit status → step+=1 → continue），镜像 `turn_loop.rs:1321-1397`。这是最后一个 in-loop guardrail gap，也是 `handle_deepseek_turn` 退役（wire-in）的直接前置——阻塞 hold + 闭合项就位后即可接入。本轮纯增量（`host_executor.rs` 单文件 + docs），零既有调用点行为改动；生产路径 `handle_deepseek_turn` 不受影响。

- **`should_hold_turn_for_subagents(queued, running)` free fn**（`host_executor.rs:676`）：`queued > 0 || running > 0`，镜像 `turn_loop.rs:2846-2848`。非阻塞 drain 已吸收（第十六切片）；本轮补上阻塞 hold 的判定。
- **steer + subagent 字段迁移 `std::sync::Mutex` → `tokio::sync::Mutex`**：`biased select!` 的 `recv().await` arm 的 guard 必须 cross `await`（与 approval 字段同 rationale）。`drain_steers` / `drain_stale_steers`（`pub fn` → `pub async fn`）/ 非阻塞 subagent drain 的 `lock().expect("poisoned")` 改为 `lock().await`——机械改动，行为不变（无竞争单消费者锁）。
- **`subagent_api: Option<Arc<dyn SubAgentApi>>` 字段**（构造器第 13 参数——65 个既有测试构造器各加一个 `None`）：最小注入（不拉入完整 `HostServices`），镜像 `LspProbe` 的 `Arc<dyn LspManagerApi>` 模式。仅用于 `running_count().await`；`None` ⇒ 阻塞 hold 禁用。`SubAgentApi` 是 `#[async_trait]` + `Send + Sync`，已是 `agent-runtime` 依赖（`host_services.rs:196`）。
- **阻塞 hold 逻辑**（`tool_uses.is_empty()` arm，非阻塞 drain 之后）：`completions.is_empty()` && `subagent_api` is Some && `running_count().await > 0` 时触发。emit `"Waiting on {running} sub-agent(s) to complete..."`。`Arc::clone(probe)` 后 `sub_guard = sub_arc.lock().await`——guard 借用 local Arc，steer arm 可自由访问 `self.steer`。`biased select!`：
  - **cancel arm**（token present → `token.cancelled().await`，否则 `pending()` fallback）：emit `"Request cancelled while waiting for sub-agents"` + `callback.on_complete(Interrupted)` + `return Ok(StopReason::Interrupted)`——**Checkpoint E**，已吸收（guardrail 10）。
  - **completion arm**（`sub_guard.recv()`）：push completion，`select!` 之后 `try_recv` drain 批量 extras（`turn_loop.rs:1342-1345`），fall through 到既有 inject+resume 路径。
  - **steer arm**（steer present → `rx.lock().await.recv().await`，否则 `pending()` fallback）：trim → skip-empty → push user message → emit `"Steer input accepted"` status → `step += 1` → `continue`。**闭合 steer post-stream resume gap**（第十七切片 deferred 的 secondary drain site）。
- **刻意部分桥接（by design / gaps）**：
  - **`ContextPatch` apply 仍 deferred**：生产 drain 每个 completion 的 `context_patch` 并 tighten-only 应用（`auto_approve`/`trust_mode`→`false`；loosen 拒绝）到 `Session` + `config.trust_mode`（`turn_loop.rs:1373-1388`）。`ChatHistory` 无接口，生产今天 hardcode `context_patch: None`——安全 no-op。随 wire-in 切片接入（同样适用于 capacity recovery）。
  - **mid-stream buffer steer drain 仍 deferred**：streaming-lifecycle-specific，与本轮 post-stream hold 的 steer arm 不同路径。随 wire-in 切片接入。
  - **`<turn_meta>` enrichment**：steer / sentinel 消息无 `user_text_message_with_turn_metadata` wrapper——与 LSP flush / compaction / steer drain 同 gap。
  - **Late-drain 折叠进单一非阻塞 drain**：executor 无 thinking-only / REPL / goal-continuation 分支。
  - **`UnboundedReceiver`（生产）vs bounded `Receiver`（executor）**：shape 差异，wire-in 时 reconcile。
- **test doubles**：`FakeSubAgentApi`（`#[async_trait]`，`VecDeque<usize>` 配置 `running_count` 序列——每次 `running_count()` pop front，耗尽返回 0；`list`/`cleanup`/`live_running_snapshots` no-op）。设计意图：测试可声明"首次 poll running=1（hold 触发），二次 poll running=0（hold skip）"——`FakeSubAgentApi::new(vec![1])`。`steer_channel()` / `subagent_channel()` 迁移 `tokio::sync::Mutex::new(...)`。
  6 个新测试：`subagent_hold_waits_for_running_children_then_resumes`（running=1 + 后台 50ms push completion → hold 触发 → "Waiting on 1 sub-agent(s)" status → completion 注入 → resume → NoToolCalls、2 次 stream）、`subagent_hold_cancel_returns_interrupted`（running=1 + 后台 50ms cancel → Checkpoint E → Interrupted、1 次 stream 证明 cancel 在 stream 之后非 Checkpoint A）、`subagent_hold_steer_arm_resumes_with_steered_text`（running=1 + 后台 50ms push steer → steer arm → push user message → resume → request 含 steer 文本 → NoToolCalls、2 次 stream）、`subagent_hold_no_subagent_api_skips_hold`（无 `subagent_api` → hold skip → NoToolCalls、1 次 stream）、`subagent_hold_no_running_children_skips_hold`（running=0 → `should_hold` false → hold skip → NoToolCalls、1 次 stream）、`subagent_hold_drains_batched_completions`（后台 50ms push 3 completion → `recv()` 取首 + `try_recv` drain 2 → 3 sentinel 消息注入 → resume → NoToolCalls、2 次 stream）。共 73 个 host_executor 测试通过（67 既有 + 6 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime`
（lib）零 warning；`cargo test -p codesmith-agent --lib` 79 通过；`cargo test -p codesmith-agent-runtime --lib
host_executor` 73 通过（67 既有 + 6 新 blocking-hold）；`cargo test -p codesmith-agent-runtime --lib` 1079
通过、0 失败、2 ignored；`cargo build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- **`HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役**：全部十个 in-loop guardrail 已吸收
  （compaction/capacity/approval/steer/transparent-retry/early-tool-start/subagent/LSP/loop-guard/cancel-token）；
  **阻塞 hold 已就位**（本轮吸收），闭合项就位后即可接入。wire-in 是下一个主聚焦。
- **wire-in 前置闭合项**：`ContextPatch` apply（tongten-only）、`<turn_meta>` enrichment、mid-stream buffer
  steer drain、`UnboundedReceiver` shape reconcile——随 wire-in 切片接入。
- **opt-in `CapacityController`**（Gate A + seam 4 post-tool checkpoint + error-escalation）：独立 opt-in
  切片，需完整 `CapacityController` 状态机，仍低优先。
- **compaction 闭合项**：summary-prompt merge / attachment reinject / post-compact cleanup / enhancements /
  working-set pins / `emit_session_updated` 随 wire-in 切片接入。同样适用于 capacity recovery 的 `ContextPatch` apply。
- **`ToolCallStarted` stream-time 合成 + bridge 去重**：需 `Callback::on_tool_start` 透传 wire id 或
  bridge 层 name+input pairing——与 wire-in 耦合，可同期接入。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-11 §E wire-in 机械前置项落地，UnboundedReceiver shape reconcile + TurnDispatchPlan.framework_tool_set，`feat/pluggable-framework-core`）：**

§E 的第十九个切片落地——wire-in 的两个自包含机械前置项。十个 in-loop guardrail 已全部吸收（slice 18 收尾），wire-in（`HostAgentExecutor` 接入 `handle_send_message` + 退役 `handle_deepseek_turn`）是下一个主聚焦。
用户决策"分阶段：先做机械前置项"——先把两个不碰 live path、不需 `Session` 可达的机械前置项落地，下一切片再做真正的 wire-in。本轮纯增量，零既有调用点行为改动；生产路径 `handle_deepseek_turn` 不受影响。

- **Part A — `UnboundedReceiver` shape reconcile**（`host_executor.rs` 单文件）：executor 的 `subagent` 字段从 bounded
  `mpsc::Receiver<SubAgentCompletion>` 改为 `mpsc::UnboundedReceiver<SubAgentCompletion>`，对齐生产
  `Engine.rx_subagent_completion`（`mod.rs:144`，unbounded）。这是 ROADMAP "wire-in 前置闭合项" 的 §d 项——wire-in 时
  `handle_send_message` 需把 `self.rx_subagent_completion` 直接交给 executor（bounded/unbounded shape 不匹配则需转换层）。
  - **edit sites（12 处）**：模块 doc prose（line 181）；字段声明（line 1088）；构造器 param（line 1133）；
    `subagent_channel()` 返回类型 `mpsc::Sender`→`mpsc::UnboundedSender` + `mpsc::Receiver`→`mpsc::UnboundedReceiver`
    （lines 4448-4449）；`subagent_channel()` body `mpsc::channel::<SubAgentCompletion>(64)`→`mpsc::unbounded_channel::<SubAgentCompletion>()`
    （line 4451）；7 处 test `send(...).await`→`send(...)`（lines 6939/6940/7068/7129/7391/7392/7393——`UnboundedSender::send`
    是同步的，`.await` 会编译错误）。
  - **不需改动**：`run_inner` 的 3 个用法点（2512 `try_recv`、2567 `recv().await`、2613 `try_recv`）——
    `UnboundedReceiver` 的 `try_recv`/`recv()` API 与 bounded 完全一致；71 个 test 构造器调用——类型推导自动适配
    （62 个 `None` 类型无关、9 个 `Some(rx_sub)` 从 `subagent_channel()` 推导）。
- **Part B — `TurnDispatchPlan.framework_tool_set` 字段**（2 文件）：给 `TurnDispatchPlan` 加
  `pub framework_tool_set: Option<Arc<codesmith_agent::tools::ToolSet>>` 字段，在 TUI `build_turn_dispatcher`
  里于 type-erase（`runtime_traits.rs:422` `Arc::new(r) as Arc<dyn ToolDispatcher>`）之前调
  `registry.to_framework_tool_set()` 填充。这是 wire-in 的关键前置——`handle_send_message` 需从 plan 拿到 framework
  `ToolSet` 喂 `HostAgentExecutor::new`（第二个构造器参数 `tools: Arc<ToolSet>`），但 `ToolDispatcher` trait-erase 后
  无法恢复 concrete `ToolRegistry`（无 `as_any` / 无 `ToolSet` accessor），故 `ToolSet` 必须在 erase 之前从 concrete
  type 派生。
  - **edit sites（2 处）**：`host_services.rs:589-596` `TurnDispatchPlan` 加字段 + doc comment；
    `runtime_traits.rs:421-424` erase 之前（~line 420）算 `let framework_tool_set = tool_registry.as_ref().map(|r|
    Arc::new(r.to_framework_tool_set()));`（`to_framework_tool_set(&self)` 借用非 move，与后续 `.map()` move 不冲突），
    加进 struct literal。
  - **不需改动**：consumer `handle_send_message`（`mod.rs:1165-1177`）纯字段访问、无解构；无 `HostServices` test mock
    需更新（workspace 内唯一 `impl HostServices` 是 `EngineHost`）；`TurnDispatchPlan` 不 derive `Default`，无
    `..Default::default()` 静默丢字段风险。`to_framework_tool_set()` 方法（`registry.rs:240`）已在 host_executor 14 处
    测试中端到端验证（含 `host_executor_drives_full_bridge_trio` 三桥组合证明），本轮只是首个**生产**调用点。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；`cargo +1.90.0 build -p codesmith-tui` 零新 warning
（143 warning 均既有死代码，与本轮无关）；`cargo test -p codesmith-agent-runtime --lib host_executor` 73 通过（含 9 个 subagent
测试用新 `UnboundedReceiver` 跑通——`try_recv` drain、`recv().await` hold、batched drain 三路径全覆盖）；`cargo test
-p codesmith-agent-runtime --lib` 1079 通过、0 失败、2 ignored；`cargo test -p codesmith-agent --lib` 79 通过；
`cargo build -p codesmith-agent-runtime --tests` 零新 warning（10 warning 均既有）；`cargo build --workspace` 全绿。
无新测试——UnboundedReceiver 改动由既有 9 个 subagent 测试作回归；`framework_tool_set` 字段在 wire-in 前无行为面。

**下一聚焦工作：**
- **`HostAgentExecutor` 接入 + `handle_deepseek_turn` 退役**（wire-in 主切片）：机械前置项已就位
  （`UnboundedReceiver` shape 对齐 + `TurnDispatchPlan.framework_tool_set` 可达）。构造 `HostAgentExecutor`（13 字段从
  Engine 状态映射）、在 `handle_send_message` 里路由到 `executor.run(&mut SessionChatHistory, user_text)`、map 返回
  `(StopReason → TurnOutcomeStatus)`、退役 `handle_deepseek_turn`（~2434 行）。已知 wire-in gap（ContextPatch apply /
  `<turn_meta>` enrichment / mid-stream buffer steer drain / compaction 闭合项 / `ToolCallStarted` stream-time / per-input
  approval）在 wire-in 切片内按优先级接入或显式 defer。
- **wire-in 前置闭合项（剩余两项）**：`ContextPatch` apply（tighten-only `auto_approve`/`trust_mode`，生产今天 hardcode
  `None` 故安全 no-op）、`<turn_meta>` enrichment（steer/LSP/subagent sentinel 消息无 `user_text_message_with_turn_metadata`
  包裹）、mid-stream buffer steer drain（`reduce_stream` 内 `try_recv`）——随 wire-in 切片接入。
- **opt-in `CapacityController`**（Gate A + seam 4 post-tool checkpoint + error-escalation）：独立 opt-in 切片，仍低优先。
- **compaction 闭合项**：summary-prompt merge / attachment reinject / post-compact cleanup / enhancements / working-set pins /
  `emit_session_updated` 随 wire-in 切片接入。同样适用于 capacity recovery + subagent 的 `ContextPatch` apply。
- **`ToolCallStarted` stream-time 合成 + bridge 去重**：需 `Callback::on_tool_start` 透传 wire id 或 bridge 层 name+input
  pairing——与 wire-in 耦合，可同期接入。
- **subagent 阻塞 hold 的 `UnboundedReceiver` shape**：已对齐（Part A）；wire-in 时 `self.rx_subagent_completion` 直接
  包进 `Arc<tokio::sync::Mutex<…>>` 交给 executor。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

**进度（2026-07-11 §E HostAgentExecutor wire-in + handle_deepseek_turn 退役，cutover 主切片，`feat/pluggable-framework-core`）：**

§E 的第二十个切片落地——wire-in cutover：`HostAgentExecutor` 接入 `handle_send_message` 成为 live production path，`handle_deepseek_turn`
（`turn_loop.rs`，~2400 行）整体退役。十个 in-loop guardrail 已全部吸收（slice 11–19），本轮是收口。用户决策"Priority wire-in"——接入
cheap/moderate 行为 gap（`emit_session_updated` / per-input approval），退役 legacy turn loop；expensive gap（`ToolCallStarted` stream-time /
post-compact cleanup / per-turn usage）显式 defer。cutover 单 commit，零既有调用点行为改动（生产路径语义不变：构造 executor → `drain_stale_steers` →
`run` → map `StopReason → TurnOutcomeStatus`，post-turn 逻辑 mod.rs:1183-1237 原样消费 tuple）。

- **Step 1 — Receiver-wrap（`Arc<tokio::sync::Mutex<…>>`）**（`mod.rs` Engine struct + `new_runtime` literal）：`rx_approval` /
  `rx_steer` / `rx_subagent_completion` 三个字段从 bare `mpsc::Receiver`/`UnboundedReceiver` 改为
  `Arc<tokio::sync::Mutex<…>>`，对齐 executor 既有字段类型。`new_runtime` struct literal 内 wrap
  （`Arc::new(AsyncMutex::new(rx_approval))` 等），tui `build_engine` 传 bare receiver（`new_runtime` 内部 wrap）。
  bare consumer `handle_deepseek_turn` 退役（删除耦合到 wrap）；stale-drain（`mod.rs:1023` `while self.rx_steer.try_recv().is_ok() {}`）
  移入 executor 的 `drain_stale_steers()`，在 `executor.run` 前调用。单 consumer：仅 executor 消费（legacy 路径退役）。
- **Step 1b — `HostServices::lsp()` → `Arc<dyn LspManagerApi>`**（`host_services.rs` trait + `runtime_traits.rs` impl）：
  executor 的 `LspProbe` 需 capture `Arc<dyn LspManagerApi>`（生命周期独立于 `&self Engine` 借用），故 `lsp()` 返回 `Arc`
  而非 `&dyn`，对齐 `bg_registry`/`subagents`/`shell` 的既有 shape。`EngineHost` 持 `Arc<LspManager>`，clone coerces。
- **Step 2 — `handle_send_message` 路由到 executor + StopReason map**（`mod.rs` ~1181-1190）：替换 `handle_deepseek_turn`
  调用为：构造 `HostAgentExecutor`（14 字段从 Engine/plan/Session 映射——`client`/`tools`/`callback`/`config`/`event_tx`/
  `lsp`/`steer`/`approval`/`compaction`/`capacity`/`subagent`/`cancel_token`/`subagent_api` + `with_tool_dispatcher`）→
  `drain_stale_steers().await` → `let mut history = SessionChatHistory::new_with_event_tx(&mut self.session, …);` →
  `executor.run(&mut history, String::new()).await` → map `StopReason`（`NoToolCalls`/`MaxSteps`→`Completed`、`Interrupted`→
  `Interrupted`、`Error(msg)`→`Failed(Some(msg))`、`Err(e)`→`Failed(Some(e.to_string()))`）。`drop(history)` 释 `&mut self.session`
  借用再进 post-turn 逻辑。**Host 预推 enriched 初始 user message 不变**（mod.rs:1084-1097：working_set observe +
  `user_text_message_with_turn_metadata_for_route` push），executor 以 `user_text: String::new()` 调用、seed push 由
  `if !user_text.is_empty()` 守卫（空⇒不 seed，host 已 seed；非空⇒73 个 executor test 不变）。`force_update_plan_first` →
  `_force_update_plan_first`（plan-force-on-final-step 特性 defer）。
- **Step 3 — `emit_session_updated` via `SessionChatHistory`**（`session_history.rs`）：加 `event_tx: Option<mpsc::Sender<Event>>`
  字段；保留 `pub fn new(session)`（`event_tx: None`——73 个 executor test 不变）+ 新增 `pub fn new_with_event_tx(session, event_tx)`。
  `push` 内 `session.add_message` 后 `try_send(Event::SessionUpdated{…})`（sync——`ChatHistory::push` 是 sync 不能 `.await`；
  drop-on-full 可接受——post-turn `TurnComplete` 携带终态 transcript + mod.rs:1133 pre-turn `emit_session_updated` 保底刷新）。
  覆盖 executor 的全部 push：steer / LSP flush / subagent sentinel / tool-result / assistant。
- **Step 4 — Per-input approval via `tool_dispatcher`**（`host_executor.rs`）：加第 14 字段
  `tool_dispatcher: Option<Arc<dyn ToolDispatcher>>`（构造器 Self literal 默认 `None`——71 个 test 调用点不动）+
  `with_tool_dispatcher(…)` builder（避免第 14 positional param 破 71 处 test）。`request_approval` 在静态
  `requires_approval(&tool.capabilities())` gate 前先 consult `tool_dispatcher.approval_requirement_for(name, input)`——
  `Some(req)` 用 `req != ApprovalRequirement::Auto`，`None` 回退静态 gate（镜像 turn_loop.rs:1704-1706 的 per-input override）。
- **Step 6 — 删除 `handle_deepseek_turn` + 私有 helper**：`turn_loop.rs` 3374→83 行（删 `handle_deepseek_turn`
  239-2671 + ~22 私有 helper + 22/23 test）。保留 `subagent_completion_runtime_message`（executor host_executor.rs:579 引用）+
  其 test + `messages_with_turn_metadata`（tui test 引用）+ `EarlyToolResult`/`EarlyToolTask`（dispatch.rs:61 type 引用）。
  `approval.rs` 173→45 行（删 `cancel_reason_suffix`/`await_tool_approval`/`await_user_input` 三个 orphan impl-Engine 方法 +
  整个 `impl Engine {}` block；保留 `ApprovalDecision`/`UserInputDecision` 跨 crate pub-reexport + use `UserInputResponse`）。
  删 genuinely-orphan `ApprovalResult` enum + `CancelReason::describe` 方法（cargo-fix 删了 14 个 unused import——handle_deepseek_turn
  的 sole-consumer imports）。
- **Warning cleanup**（17 个 dead-code 全是 handle_deepseek_turn 孤儿）：module-level `#![allow(dead_code)]` 于
  `streaming.rs`（stream-reducer config cluster：`*_STREAM_CHUNK_TIMEOUT_SECS`/`stream_chunk_timeout_secs`/`ContentBlockKind`/
  `STREAM_MAX_*`——executor `reduce_stream` 自带 config，scrubber/retry-policy 仍 live）+ `turn_loop.rs`（residual
  `EarlyToolResult`/`EarlyToolTask` 字段 unread——dispatch.rs 仅 type 引用未构造，待 follow-up re-wire speculative dispatch）；
  per-item `#[allow(dead_code)]` 于 `mod.rs` 四个 superseded impl-Engine 方法（`kod_prefetch_spawn`/`kod_prefetch_collect`/
  `recover_context_overflow`/`layered_context_checkpoint`——executor 有对应 probe，待 re-wire Kod prefetch）+ 三字段
 （`rx_user_input` write-only post-retire / `tool_exec_lock` executor 自串行 / `knowledge_prefetch` 待 re-wire）+
  `emit_tool_audit`（env-gated 审计钩子）+ `mcp_tool_approval_description`（待 re-wire 进 CallbackBridge）。均带 doc 注明
  "deferred deletion / re-wire"，非永久 allow。

**Deferred gaps（本轮显式 defer，附理由）：**
- **per-turn usage tracking**：executor 的 `reduce_stream` 解构 `MessageDelta { stop_reason: sr, .. }`——`..` 丢 `usage`；
  生产原经 `turn.add_usage` 累加，token 计数器停转。需在 `reduce_stream` 透传 usage 或 `Callback::on_llm_end` 携带。
- **`<turn_meta>` enrichment for steer/LSP/subagent sentinel**：executor 经 `&mut dyn ChatHistory` 在 run 内构建这些消息时
  `&mut self.session` 已借用，host 端 `&self Engine` callback 读 live `working_set`/`config` 无法 capture 而不冲突借用。
  初始 user message 保留 turn_meta（host mod.rs:1084-1097 push 不变）。follow-up 重设计 seam（Arc-shared working_set 或
  post-stream host callback 从 buffered steer text 构建 enriched message）。
- **working_set `observe_user_message` for steer**：同 turn_meta borrow-conflict。初始 user message observe 保留。
- **`ToolCallStarted` stream-time 合成**：需 `Callback::on_tool_start` 透传 wire tool id（CORE trait change）或
  `CallbackBridge` name+input pairing；executor 当前在 execute-time 合成 `bridge-{n}` id。
- **post-compact cleanup**（`merge_compaction_summary` / `reinject_compaction_attachments` / `post_compact_cleanup`）：
  需 Session system-prompt 可变 + host-coupled attachment/working-set 可达，`ChatHistory` seam 够不到；
  summary-prompt 当前 compute-and-discard。
- **`ContextPatch` apply**：今天 no-op（dispatch 在 11 处 hardcode `None`），安全 defer。
- **mid-stream steer drain**（`reduce_stream` 内 `try_recv` + `run_inner` flush）：deferred——其 push 是 plain user message
  （turn_meta 已 defer），边际价值低；stale-drain 已在 `drain_stale_steers` 覆盖 pre-turn 清空。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool checkpoint + error-escalation）：独立 opt-in 切片，仍低优先。
- **per-input-approval 专用测试**：Step 4 impl 已落地（`request_approval` per-input consult 路径在 73 个 test 的 build path
  验证无回归），专用 override-downgrade 断言 test 待补。

**验证：** `cargo +1.90.0 build -p codesmith-agent` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime` 零新 warning
（17 dead-code 全清理——module-level/per-item `#[allow]` + 删 `ApprovalResult`/`CancelReason::describe` + cargo-fix 删 14 unused
import）；`cargo +1.90.0 build -p codesmith-tui` 零新 warning（143 均既有死代码，`lsp()→Arc` + `build_engine` 不改——
`new_runtime` 内部 wrap）；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过；`cargo +1.90.0 test -p codesmith-agent-runtime
--lib host_executor` 73 通过（含 9 个 subagent test、approval test——per-input consult 路径无回归）；`cargo +1.90.0 test
-p codesmith-agent-runtime --lib` 1055 通过 + 1 flaky `mcp::streamable_http_stale_session_reconnects_and_retries_tool_call`
（isolated rerun pass——已知网络/timing flaky，与本轮无关）；`cargo +1.90.0 build --workspace` 全绿。无新 test——Step 4 per-input
override 专用 test 待补、Step 5 mid-stream-steer test 随 defer 略。

**下一聚焦工作：**
- **deferred-gaps cleanup 切片**：最高价值是 `<turn_meta>` enrichment 的 seam 重设计（Arc-shared working_set 或 post-stream
  host callback）——解锁 steer/LSP/subagent 的 turn_meta + working_set observe + mid-stream steer drain（三 gap 同根）。
- **per-turn usage tracking**：`reduce_stream` 透传 `MessageDelta.usage` 或 `Callback::on_llm_end` 携带——token 计数器修复。
- **`ToolCallStarted` stream-time + bridge 去重**：`Callback::on_tool_start` 透传 wire id 或 bridge name+input pairing。
- **post-compact cleanup**：`Session` system-prompt mutability + host-coupled attachment/working-set 可达（summary-prompt 当前丢弃）。
- **per-input-approval 专用 test**：override-downgrade 断言（ExecutesCode 工具 + dispatcher 返 Auto ⇒ 不 approval）。
- **dead-code deletion 切片**：把本轮 `#[allow(dead_code)]` 的 17 项中真正 orphan 的删掉（streaming config cluster /
  `mcp_tool_approval_description` / `emit_tool_audit`），superseded 方法按 re-wire 决策保留或删。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：独立 opt-in 切片，仍低优先。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

**进度（2026-07-12 §E per-turn usage tracking（token 计数器修复），slice 21，`feat/pluggable-framework-core`）：**

§E 的第二十一个切片落地——修复 wire-in cutover（slice 20）丢的 token-usage 采集。cutover 后 `reduce_stream` 把 `MessageStart` 当 no-op（丢 `message.usage`）、`MessageDelta` 解构 `..` 丢 `usage: Option<Usage>`，致 `turn.usage` 恒零、`session.total_usage` 不累加（mod.rs:1306 add 空值）、`Event::TurnComplete.usage` 恒空（mod.rs:1313）——token 计数器停转。本轮按"不动核心 trait"约束，在 executor 内部用 interior-mutability 字段复刻退役 `handle_deepseek_turn` 的 usage 语义（经 `git show 42123572~1:turn_loop.rs` 核实）。零既有调用点行为改动（73 个 test 调用点不动——usage 字段在 `new()` Self literal 默认，镜像 slice 20 的 `tool_dispatcher: None` 模式）。

- **Step 1 — usage 字段 + 采集 helper**（`host_executor.rs`）：加 `usage: std::sync::Mutex<Usage>` 字段（`std::sync` 非 tokio——累加是 sync 字段算术，锁不跨 `await`，镜像 LSP/steer/compaction 先例），`new()` Self literal 内 `usage: std::sync::Mutex::new(Usage::default())` 默认（无新构造器 param——73 test 不动）。加 `take_usage(&self) -> Usage`（lock+clone，executor 每轮 fresh 构造故无跨轮泄漏）+ `accumulate_usage(&self, &Usage)`（`input`/`output` saturating_add，`prompt_cache_hit`/`miss`/`reasoning_tokens` 经 `add_optional_usage`——镜像 `TurnContext::add_usage` 同 5 字段；`reasoning_replay_tokens`/`server_tool_use` 不累加，镜像 `add_usage` 亦不碰——faithful）。模块级 free fn `add_optional_usage`（`turn.rs` 同名 fn private 不可达，duplicate 注 "lift later"，同 `approval_intent_summary`/`block_tool_result` class）。
- **Step 2 — `reduce_stream` 采集**（`host_executor.rs`）：local `let mut usage = Usage { input_tokens: 0, ..default };`。`MessageStart` arm `{ message } => { usage = message.usage; }`（REPLACE——镜像 `turn_loop.rs:838`）。`MessageDelta` arm bind `usage: delta_usage`，`if let Some(u) = delta_usage { usage = u; }`（REPLACE——latest cumulative wins，镜像 `turn_loop.rs:1137-1141`）。`StreamReduceOutcome`：`Complete` + `Partial` 加 `usage: Usage` 字段；`Empty` 不带（Empty→retry 丢 usage，镜像生产 `continue` before `turn.add_usage`——by-design divergence：失败轮的 partial MessageStart usage 丢，`total_usage` 仅反映成功 step）。
- **Step 3 — 透传 `StreamRoundOutcome::Content` + `run_inner` 累加**（`host_executor.rs`）：`StreamRoundOutcome::Content` 加 `usage: Usage`；`stream_with_transparent_retry` 从 `Complete`/`Partial` destructure `usage` 透传进 `Content { content, stop_reason, usage }`。`run_inner` Content arm destructure `usage` → `self.accumulate_usage(&usage);`（跨 step ADD——镜像 `turn_loop.rs:1193` `turn.add_usage`）。
- **Step 4 — host 回读**（`mod.rs handle_send_message`）：`let turn =` → `let mut turn =`（可赋值 `turn.usage`）。`drop(history);` 后、`self.session.total_usage.add(&turn.usage)` 前插 `turn.usage = executor.take_usage();`——流入既有 `total_usage.add`（mod.rs:1306）+ `Event::TurnComplete { usage: turn.usage }`（mod.rs:1313），无其他 post-turn 改动。early-failure 路径（mod.rs:1093）不构造 executor，`turn.usage` 恒零（正确——无 LLM 跑）。
- **Tests**（`host_executor.rs #[cfg(test)]`）：helpers `message_start_with_usage(input)` / `finish_with_usage(stop, input, output)`（cumulative——MessageDelta usage 在生产是 cumulative，REPLACE 覆盖整 `Usage`，故 delta 重发 input）/ `usage_round(input, output)`。5 个新 test：(1) `usage_captures_message_start_and_delta_within_a_stream`（单流 MessageStart{in:100}+text+Delta{cumulative in:100,out:50} → take_usage in:100/out:50）；(2) `usage_accumulates_across_multiple_steps`（echo tool roundtrip 两流 in:100/out:50 + in:120/out:30 → in:220/out:80，跨 step ADD）；(3) `usage_replaces_within_stream_keeps_latest_delta`（单流两 Delta usage out:30→70 → out:70，within-stream REPLACE）；(4) `usage_none_on_clean_stream_is_zero`（`end_call()` 无 MessageStart、usage:None → 零，legacy event shape 无回归）；(5) `usage_empty_retry_drops_failed_attempt_usage`（MessageStart{in:100} then mid-flight Err（Empty）then clean round in:200/out:60 → take_usage 仅计 clean round，failed attempt 的 100 丢——验证"thread through Content not Empty"+transparent-retry 交互）。

**By-design divergences（附理由）：**
- **Empty→budget-exhausted→Err**：生产在失败流也累加 partial（MessageStart）usage；executor 返 `Err`（轮失败）丢之。失败轮的 minor divergence——`total_usage` 仅反映成功 step。
- **`reasoning_replay_tokens`/`server_tool_use`**：`TurnContext::add_usage` 亦不累加（生产 `turn.usage` 从不带），`accumulate_usage` 镜像同 5 字段保持 faithful。
- **`take_usage` read-once**：host 每轮后读一次；executor 每轮 fresh 构造故无跨轮泄漏。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零新 warning；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 78 通过（73 既有 + 5 新）；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（未改——不动核心 trait）；`cargo +1.90.0 build --workspace` 全绿（tui 143 均既有死代码）。

**下一聚焦工作：**
- **deferred-gaps cleanup 切片**：最高价值是 `<turn_meta>` enrichment 的 seam 重设计（Arc-shared working_set 或 post-stream host callback）——解锁 steer/LSP/subagent 的 turn_meta + working_set observe + mid-stream steer drain（三 gap 同根）。
- **`ToolCallStarted` stream-time + bridge 去重**：`Callback::on_tool_start` 透传 wire id 或 bridge name+input pairing。
- **post-compact cleanup**：`Session` system-prompt mutability + host-coupled attachment/working-set 可达（summary-prompt 当前丢弃）。
- **per-input-approval 专用 test**：override-downgrade 断言（ExecutesCode 工具 + dispatcher 返 Auto ⇒ 不 approval）。
- **dead-code deletion 切片**：把 slice 20 `#[allow(dead_code)]` 的 17 项中真正 orphan 的删掉（streaming config cluster / `mcp_tool_approval_description` / `emit_tool_audit`），superseded 方法按 re-wire 决策保留或删。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：独立 opt-in 切片，仍低优先。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

**进度（2026-07-12 §E `<turn_meta>` enrichment seam 落地，Arc-shared WorkingSet + TurnMetaProbe，slice 22，`feat/pluggable-framework-core`）：**

§E 的第二十二个切片落地——闭合 steer/LSP push 在 `executor.run` 期间的 `<turn_meta>` 富化 + `working_set.observe_user_message` 缺口。根因：`executor.run(&mut SessionChatHistory)` 期间 `&mut self.session` 被 `SessionChatHistory` 借走（mod.rs:1187），host 无法触达 live `working_set`/`config` 富化 mid-run push 的 steer/LSP 消息（subagent sentinel 本就 plain，匹配生产）。本轮按"全 Arc-shared"方案（用户批准）：把 `Session.working_set` 从 `WorkingSet` 改为 `Arc<std::sync::Mutex<WorkingSet>>`（既有 probe 模式——LspProbe/CompactionProbe/CapacityProbe），host 在 executor 构造时（borrow 前）clone Arc 进新 `TurnMetaProbe`，故 probe 在 run 期间仍能 observe + 构建 `<turn_meta>`。零既有 test 行为改动（`with_turn_meta` 默认 `None`，78 个既有 test 调用点不动——镜像 slice 20/21 的 `tool_dispatcher`/`usage` 模式）。经 `git show 42123572~1:turn_loop.rs` 核实退役 `handle_deepseek_turn` 的 push 语义（steer observe+enrich / LSP enrich-only / subagent plain）。

- **Step 1 — WorkingSet → Arc<std::sync::Mutex>**（`session.rs` + ~12 访问点）：`pub working_set: WorkingSet` → `pub working_set: Arc<std::sync::Mutex<WorkingSet>>`（`WorkingSet` `Send+Sync`，纯 `HashMap`/`u64` 字段）。机械访问点全改 `…working_set.lock().expect("working_set poisoned").<method>()`：`engine/mod.rs`（observe_site/compaction_pins/paths/pinned/turn_meta wrappers）、`capacity_flow.rs`（4 处 top_paths/pinned）、`post_compact_cleanup.rs`（force_rebuild）、`session.rs rebuild_working_set`、`tui/tests.rs`（7 处 observe_user_message）。
- **Step 2 — `&'a WorkingSet` borrow API 局部化**（保持 API 稳定）：`StructuredStateRequest.working_set: &'a WorkingSet` + `StructuredState::capture(working_set: &WorkingSet)` **不变**——cycle path 的 post-turn async 场景持有 std `MutexGuard` 跨 `.await` 非法（guard 非 Send）。3 处传 `&session.working_set` 的点改 clone-and-local：`engine/mod.rs` 2 处 + `tui/runtime_traits.rs:254` → `let ws = …lock()…clone(); … &ws …`（clone 是 point-in-time 快照，cycle restart 罕见故 HashMap clone 可忽略）。
- **Step 3 — turn_meta free fn 提取**（`engine/turn_meta.rs`，dedup）：把 `Engine` 的 `turn_metadata_block` / `conditional_skills_block` / `user_text_message_with_turn_metadata(_for_route)` method body 提为 `pub(crate)` free fn（显式参数，不读 `self`）。`Engine` 对应 method 变 thin wrapper（lock working_set + 转发），单一 source 给 Engine wrapper + 新 `TurnMetaProbe` 共用——"lift now" 而非再添一个 `approval_intent_summary`/`block_tool_result`-class 重复。
- **Step 4 — `TurnMetaProbe`**（`host_executor.rs`，镜像 `CompactionProbe`）：`pub struct TurnMetaProbe { working_set: Arc<StdMutex<WorkingSet>>, workspace, skills_dir, model, auto_model, reasoning_effort, reasoning_effort_auto }` + `new()`。`observe_user_message(&self, text)`（lock + observe，sync——生产 steer observe）+ `enrich_user_text_message(&self, text) -> Message`（lock + 调 `turn_meta::user_text_message_with_turn_metadata`）。`std::sync::Mutex`（sync read，不跨 `await`；conditional_skills fs walk 亦 sync，镜像 LSP/compaction probe 先例）。`Send+Sync`。
- **Step 5 — executor 接线**（`host_executor.rs`）：加 `turn_meta: Option<TurnMetaProbe>` 字段（`new()` 默认 `None`）+ `with_turn_meta(Option<TurnMetaProbe>)` builder。rewire 3 个 push 点（subagent sentinel `:2891` **不变**）：LSP flush（enrich-only，无 observe——diagnostics 无 path token）；steer pre-request drain（observe+enrich）；steer blocking-hold arm（observe+enrich）。三处 `match &self.turn_meta { Some(p) => …, None => plain }`——`None` 走纯文本保 pre-slice-22 行为。更新 module doc gap 注释（LSP/subagent）标记 `<turn_meta>` 富化已落地。
- **Step 6 — wire-in**（`mod.rs handle_send_message` ~1163，`with_tool_dispatcher` 前）：`SessionChatHistory::new` borrow 前构造 `TurnMetaProbe::new(Arc::clone(&self.session.working_set), …model 字段 snapshot from self.session.*，set at mod.rs:1043-1059)`，`.with_turn_meta(Some(turn_meta_probe))` 接 builder chain。model 字段镜像生产 session-model route 变体。
- **Step 7 — Tests**（`host_executor.rs #[cfg(test)]`）：5 个新 test：(1) `turn_meta_probe_enrich_wraps_text_and_observe_increments_turn`（probe 单元——enrich 出 2-block user message[<turn_meta> Text, body Text]；observe 增 `working_set.turn`）；(2) `steer_drain_enriches_with_turn_meta_and_observes`（steer push → `<turn_meta>` block + turn 增；seed push 保持 plain 1-block）；(3) `lsp_flush_enriches_with_turn_meta`（LSP push → `<turn_meta>`+`<diagnostics` 2-block，turn 不增——enrich-only）；(4) `subagent_sentinel_stays_plain_under_turn_meta`（回归 guard——sentinel 仍 1-block 纯文本，无 `<turn_meta>`）；(5) `turn_meta_reflects_working_set_summary`（seed path 进 WorkingSet → steer 的 `<turn_meta>` 含 "Repo Working Set" + "src/lib.rs"）。helper `turn_meta_probe(&Session)`（镜像生产 wire-in）。既有 78 test 不动。

**By-design gaps（延后）：**
- **mid-stream steer buffer drain**（`reduce_stream` `try_recv` + flush）——独立 reduce_stream 切片；pre-request drain + blocking-hold steer arm 现已富化。
- **`<turn_meta>` for compaction re-inject messages**——独立（compaction closure：summary-prompt merge / attachment reinject / post-compact cleanup）。
- **`ContextPatch` apply**——不变（仍 no-op；生产硬编码 `None`）。
- **conditional_skills fs walk 持锁**——run 期间单消费者无争用，镜像生产 per-`turn_meta` fs-walk 成本；perf 关注时可拆 extract-then-discover。

**验证：** `cargo +1.90.0 build -p codesmith-agent`（未改，零 warning）；`cargo +1.90.0 build -p codesmith-agent-runtime`（零新 warning）；`cargo +1.90.0 build -p codesmith-tui`（零新 warning——Arc 字段 + 3 clone+local + 7 test lock 编译，143 均既有死代码）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 83 通过（78 既有 + 5 新）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1066 全绿；`cargo +1.90.0 test -p codesmith-tui` 2921 全绿；`cargo +1.90.0 build --workspace` 全绿。另清掉 `post_compact_cleanup.rs` 测试模块一 stale `use WorkingSet` 既有 warning。

**下一聚焦工作：**
- **mid-stream steer buffer drain**（`reduce_stream` 的 `try_recv` + flush + enrich）：现 steer 仅 pre-request drain + blocking-hold arm 富化，streaming 期间到达的 steer 仍待 mid-stream 切片。
- **`<turn_meta>` for compaction re-inject**：compaction closure items（summary-prompt merge / attachment reinject / post-compact cleanup）的 turn_meta 富化。
- **`ToolCallStarted` stream-time + bridge 去重**：`Callback::on_tool_start` 透传 wire id 或 bridge name+input pairing。
- **post-compact cleanup**：`Session` system-prompt mutability + host-coupled attachment/working-set 可达（summary-prompt 当前丢弃）。
- **per-input-approval 专用 test**：override-downgrade 断言（ExecutesCode 工具 + dispatcher 返 Auto ⇒ 不 approval）。
- **dead-code deletion 切片**：slice 20 `#[allow(dead_code)]` 17 项中 orphan 的删掉。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：独立 opt-in 切片，仍低优先。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-13 §E mid-stream steer buffer drain 落地，闭合最后一个 steer 次级排空点，`feat/pluggable-framework-core`）：**

§E 的第二十三个切片落地——闭合 steer 的最后一个次级排空点：流式期间到达的 steer 现在由 `reduce_stream` 的 `try_recv` 捕获进 per-step `pending_steers` 缓冲，并在两个位置 flush（post-stream no-tools resume + post-tool-execution），镜像退役 `handle_deepseek_turn` 的 `pending_steers` 机制（`turn_loop.rs:683` 声明、`:721-731` 流内 `try_recv` + "queued" status、`:1297-1307` post-stream flush + resume、`:2632-2637` post-tool flush）。此前 steer 仅在 pre-request drain + blocking-hold arm 被处理——流式期间到达的 steer 要么等下一步 pre-request drain（若 turn 继续），要么被下一 turn 的 `drain_stale_steers` **丢弃**（若 turn 以 NoToolCalls 结束且无子 agent 运行）。本轮闭合该正确性缺口。本轮纯增量（`host_executor.rs` 一个文件 + 文档），零既有调用点行为改动；生产路径不受影响。

- **`reduce_stream` 缓冲**（seam 2 内）：新增 `pending_steers: &mut Vec<String>` 参数（镜像 `early_tasks`）。在 `any_content_received` flip 之后、`match event` 之前，`try_recv` 循环排空 `self.steer`（lock tokio mutex → `try_recv` → trim → skip empty → `pending_steers.push` → `emit_status("Steer input queued: {summarize}")`）。guard 在 `{ }` 块内取/放，不跨 `emit_status().await`（匹配 `drain_steers` 先例）。每个 stream event 后都跑一次——`try_recv` 非阻塞、通常空，开销可忽略。
- **`stream_with_transparent_retry` 透传**：新增 `pending_steers` 参数，透传给 `reduce_stream`。透明重试（Empty → retry）复用同一 `&mut` 引用，故失败流的缓冲 steer 保留（匹配生产 `pending_steers` 声明在 stream loop 之前）。`RecoveredContextOverflow` → `continue` 时 `pending_steers` 重新声明（per-step），但此时流未开起故缓冲为空——丢弃正确。`Interrupted` → return 时 `pending_steers` drop——取消的 turn 的 steer 丢弃正确（下一 turn 的 stale drain 也会丢）。
- **`run_inner` 两个 flush 站点**：
  - **Post-stream no-tools**（`tool_uses.is_empty()` 臂顶部、subagent drain 之前）：`flush_pending_steers` 返 count > 0 则 `step += 1; continue`（resume，镜像 `turn_loop.rs:1297-1307`）。生产顺序：pending_steers 先于 subagent drain。
  - **Post-tool-execution**（`early_tasks.clear()` 之后、Checkpoint G cancel 检查之前）：`flush_pending_steers`（镜像 `turn_loop.rs:2632-2637`）。无 `continue`——fall through 到下一步。闭合 "last step before MaxSteps" 的 stale-drain 丢失缺口。
- **helper 提取（dedup 3 站点 → 1）**：`push_steer_message(&self, steer: String, history)` —— observe + enrich（if `TurnMetaProbe`）or plain text + `history.push`。sync（`ChatHistory::push` sync）。`flush_pending_steers(&self, pending, history) -> usize`——`drain(..)` + `push_steer_message` per steer，返 count。refactor `drain_steers` + blocking-hold steer arm 用 `push_steer_message`（零行为变化）。三个 steer push 站点（pre-request drain / blocking-hold arm / mid-stream flush）共享单一源，不可漂移。
- **module 文档**：line 73-77 "mid-stream buffer … deferred" → "absorbed ✅"；line 547-552 同；`drain_steers` doc + seam-2 注释更新。
- **test doubles**：`MockLlm` 新增 `steer_on_stream: Mutex<Option<(mpsc::Sender<String>, String)>>` 字段 + `with_steer_on_stream(tx, text)` 方法（镜像 `with_cancel_on_stream`：在 `create_message_stream` 的 async block 内 `tx.try_send(text)`——在 pre-request `drain_steers` 跑完后才推，故 steer 不被 pre-request drain 拦截，而是被 `reduce_stream` 的首个 `try_recv` 捕获）。`with_rounds` 初始化新字段。
- **5 个新测试**：`mid_stream_steer_buffered_and_flushed_on_no_tool_calls`（2-round mock + steer-on-open → 缓冲 → post-stream flush → resume → request 2 含 steer、transcript len=4）、`mid_stream_steer_buffered_and_flushed_after_tool_execution`（echo tool roundtrip + steer-on-open → post-tool flush → request 2 含 steer、transcript len=5）、`mid_stream_steer_emits_queued_status`（event channel 收 "Steer input queued:" 而非 "accepted:"——证明 mid-stream steer 不经 pre-request drain）、`mid_stream_steer_enriched_with_turn_meta`（`TurnMetaProbe` → flushed steer 是 2-block `<turn_meta>` + text、working_set turn 增）、`mid_stream_steer_empty_skipped`（空白 steer-on-open → 不缓冲、transcript len=2）。共 88 个 host_executor 测试通过（83 既有 + 5 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent` 零 warning（未改）；`cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 零新 warning（9 均既有）；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 88 通过（83 既有 + 5 新 mid-stream-steer）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1070 通过 + 1 flaky `mcp::streamable_http_stale_session_reconnects_and_retries_tool_call`（隔离重跑通过，既有偶发，与本轮无关）= 1071 总（原 1066 +5）；`cargo +1.90.0 build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- **`<turn_meta>` for compaction re-inject**：compaction closure items（summary-prompt merge / attachment reinject / post-compact cleanup）的 turn_meta 富化。
- **`ToolCallStarted` stream-time + bridge 去重**：`Callback::on_tool_start` 透传 wire id 或 bridge name+input pairing。
- **post-compact cleanup**：`Session` system-prompt mutability + host-coupled attachment/working-set 可达（summary-prompt 当前丢弃）。
- **per-input-approval 专用 test**：override-downgrade 断言（ExecutesCode 工具 + dispatcher 返 Auto ⇒ 不 approval）。
- **dead-code deletion 切片**：slice 20 `#[allow(dead_code)]` 17 项中 orphan 的删掉。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：独立 opt-in 切片，仍低优先。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-13 §E `<turn_meta>` for compaction re-inject 落地，富化宿主侧 compaction 附件重注入消息，`feat/pluggable-framework-core`）：**

§E 的第二十四个切片落地——将 slice 22 建立的 `<turn_meta>` 富化 seam 扩展到 compaction 附件重注入消息。宿主侧 `Engine::reinject_compaction_attachments`（`agent-runtime/src/engine/mod.rs`）在 host-triggered compaction（manual `mod.rs:1361` / capacity `capacity_flow.rs:514,541`）后推送 plan/todos/subagents/read_files 恢复消息，此前仅以 `<system-reminder>` 包裹——**无 `<turn_meta>`**。compaction 卷起 transcript 后，model 直到下一 user message 才重新获得 current-state 信号（working-set summary / conditional skills / date / model route）。本轮为每个重注入候选前置 `<turn_meta>` block，使 model 在 compaction 后立即获得 orientation，与 user/steer/LSP 消息的 `[turn_meta, content]` shape 一致。本轮纯增量（`agent-runtime/src/engine/mod.rs` + `engine/turn_meta.rs` 文档 + `tui/src/core/engine/tests.rs` 测试），enrich-only（无 observe——匹配 slice 22 LSP-flush 先例：重注入消息是合成 system-reminder、非 user intent）；零既有调用点行为改动；3 个既有 reinject 测试不变即通过。

- **helper 提取（dedup）**：`turn_metadata_content_block_for_route(&self, routed_model, auto_model, reasoning_effort, reasoning_effort_auto) -> ContentBlock`——lock `working_set` + 调 `turn_meta::turn_metadata_block`，单一 block 源。refactor `user_text_message_with_turn_metadata_for_route`（`mod.rs:910`）用它建 block（原 block 构造 bury 在 free fn `user_text_message_with_turn_metadata` 内）。新增 `enrich_reinject_message(&self, mut msg) -> Message`——前置 `turn_metadata_content_block_for_route(...)`（用 session 当前 model route），返 enriched msg。镜像 slice 22/23 `push_steer_message` enrich-then-push 模式。
- **`reinject_compaction_attachments` 接线**：在 `for candidate in candidates` 循环顶部 `let candidate = self.enrich_reinject_message(candidate);`——单一接线点，enrich 所有候选（plan/todos/subagents/read_files），使 dedup 检查、budget trial、push 均见 enriched 候选。
- **dedup 安全性（验证）**：`<turn_meta>` block 字节稳定（`summary_block` 字节稳定；date + model route 在 session 内稳定；enrich-only 故两次连续调用间 working_set 不变）→ `message == &candidate` dedup 仍匹配 → 第二次 reinject 注入 0（既有 dedup 测试 `tests.rs:1483-1485` 不变通过；新增 `dedup_still_works_with_enrichment` 回归守卫）。
- **budget 安全性（验证）**：enrichment 使候选更大 → budget trial `Some(before.saturating_sub(1))` 仍超预算 → 仍跳过 → 既有 budget 测试 `tests.rs:1546` 不变通过。
- **module 文档**：`turn_meta.rs` line 3-4 列表加 "compaction attachment re-inject"；`reinject_compaction_attachments` 新增 doc 注释（enrich-only / `[turn_meta, content]` shape / LSP-flush 先例 / dedup + budget 行为）。
- **3 个新测试**（`tui/src/core/engine/tests.rs`，扩 reinject 组）：`reinject_compaction_attachments_prepends_turn_meta_block`（plan+todos+read_files → 3 消息各 ≥2-block：首 `<turn_meta>`、次 `<system-reminder>`）、`reinject_compaction_attachments_turn_meta_reflects_working_set`（tempdir + `observe_user_message("src/lib.rs")` → reinject 的 turn_meta block 含 `## Repo Working Set` marker，证明 working-set snapshot 在 enrich-time 重读）、`reinject_compaction_attachments_dedup_still_works_with_enrichment`（两次连续 reinject → 第二次 0、`messages.len()` 不变）。共 6 个 reinject 测试通过（3 既有 + 3 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；`cargo +1.90.0 build -p codesmith-tui` 143 既有 warning、零新；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui reinject_compaction_attachments` 6 通过（3 既有 + 3 新）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1070 通过 + 1 flaky `mcp::legacy_sse_closed_stream_reconnects_and_retries_tool_call`（隔离重跑通过，既有偶发，与本轮无关）= 1071 总（不变——host_executor 未触）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui` 2848 全绿；`cargo +1.90.0 build --workspace` 全绿（tui 143 warning 均既有死代码）。

**下一聚焦工作：**
- **post-compact cleanup**（framework-core compaction closure）：`HostAgentExecutor::run_compaction` / `recover_context_overflow` 的 "summary_prompt discarded" 缺口——需 `ChatHistory` system-prompt seam + 宿主侧 attachment/working-set 可达（summary-prompt merge / attachment reinject / post_compact_cleanup 三项当前在 framework-core 路径全丢弃）。独立大切片，本轮 `<turn_meta>` 富化的宿主侧对应物。
- **`ToolCallStarted` stream-time + bridge 去重**：`Callback::on_tool_start` 透传 wire id 或 bridge name+input pairing。
- **per-input-approval 专用 test**：override-downgrade 断言（ExecutesCode 工具 + dispatcher 返 Auto ⇒ 不 approval）。
- **dead-code deletion 切片**：slice 20 `#[allow(dead_code)]` 17 项中 orphan 的删掉。
- **observe-and-repopulate-working-set-from-reinject**：本轮 enrich-only（匹配 LSP 先例），read_files 重注入携带真实路径但 working-set 不从中重填——未来可考虑 observe 路径（post-compact cleanup 切片内）。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：独立 opt-in 切片，仍低优先。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-13 §E compaction `summary_prompt` merge（deferred post-`run`）落地，关闭 framework-core compaction closure 第一项，`feat/pluggable-framework-core`）：**

§E 的第二十五个切片（item #3 "post-compact cleanup" 的子切片 25a）落地——关闭 "Known gaps in compaction" 中最显式命名的缺口：`HostAgentExecutor::run_compaction` / `recover_context_overflow` 的 `Ok(result)` 臂通过 `history.clear()` + `push()` 替换 transcript，但**丢弃 `result.summary_prompt`**（生产侧 `Engine::merge_compaction_summary` 折叠进 `session.system_prompt` 的 rolled-up 摘要）。本切片把 summary 记录进 executor slot（`take_usage` 模式），在 host **`run` 返回后**（`&mut self.session` 回到 host 手中时）合并——mirror production host-side post-compaction closure。closure 的另三项（`reinject_compaction_attachments` / `post_compact_cleanup` / full `emit_session_updated`）拆到 25b/25c（需求更硬：reinject 必须在 `run` 内、cleanup 有 merge XOR 互斥 + divorced probe slots）。

- **behavior-equivalence 论证**：deferred-to-post-`run` 等价于 mid-`run` 合并，因为 executor 的 system prompt 是静态快照——`let system = self.config.system.clone();`（`host_executor.rs:2646`），在构造时从 `session.system_prompt` 快照（`mod.rs:1206`，在 `&mut session` 借用之前）。故 `run` 期间任何 `session.system_prompt` 变更对**同 turn** 的请求不可见——合并的 summary 只对**下一 turn**（重新快照）才重要。所以把合并 defer 到 post-`run` 与 mid-`run` 合并行为等价。
- **executor slot（mirror `take_usage`）**：`pending_compaction_summary: std::sync::Mutex<Option<SystemPrompt>>` 字段（`host_executor.rs`，紧邻 `usage`）+ `new` 初始化 `None`。`#[must_use] pub fn take_pending_compaction_summary(&self) -> Option<SystemPrompt>`——one-shot drain（`lock().take()`），镜像 `take_usage`；executor 每 turn 新建故无 cross-turn 泄漏。`fn record_compaction_summary(&self, summary: Option<SystemPrompt>)` helper——通过 `crate::compaction::merge_system_prompts` 折叠（`current.take()` → `*guard = merge_system_prompts(current.as_ref(), summary)`），使一个 turn 内多次 compaction **累积**而非 last-wins（镜像生产 `merge_compaction_summary` 逐次折叠进 `session.compaction_summary_prompt`）。
- **store（两处 Ok 臂）**：`run_compaction` Phase-2 full-compact 臂 + `recover_context_overflow` Phase-2 臂 → `self.record_compaction_summary(result.summary_prompt.clone());`（在 `history.clear()` + `push()` 之前/之后）。替换原 "summary_prompt discarded — same gap as run_compaction." 注释。Phase-1 micro-compact 臂不动（micro-compact 无 `summary_prompt`）。
- **host post-`run` 接线**（`mod.rs`，紧邻 `turn.usage = executor.take_usage();`）：`if let Some(summary) = executor.take_pending_compaction_summary() { self.merge_compaction_summary(Some(summary)); self.emit_session_updated().await; }`。镜像生产 host-side post-compaction closure（`merge_compaction_summary` @ `mod.rs:1411-1418`）。
- **module 文档**："Known gaps in compaction"——"summary-prompt merge dropped" → "absorbed ✅ (post-`run`, slice 25a §E)"（论证静态快照等价性）；"no `emit_session_updated`" → "partial ✅ (merge path only)"；"attachment reinject deferred" → "deferred (25b)"（补 within-`run` host-state access 需求）；"post-compact cleanup deferred" → "deferred (25c)"（补 merge XOR cleanup 互斥 + divorced probe slots）。`run_compaction` / `recover_context_overflow` doc string + inline `Ok(result)` 注释更新。
- **4 个新测试**（`host_executor.rs` test module，新增 "compaction summary_prompt slot" 组 + `system_prompt_text` helper）：`run_compaction_records_summary_prompt`（Phase-2 auto-compact 触发 → slot `Some`、含 LLM summary 文本、one-shot drain 二次取 `None`）、`compaction_summary_flows_to_host_merge_post_run`（reactive context-length recovery → `recover_context_overflow` 记录 → slot `Some`、seam 断言而非 fold——fold 在 `Engine`）、`multiple_compactions_accumulate_summary`（两次 `record_compaction_summary` → 合并保留两个 summary、守卫累积语义非 last-wins）、`no_compaction_yields_none_summary`（high-threshold clean run → slot `None`、host merge no-op）。共 92 个 host_executor 测试通过（88 既有 + 4 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 零新 warning（11 均既有，`task_v2.rs`/`purge.rs`，与本轮无关）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 92 通过（88 既有 + 4 新）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1075 通过（含 4 新；既有 flaky mcp 重连测试本轮通过，2 ignored）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 123 通过（host post-`run` merge 接线在 Engine run 路径中 live，零回归——含 slice 24 的 6 个 reinject 测试）；`cargo +1.90.0 build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**进度（2026-07-13 §E compaction attachment `reinject_compaction_attachments`（during `run`）落地，关闭 framework-core compaction closure 第二项，`feat/pluggable-framework-core`）：**

§E 的第二十六个切片（item #3 "post-compact cleanup" 的子切片 25b，三项 closure 的第二项）落地——关闭 "Known gaps in compaction" 的 "attachment reinject deferred (25b)" 缺口：`HostAgentExecutor::run_compaction` / `recover_context_overflow` 的 `Ok(result)` 臂用 `history.clear()` + `push(result.messages)` 替换 transcript 后**不重注入** plan/todos/subagents/read_files 附件消息（生产 `Engine::reinject_compaction_attachments` 在 host 侧 pre-turn/manual/recover 后推送，但**只在 `executor.run` 外**触发，从不在 mid-turn）。framework-core executor 在 `run` 期间只持 `&mut dyn ChatHistory`（无 `&mut Session`），触不到 `config.plan_state`/`config.todos`/`recent_read_files`/`subagent_api`。本切片把 reinject 搬进 `run`（紧跟 transcript 替换之后），经新 `ReinjectProbe`（Arc-clone 三 host-state 源）+ executor 既有的 `subagent_api`/`turn_meta` + `ChatHistory`（live transcript for dedup/budget/push）读取状态。closure 第三项 `post_compact_cleanup` 仍延后（25c：merge XOR cleanup 互斥 + divorced probe slots）；read_file observe site（`record_read_file_result` 无生产调用方——read_files 候选仅 test 触发）独立后续切片。

- **Arc-ify `Session.recent_read_files`**（`session.rs`，镜像 slice 22 的 `working_set`）：`VecDeque<RecentReadFile>` → `Arc<std::sync::Mutex<VecDeque<RecentReadFile>>>`。clone 审计安全：`Session` derive `Clone` 但无生产 `Session::clone()` 调用方；cycle/checkpoint 原地 mutate `self.session`（保留 `recent_read_files`）；无 by-value move。`record_read_file_result` body 包在 lock guard 下；生产 reinject 读点（`mod.rs`）改 `lock().expect(...).iter().cloned().collect()`；3 个 session.rs 单测改 lock 下访问。
- **extract 纯格式 helper → `pub(crate)`**（`compaction/attachment_reinject.rs`）：`format_plan_reinject_summary`/`format_todo_reinject_summary`/`compaction_reinject_message`（mod.rs 私有 fn → attachment_reinject.rs `pub(crate)`）+ `summarize_subagents`（从 `Engine::compaction_subagent_summaries` 抽出 mapping，Engine 方法瘦身为 `summarize_subagents(&self.host.subagents().live_running_snapshots().await)` 转发）。Option A：只抽纯 free fn（行为恒等），**生产 reinject 循环不动**（enrich/dedup/budget/push 仍 inline 于 `mod.rs`）——零 slice-24 reinject 回归风险。生产调用点改 `crate::compaction::attachment_reinject::…`。
- **`ReinjectProbe` collaborator**（`host_executor.rs`，紧邻 `CompactionProbe`）：`pub struct { plan_state: SharedPlanState, todos: SharedTodoList, recent_read_files: Arc<StdMutex<VecDeque<RecentReadFile>>> }` + `new()`。私有字段（同文件 `HostAgentExecutor` reinject 方法访问，镜像 `CompactionProbe` 先例）。subagent 状态**无需 probe**（executor 已持 `subagent_api` + `live_running_snapshots`）；`<turn_meta>` 富化经既有 `TurnMetaProbe`。
- **`TurnMetaProbe::turn_metadata_block(&self) -> ContentBlock`**：thin forwarder（lock working_set + 调 `turn_meta::turn_metadata_block`），供 reinject 富化步用（enrich 候选前置 `<turn_meta>`，匹配 slice-24 的 `[turn_meta, system-reminder]` shape）。
- **executor reinject 方法**（`host_executor.rs`，镜像生产但读 probe + `subagent_api` + `turn_meta` + `ChatHistory`）：`async fn reinject_compaction_attachments(&self, history: &mut dyn ChatHistory, target_input_budget: Option<usize>) -> usize`。no probe ⇒ early return 0（embeds/tests 不 opt-in 不受影响）。读 `probe.plan_state.lock().await.snapshot()` → `format_plan_reinject_summary`；`probe.todos.lock().await.snapshot()` → `format_todo_reinject_summary`；`self.subagent_api.live_running_snapshots()` → `summarize_subagents`；`probe.recent_read_files.lock().iter().cloned()`。per-candidate 循环（匹配 slice-24）：enrich FIRST（前置 `<turn_meta>` 使 byte-stable equality 让 dedup 工作）→ dedup `history.messages().iter().any(|m| m == &candidate)` → budget trial（`Some` 时，against `history.messages()` + 静态 `config.system` 快照）→ `history.push`。borrow-safety：每次 `history.messages()` 不可变借在 `history.push` 前 NLL 结束；reads 见 prior pushes（live，匹配生产 `session.messages` 语义）。
- **调用点（两 full-compact Ok 臂）**：`run_compaction` Ok 臂（`history.clear()`+push 后、`record_success` 前）`self.reinject_compaction_attachments(history, None).await;`（None budget——auto-compact 不在硬顶，dedup+push only）；`recover_context_overflow` Ok 臂（clear+push 后、`record_compaction_summary` 后）`self.reinject_compaction_attachments(history, Some(target_budget)).await;`（Some budget——硬顶，镜像生产 `Some(target_budget)`）。Phase-1 micro-compact 臂不动（micro-compact 原地清 tool-result content、不移除附件消息——匹配生产：reinject 只在 full-compact 路径）。
- **wire-in**（`mod.rs`，`:1209-1256` block，`&mut session` 借用之前）：`let reinject_probe = ReinjectProbe::new(Arc::clone(&self.config.plan_state), Arc::clone(&self.config.todos), Arc::clone(&self.session.recent_read_files));`（紧邻 `turn_meta_probe` 构造之后），`.with_reinject(Some(reinject_probe))`（`.with_turn_meta(...)` 之后）。
- **module 文档**："Known gaps in compaction"——"attachment reinject deferred (25b)" → "absorbed ✅ (during `run`, slice 25b §E)"（补 `ReinjectProbe` 携三 Arc-clone + `subagent_api` + `TurnMetaProbe` 富化 + 两 full-compact Ok 臂 + None/Some budget split + micro-compact 臂不动）+ read_file observe-site 子弹（`record_read_file_result` 无生产调用方、read_files 候选仅 test 触发、observe site 接线独立后续切片）；"post-compact cleanup" 保 "deferred (25c)"。intro 行 + `CompactionProbe` doc + `run_compaction`/`recover` doc string + 两 Ok 臂 inline 注释更新。
- **5 个新测试**（`host_executor.rs` test module，新增 "compaction reinject (slice 25b §E)" 组 + `populated_reinject_probe` async helper）：`reinject_pushes_plan_todo_readfile_candidates_after_compact`（Phase-2 auto-compact full-path：plan+todos+read_files 填充 → `run` 后 transcript 含 plan-step-one / Active todos resumed / read_file_path.rs 三个 marker，证明接线端到端）、`reinject_dedup_skips_already_present`（直调方法：pre-push 同一 plan candidate → dedup 跳过、pushed==1、plan 计数==1）、`reinject_budget_skips_oversized`（直调方法 + `Some(1)` budget → 全候选 over-budget 跳过、pushed==0、history 不变）、`reinject_enriches_with_turn_meta`（直调方法 + `TurnMetaProbe` → 每个推送候选首 block 含 `<turn_meta>`）、`reinject_no_probe_is_noop`（直调方法 + `.with_reinject(None)` → early return 0、history 不变）。共 97 个 host_executor 测试通过（92 既有 + 5 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 零新 warning（3 均既有，`task_v2.rs` PathBuf + 25a 测试 `history` unused，与本轮无关）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 97 通过（92 既有 + 5 新）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1079 通过 + 1 flaky `mcp::streamable_http_stale_session_reconnects_and_retries_tool_call`（隔离重跑通过，既有偶发 MCP 重连，与本轮无关）+ 2 ignored；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui reinject_compaction_attachments` 6 通过（slice-24 生产 reinject 经 helper 抽出后零回归）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 123 通过（host wire-in live，零回归）；`cargo +1.90.0 build --workspace` 全绿（tui 143 warning 均既有死代码）。

**下一聚焦工作：**
- **read_file observe site**（25b 遗留子项）：`Session::record_read_file_result` 仍无生产调用方（read_file tool-result 路径不喂数据）——`recent_read_files` 仅 test 填充，故 read_files 候选在生产仅 test 触发。把 observe 接进 read_file tool-result 路径（executor 或 bridge）使 `recent_read_files` 生产填充——独立切片（Arc-ification + reinject 路径已就位，待接线）。`run_compaction` reinject 的 provider-budget（当前 `None` budget；传 `context_input_budget_for_provider` 需 thread `api_provider`/`model`）亦后续 refinement。
- **25c — `post_compact_cleanup`**（closure 第三项）：merge XOR cleanup 互斥（生产按 compaction type：full→merge、micro→cleanup）+ divorced `CompactionProbe` circuit-breaker/micro-state slots（fresh per run、非 session 的）需小心。Option B（共享 `select_reinject_candidates` 抽出 enrich/dedup/budget 循环为 `pub(crate)` fn 并重构生产 reinject 用它——会消 ~15 行 orchestration 近重复）亦延后（避免动 slice-24 生产 reinject，保零回归）。
- **`ToolCallStarted` stream-time + bridge 去重**：`Callback::on_tool_start` 透传 wire id 或 bridge name+input pairing。
- **per-input-approval 专用 test**：override-downgrade 断言（ExecutesCode 工具 + dispatcher 返 Auto ⇒ 不 approval）。
- **dead-code deletion 切片**：slice 20 `#[allow(dead_code)]` 17 项中 orphan 的删掉。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：独立 opt-in 切片，仍低优先。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-13 §E read_file observe site 落地，闭合 25b 遗留子项，使 `recent_read_files` 生产填充，`feat/pluggable-framework-core`）：**

§E 的第二十七个切片落地——闭合 slice 25b 的遗留子项："read-file observe site still deferred"。`Session::record_read_file_result` 此前**无生产调用方**（6 个 call site 全在 `#[cfg(test)]`），故 `recent_read_files` 仅 test 填充，compaction reinject 的 read_files 候选在生产仅 test 触发。退役 `handle_deepseek_turn` 的 observe site 在 `turn_loop.rs:2523-2525`（commit `42123572~1`）：tool 执行成功后，`compact_tool_result_for_context(&session.model, "read_file", &output)` 产出 sanitized/compacted 形态，`if output.success && name == "read_file" { session.record_read_file_result(&input, &output_for_context) }`。本切片把该 observe 搬进 `HostAgentExecutor::run_inner` 的 (3) per-tool seam（紧邻 LSP post-edit collect），经既有 `ReinjectProbe`（持有 `recent_read_files` `Arc` clone）写入共享队列。本轮纯增量（`session.rs` + `host_executor.rs` + `engine/mod.rs` + 文档），零既有调用点行为改动；生产路径不受影响。

- **free fn 提取（dedup）**（`session.rs`）：`Session::record_read_file_result` 的 body 提为 `pub(crate) fn record_read_file_result_into(files: &Arc<StdMutex<VecDeque<RecentReadFile>>>, input, output_for_context)`——`Session::record_read_file_result` 变 thin wrapper；新 `ReinjectProbe::record_read_file_result` 调同一 free fn。单一 observe 逻辑源（"lift now"，同 `edit_file_paths` / `summarize_subagents` 先例）。3 个既有 session.rs 单测不变通过。
- **`ReinjectProbe` 扩展**（`host_executor.rs`）：新增 `model: String` 字段（供 `compact_tool_result_for_context` 的 model-dependent context limits——镜像退役 `turn_loop` 的 `&self.session.model`）+ `new()` 第 4 个参数 + `pub(crate) fn model(&self) -> &str` accessor + `pub fn record_read_file_result(&self, input, output_for_context)`（转发 free fn）。
- **executor observe 方法**（`host_executor.rs`）：`fn record_read_file_result(&self, name, input, result: &ToolResult)`（private, sync）——gate `name == "read_file" && result.success`；`reinject: Some` 时 `compact_tool_result_for_context(probe.model(), name, result)` 产出 sanitized 形态（`partially_sanitize_unicode` strip 隐藏 Unicode 攻击 HackerOne #3086545——安全属性：raw-content 路径会丢失），再 `probe.record_read_file_result(input, &output_for_context)` 写入共享 `Arc<VecDeque>`（dedup-by-path + push + trim to 12）。`reinject: None` → silent no-op（无 probe ⇒ 无 `Arc` 可写 ⇒ 数据永不被 reinject 读 ⇒ skip 正确）。锁在 sync critical section 内取/放，不跨 `await`。
- **`run_inner` 接线**（seam 3 per-tool）：在既有 `if !blocked { if let Ok(r) = &result { if r.success { self.collect_lsp_diagnostics(...).await; } } }` 块内、LSP collect 之后插 `self.record_read_file_result(&name, &input, r);`。顺序：`on_tool_start` → loop-guard `record_attempt` → approval gate → `tool.run` → `on_tool_end` → loop-guard `record_outcome` → LSP collect → **read_file observe** → push ToolResult。更新 seam-3 注释（read_file observe ✅）。
- **wire-in**（`mod.rs` `handle_send_message` ~1242）：`ReinjectProbe::new(..., self.session.model.clone())` 第 4 参数——紧邻既有 `Arc::clone(&self.session.recent_read_files)` 之后，`&mut self.session` 借用之前。
- **test doubles**：新增 `ReadFileSpec { content, success }`（`impl ToolSpec`，name `"read_file"`，返回 configurable content + success flag——`new(content)` / `failing(content)` 构造器）；`read_file_tools(spec)` / `read_file_call(path)` helper（镜像 `write_tools`/`write_call`）；`read_file_reinject_probe(&sess)` + `recent_read_files_snapshot(&sess)` helper。`populated_reinject_probe` 传 `sess.model.clone()`（第 4 参数）——5 个既有 reinject 测试零回归。
- **6 个新测试**（`host_executor.rs` test module，新增 "read_file observe site (slice 25b §E follow-on)" 组）：`read_file_observe_populates_recent_read_files`（read_file 成功 → `recent_read_files` 1 entry：path + preview 含 `pub fn library()`）、`read_file_observe_skips_non_read_file_tools`（echo 工具 → 队列空）、`read_file_observe_skips_failed_read_file`（`ReadFileSpec::failing` → `success: false` → 不 observe）、`read_file_observe_dedup_by_path_keeps_latest`（同 path 两次 read_file → 1 entry、latest content `fn second_read()` 保留）、`read_file_observe_strips_hidden_unicode`（**安全守卫**：content 含 U+200B zero-width space → preview 不含它、但 `clean_start` 保留——证 `compact_tool_result_for_context` 的 `partially_sanitize_unicode` 在 observe 路径生效）、`read_file_observe_none_reinject_is_noop`（`reinject: None` → 无 panic、队列空）。共 103 个 host_executor 测试通过（97 既有 + 6 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 零新 warning（11 均既有，`task_v2.rs`/既有 unused，与本轮无关——changed-file 专项 grep 零命中）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 103 通过（97 既有 + 6 新 read_file_observe）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1086 通过（0 失败、2 ignored，原 1079 + 6 新 + 既有 flaky MCP 重连本轮通过）；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（未改——不动核心 trait）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 123 通过（host wire-in live，零回归）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui reinject_compaction_attachments` 6 通过（slice-24 生产 reinject 经 helper 抽出后零回归）；`cargo +1.90.0 build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- **25c — `post_compact_cleanup`**（compaction closure 第三项，最后一项）：生产 `post_compact_cleanup` force-rebuild working set + reset per-file cycle state（`micro_compact_state` / `circuit_breaker` / `last_system_prompt_hash`）。**已知复杂点**：该函数当前在生产**全死**（唯一非 test 调用方在 `#[allow(dead_code)]` 的 `Engine::recover_context_overflow` 的 micro/partial 臂——full-compact 臂只 merge 不 cleanup）；framework-core executor 只有 full-compact 臂（无 micro/partial），故 naive 接线会 combine merge+cleanup（生产 XOR：full→merge, micro→cleanup, partial→both）；状态位置失配——production cleanup reset 跨 turn 持久的 `Session` 字段，executor 只触达 per-turn fresh 的 `CompactionProbe` 槽。两条路径：**Option A**——hoist 到 host post-`run`（紧邻 25a 的 `merge_compaction_summary` @ `mod.rs:1303-1305`，host 有 `&mut Session`，最简，匹配 25a 模式，但需 thread compaction-type 信号以守 XOR）；**Option B**——新 `CleanupProbe`（Arc-clone `working_set` / `micro_compact_state` / `circuit_breaker`，镜像 `ReinjectProbe`/`TurnMetaProbe`）让 executor mid-`run` reset。read_file observe site（本轮）已闭合，不阻塞 25c。
- **`run_compaction` reinject 的 provider-budget refinement**：当前 `None` budget；传 `context_input_budget_for_provider` 需 thread `api_provider`/`model`（`ReinjectProbe.model` 已就位，缺 `api_provider`）。低优先 refinement。
- **`ToolCallStarted` stream-time + bridge 去重**：`Callback::on_tool_start` 透传 wire id 或 bridge name+input pairing。
- **per-input-approval 专用 test**：override-downgrade 断言（ExecutesCode 工具 + dispatcher 返 Auto ⇒ 不 approval）。
- **dead-code deletion 切片**：slice 20 `#[allow(dead_code)]` 17 项中 orphan 的删掉。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：独立 opt-in 切片，仍低优先。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-13 §E `post_compact_cleanup` 落地，关闭 framework-core compaction closure 第三项/最后一项，`feat/pluggable-framework-core`）：**

§E 的第二十八个切片（compaction closure 三项的第三项/最后一项）落地——关闭 "Known gaps in compaction" 的 "post-compact cleanup deferred (25c)" 缺口：生产 `post_compact_cleanup(&mut Session)` force-rebuild working set + reset per-file cycle state（`micro_compact_state` / `circuit_breaker` / `last_system_prompt_hash`），但 framework-core executor 在 `run` 期间只持 `&mut dyn ChatHistory`（无 `&mut Session`），触不到这三个 plain `Session` 字段。**用户决策**：Option A（post-`run`、host 侧、复用既有 free fn）+ trigger scope "any non-merge compaction"（pre-request micro / recovery micro / hard-trim；**NOT** full-compact——full 经 25a merge 路径）。Option A 紧邻 25a 的 `merge_compaction_summary` 接线（`mod.rs:1303-1305` → 重构为 merge-then-cleanup），host 在 `run` 返回后持 `&mut self.session`，最简且匹配 25a 模式；Option B（新 `CleanupProbe` Arc-ify 三 plain 字段让 executor mid-`run` reset）被否——`CompactionProbe::new` 每 turn 新建（executor 在 `handle_send_message` per-turn 构造），probe 的 `micro_state`/`circuit_breaker` 槽 divorced 于 session 的 vestigial 字段，mid-run reset probe 不 reset session（需 Arc-ify 3 字段，invasive 且 pointless——probe fresh-each-turn）。四 reset 中仅 2 个 live-meaningful（`last_system_prompt_hash=None` 强下一 turn re-assembly；`working_set.force_rebuild()` 清 stale entries）；另 2 个（`micro_compact_state`/`circuit_breaker`）reset vestigial session 字段（live 路径不读——用 probe 的 divorced 槽），harmless、匹配生产 intent。

- **Step 1 — executor slot + accessors**（`host_executor.rs`，镜像 25a `pending_compaction_summary`）：`pending_post_compact_cleanup: std::sync::Mutex<bool>` 字段（紧邻 `pending_compaction_summary`/`usage`），`new()` Self-literal 默认 `false`（无新 ctor 参数 → 103 既有 test 调用点不动，同 slice 20/21 模式）。`#[must_use] pub fn take_pending_post_compact_cleanup(&self) -> bool`——one-shot drain（`std::mem::replace(&mut *guard, false)`）。`fn signal_post_compact_cleanup(&self)`——置 `true`（idempotent，镜像 `record_compaction_summary`）。
- **Step 2 — signal-setting sites（3 处，全 non-merge 臂）**：(1) `run_compaction` Phase-1 micro（`if cleared > 0 {}` 内，clear+push 循环 + `tracing::info!` 之后）；(2) `recover_context_overflow` Phase-1 micro success（`if after_micro <= target_budget {}` 内、`return true` 之前）；(3) `recover_context_overflow` Phase-3 hard-trim（`if after_compact > target_budget {}` 内，`let before_trim = msgs.len();` 于 `while` 前，trim 后 `if before_trim > msgs.len() { signal }`——仅 trim 实际移除消息时 fire，bounded by `MIN_RECENT_MESSAGES_TO_KEEP`）。**NOT set in**（merge 路径，保 full→merge XOR）：`run_compaction` Phase-2 full Ok 臂（经 25a 记 summary）+ `recover` Phase-2 full Ok 臂（同）。两 slot 编码 XOR：micro/partial→cleanup-only、full→merge-only、full+trim（rare）→merge THEN cleanup。
- **Step 3 — host post-`run` 接线**（`mod.rs`，重构 25a 块处理两信号，**merge first then cleanup**——匹配生产 partial order `:1901` merge → `:1905` cleanup；both fire 时 net on `last_system_prompt_hash`：merge 设、cleanup 清 → `None` → 下一 turn re-assembly）：`take_pending_compaction_summary()` + `take_pending_post_compact_cleanup()` 两 drain；`if let Some(s) = summary { self.merge_compaction_summary(Some(s)); }`；`if needs_cleanup { crate::compaction::post_compact_cleanup::post_compact_cleanup(&mut self.session); }`；`if had_summary || needs_cleanup { self.emit_session_updated().await; }`。更新 25a 注释块（原 "reinject + post_compact_cleanup remain deferred (25b/25c)" → reinject ✅ 25b、cleanup ✅ 25c）。
- **Step 4 — module 文档 + inline 注释**（`host_executor.rs`）："Known gaps in compaction" `:416` "post-compact cleanup deferred (25c)" → "absorbed ✅ (post-`run`, slice 25c §E)"（Option A 论证 + trigger = any non-merge + 复用 free fn + merge-then-cleanup order + 2 live + 2 vestigial + probe fresh-each-turn）；capacity-recovery bullet `:510` re-flag 25c → 标 absorbed；两 full-compact Ok 臂 inline 注释（`run_compaction` + `recover`）原 "post_compact_cleanup remains deferred (25c)" → 更新（cleanup 是 non-merge-only，full→merge 故此处不 cleanup 是正确的——XOR）。
- **Step 5 — 6 个新测试**（`host_executor.rs` test module，新增 "post-compact cleanup signal (slice 25c §E)" 组，镜像 25a capacity/compaction helper）：`cleanup_signal_none_on_clean_run`（clean run、无 compaction → slot false）、`cleanup_signal_on_pre_request_micro`（`seed_large_file_read` + high-threshold → Phase-1 micro fire → signal true、`compaction_calls()==0`）、`cleanup_signal_on_recovery_micro`（over-budget + large tool result + capacity probe → recovery Phase-1 micro clears enough → signal true，镜像 `capacity_micro_compact_clears_tool_results_in_recovery`）、`cleanup_signal_on_hard_trim`（over-budget + `with_compaction_error` → Phase-2 fails → Phase-3 trims → signal true，镜像 `capacity_over_budget_recovers_via_hard_trim`）、`full_compact_does_not_signal_cleanup`（over-budget + mock summary → Phase-2 full → signal false AND `take_pending_compaction_summary().is_some()`——XOR 守卫，镜像 `capacity_over_budget_recovers_via_compaction`）、`take_pending_post_compact_cleanup_is_one_shot`（micro → take true、take again false）。共 109 个 host_executor 测试通过（103 既有 + 6 新）。`post_compact_cleanup` free fn 的 3 个既有单测覆盖 4 resets；host 侧 `mod.rs` 接线经 tui engine 回归覆盖（25a 先例——无新 Engine 测试）。

**已知 by-design divergences（记录）：**
- **Phase-2 full + Phase-3 trim**（rare）：两信号皆 fire → merge+cleanup（partial→both 类比）。生产把 trim-within-full 当 merge-only；framework-core 亦 cleanup。保守（cleanup safe/idempotent）；`last_system_prompt_hash` net `None`（下一 turn re-assembly）、`working_set` force_rebuilt。
- **probe 的 breaker/micro_state fresh-each-turn**（生产 executor per-turn）；`compaction_cross_turn_circuit_breaker_persistence` 测试是单测场景（一个 executor 跨两 `run`），生产不发生。25c reset session 的 vestigial 字段（非 probe 的），匹配生产 intent。
- **post-run 时序**：cleanup reset 的字段只对**下一** turn 重要（同 25a merge）；mid-turn `working_set` 在 recovery micro 后 stale 直到 post-run cleanup——minor，匹配 25a 先例。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 零新 warning（11 均既有，`task_v2.rs`/`purge.rs`/既有 unused，与本轮无关——changed-file 专项 grep 零命中）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 109 通过（103 既有 + 6 新 cleanup_signal）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1092 通过（0 失败、2 ignored，原 1086 + 6 新）；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（未改——不动核心 trait）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 123 通过（host post-`run` merge+cleanup 接线 live，零回归）；`cargo +1.90.0 build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- **`run_compaction` reinject 的 provider-budget refinement**：当前 `None` budget；传 `context_input_budget_for_provider` 需 thread `api_provider`/`model`（`ReinjectProbe.model` 已就位，缺 `api_provider`）。低优先 refinement。
- **`ToolCallStarted` stream-time + bridge 去重**：`Callback::on_tool_start` 透传 wire id 或 bridge name+input pairing。
- **per-input-approval 专用 test**：override-downgrade 断言（ExecutesCode 工具 + dispatcher 返 Auto ⇒ 不 approval）。
- **dead-code deletion 切片**：slice 20 `#[allow(dead_code)]` 17 项中 orphan 的删掉。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：独立 opt-in 切片，仍低优先。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-13 §E `ToolCallStarted` stream-time + bridge dedup 落地，wire id 透传 + `bridge-{n}` 退役，`feat/pluggable-framework-core`）：**

§E 的第二十九个切片落地——关闭 ROADMAP "下一聚焦工作" 的 "`ToolCallStarted` stream-time + bridge 去重" 项。生产 `handle_deepseek_turn`（slice 20 退役）在流式 `ContentBlockStop`（tool 块）处发 `Event::ToolCallStarted { id, name, input }`（wire tool id），但 framework `HostAgentExecutor` 此前**不**——`reduce_stream` 的 `ContentBlockStop`（tool 块）只 spawn early-start task、不发 callback；`on_tool_start` 只在 execute-time（tool loop）fire，且 `CallbackBridge` 合成 `bridge-{n}` id（wire id 丢失）。UI 看到 "calling X" 延迟（流式完成后）、且 id 不可关联。本轮两处协同改动闭合该缺口：(1) CORE trait——`Callback::on_tool_start` 加 `id: &str` 首参（透传 wire id）+ `StreamDelta::ToolCallStarted { id, name, input }` variant；(2) bridge 去重——`CallbackBridge::on_stream_delta(ToolCallStarted)` 发 `Event::ToolCallStarted`（real wire id）+ 标 announced；`on_tool_start(id, ...)` 查 announced set——已宣布则跳过 `Event::ToolCallStarted`（dedup），未宣布则发（fallback for `DefaultAgentExecutor` + `accumulate_stream`）。`bridge-{n}` 合成退役。`on_tool_end` 签名不动——LIFO `pending` 栈 pop 出 `on_tool_start` push 的 real wire id。

- **Step 1 — CORE `StreamDelta` + `Callback::on_tool_start`**（`crates/agent/src/callback/mod.rs`）：`StreamDelta` 加 `ToolCallStarted { id: String, name: String, input: serde_json::Value }` variant（enum doc 更新——移除 "not here yet" 注、variant 列表 + dedup 说明）；`Callback::on_tool_start` 加 `id: &'a str` 首参（default impl `let _ = (id, name, input);`，doc 说明 wire id + dedup intent）；`CallbackSet::on_tool_start` forward `id`；`noop_callback_defaults_are_callable` 测试加 `on_tool_start("t1", "echo", &Value::Null)` + `on_stream_delta(&StreamDelta::ToolCallStarted{..})` 调用。纯增量——新 variant default no-op（所有既有 `Callback` impl 不受影响），新参有 default impl（既有 impl 不 break，仅需传 `id` 的两处调用点更新）。
- **Step 2 — `RecordingCallback` + `DefaultAgentExecutor`**（`crates/agent/src/executor/mod.rs`）：`RecordingCallback::on_tool_start` 加 `id: &str`（log 格式 `"tool_start:{name}"` 不变——test double 忽略 `id`，最小化 test churn，既有 `tool_start:echo` 断言绿）；tool loop `callback.on_tool_start(&id, &name, &input)`（`id` 来自 `tool_uses` 元组解构，已就位）。
- **Step 3 — `CallbackBridge`**（`crates/agent-runtime/src/callback_bridge.rs`）：`BridgeState` 移 `counter: u64`、加 `announced: std::collections::HashSet<String>`；`on_tool_start(id, ...)` push `(id, input)` 到 LIFO `pending`（real wire id），若 `id` NOT in `announced` → 发 `Event::ToolCallStarted`（fallback），若 IN → 跳过（dedup），always fire `ToolCallBefore` hook；`on_tool_end` 不动——LIFO pop 给 real wire id，`Event::ToolCallComplete` 携 real wire id；`on_stream_delta` 加 `ToolCallStarted` 臂——发 `Event::ToolCallStarted`（real wire id）+ insert `announced`。模块 doc "Bridged vs. documented gaps" 表 + "Synthesized tool-call id" 段 + stream-time `ToolCallStarted` gap 注更新（absorbed）。既有测试更新：`bridge_forwards_tool_start_and_end...` / `bridge_emits_events_even_without_hooks` 的 `on_tool_start("echo", &input)` → `on_tool_start("wire-1", "echo", &input)`，断言 `s_id.starts_with("bridge-")` → `s_id == "wire-1"`；`executor_drives_callback_bridge` 不动（`DefaultAgentExecutor` 自动透传 wire id）。
- **Step 4 — `HostAgentExecutor`**（`crates/agent-runtime/src/engine/host_executor.rs`）：`reduce_stream` `ContentBlockStop`（tool 块）`finalize_tool_input` 后、early-start spawn 前——发 `on_stream_delta(&StreamDelta::ToolCallStarted { id, name, input })`（**ALL** tool blocks，registered/unregistered、early-start-safe/unsafe——UI 在 tool block finalized 时即见 "calling X"）；tool loop `callback.on_tool_start(&id, &name, &input)`（`id` 已就位）。模块 doc "Known gaps in early-tool-start" + `reduce_stream` doc 更新（stream-time `ToolCallStarted` gap → absorbed）。
- **Step 5 — 4 个新测试**（`host_executor.rs` test module，新增 "§E slice 29 — ToolCallStarted stream-time + bridge dedup" 组）：`stream_emits_tool_call_started_at_content_block_stop`（`DeltaRecorder` 捕获 `StreamDelta::ToolCallStarted { id: "toolu_1", name: "echo", input: {"text":"hi"} }`——证 stream-time seam 在 `ContentBlockStop` fire）、`tool_call_started_flows_through_callback_bridge_with_wire_id`（端到端：executor → bridge → `Event::ToolCallStarted` 携 real wire id "toolu_42"、**非** `bridge-{n}`、start/end id 配对）、`tool_call_started_not_duplicated_at_execute_time`（dedup：恰好**一个** `Event::ToolCallStarted` per call——stream-time 发 + execute-time `on_tool_start` 跳过）、`tool_call_started_emitted_even_for_unregistered_tool`（ghost 工具 → `StreamDelta::ToolCallStarted` 仍发——UI 在 execute-time lookup 失败前即见 "calling ghost"）。共 113 个 host_executor 测试通过（109 既有 + 4 新）。

**验证：** `cargo +1.90.0 build -p codesmith-agent` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（CORE trait 改动不破既有——`RecordingCallback` log 格式不变）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 113 通过（109 既有 + 4 新 slice 29）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib callback_bridge` 8 通过（5 bridge-module + 3 host_executor bridge-flow，3 既有更新 wire-1）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1096 通过（0 失败、2 ignored，原 1092 + 4 新）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 123 通过（0 失败、1 ignored——bridge dedup 在 live `handle_send_message` 路径零回归，wire id 替 `bridge-{n}` 不破既有断言）；`cargo +1.90.0 build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。changed-file 专项 grep 零新 warning（11 均既有 unused/deprecated，与本轮无关）。

**下一聚焦工作：**
- **`run_compaction` reinject 的 provider-budget refinement**：当前 `None` budget；传 `context_input_budget_for_provider` 需 thread `api_provider`/`model`（`ReinjectProbe.model` 已就位，缺 `api_provider`）。低优先 refinement。
- ~~**per-input-approval 专用 test**~~ → ✅ 已落地（slice 30 §E，见下）。
- **dead-code deletion 切片**：slice 20 `#[allow(dead_code)]` 17 项中 orphan 的删掉。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：独立 opt-in 切片，仍低优先。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-14 §E per-input-approval 专用 test 落地，闭合 slice 20 Step 4 测试缺口，`feat/pluggable-framework-core`）：**

§E 的第三十个切片落地——闭合 slice 20（wire-in cutover）遗留的测试缺口："per-input-approval 专用 test"。slice 20 Step 4 落地了 `request_approval` 的 per-input 动态 override 路径（`tool_dispatcher.approval_requirement_for(name, input)` 先于静态 `requires_approval(&tool.capabilities())` gate 查询——`Some(req)` 用 `req != Auto`，`None` 回退静态 gate，镜像 `turn_loop.rs:1700` 的 `registry.approval_requirement_for(..)`），但仅有构建路径无回归断言覆盖，缺 override-downgrade 专用测试。本切片用新 `FakeDispatcher` test double 锁定该路径的三条 match 臂，零既有调用点行为改动（`request_approval` 实现不变；纯测试新增）。

- **`FakeDispatcher` test double**（`host_executor.rs` test module）：`impl ToolDispatcher`，持 `Mutex<Option<ApprovalRequirement>>`，`approval_requirement_for` 对任意 (name, input) 返同一可配置答案。其余 10 方法 stub（`has_tool`→true / `resolve`→identity / `metadata`→None / `is_destructive`+`is_interactive`→false / `validate_input`→Ok / `to_api_tools*`→空 / `execute`→`unreachable!`（executor 走 `Tool::run` 非 `ToolDispatcher::execute`）/ `hook_host`→None）。镜像 `FakeLsp`/`FakeSubAgentApi` 的最小注入模式——不拉入完整 `HostServices`。
- **`ExecSpec` test double**（紧邻 `WriteSpec`）：`impl ToolSpec`，name `"exec_shell"`，声明 `ToolCapability::ExecutesCode`（故静态 `requires_approval` 返 true）。`exec_tools()` / `exec_call()` helper 镜像 `write_tools()` / `write_call()`。
- **3 个新测试**（覆盖 `request_approval` override match 的三条臂）：
  - **`per_input_approval_downgrade_skips_gating`**（headline，匹配 ROADMAP 描述）：`ExecSpec`（ExecutesCode→静态 gate says 需审批）+ `FakeDispatcher(Some(Auto))` → override-**downgrade**。approval channel 有但**不推决策**（若误触发 gate 则 `recv()` 阻塞）。2 s timeout guard 证不阻塞；断言 tool ran ungated（`ran:ls`）、**无** `ApprovalRequired` event。
  - **`per_input_approval_upgrade_fires_gating`**：`EchoSpec`（ReadOnly→静态 gate says 不审批）+ `FakeDispatcher(Some(Required))` → override-**upgrade**。预推 Approved。断言 `ApprovalRequired` event 发出（id=`call_1`/tool_name=`echo`/fingerprint 非空）+ tool ran after approval（early-started read-only task reused，结果带 workspace stamp）。
  - **`per_input_approval_none_opinion_falls_back_to_static_gate`**：`ExecSpec`（静态 gate says 需审批）+ `FakeDispatcher(None)`（dispatcher 无意见）→ 回退静态 gate。预推 Approved。断言 `ApprovalRequired` event 发出（证 `None` 臂不静默禁用静态 gate）+ tool ran after approval。
- **三臂覆盖**：`Some(Auto)`（req==Auto ⇒ false，downgrade）、`Some(Required/Suggest)`（req!=Auto ⇒ true，upgrade）、`None`（fallback to `requires_approval`）。`Suggest` 与 `Required` 在 `req != Auto` 等价（gate fires），故用 `Required` 代表 non-Auto 臂即可。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime`（lib）零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 零新 warning（11 均既有——`captured_requests`/unused imports/`tempfile::into_path` deprecated/unused vars，changed-file 专项 grep 零命中）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 116 通过（113 既有 + 3 新 per-input-approval）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1099 通过（0 失败、2 ignored，原 1096 + 3 新）；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（未改——不动核心 trait）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 123 通过（0 失败、1 ignored——live `handle_send_message` wire-in 路径零回归，`request_approval` 实现不变）；`cargo +1.90.0 build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- ~~**`run_compaction` reinject 的 provider-budget refinement**~~ → ✅ 已落地（slice 31 §E，见下）。
- **dead-code deletion 切片**：slice 20 `#[allow(dead_code)]` 17 项中 orphan 的删掉（streaming config cluster / `mcp_tool_approval_description` / `emit_tool_audit`），superseded 方法按 re-wire 决策保留或删。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：独立 opt-in 切片，仍低优先。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-14 §E `run_compaction` reinject provider-budget refinement 落地，闭合 compaction closure 精化项，slice 31，`feat/pluggable-framework-core`）：**

§E 的第三十一个切片落地——闭合 slice 25b（compaction attachment reinject）遗留的精化项："`run_compaction` reinject 的 provider-budget refinement"。slice 25b 的 `run_compaction` Ok 臂调用 `reinject_compaction_attachments(history, None)` — `None` budget 意味着不做 budget trial（仅 dedup + push）。但生产侧 `Engine::reinject_compaction_attachments` 在 `mod.rs:1465` 和 `:1620` 传的是 `context_input_budget_for_provider(self.api_provider, &self.session.model)`。auto-compaction 后刚把 transcript 压小，reinject 不应再把超预算的候选推回去——本切片闭合该正确性缺口。本轮纯增量（`host_executor.rs` + `mod.rs` + 文档），零既有调用点行为改动（`ApiProvider` 是 `Copy`，wire-in 加一个 by-value 参数，匹配紧邻的 `CapacityProbe::new` 先例）；生产路径不受影响。

- **`ReinjectProbe` 扩展**（`host_executor.rs:875`）：加 `api_provider: ApiProvider` 字段（紧邻既有 `model`）。`ApiProvider` 是 `Copy`（`config_types.rs:198` derive `Clone, Copy`），cheap by-value 传递。`ReinjectProbe::new`（`:895`）加 `api_provider: ApiProvider` 第 5 参数。新 `pub(crate) fn provider_input_budget(&self) -> Option<usize>` accessor（`:911` 之后）——调 `context_input_budget_for_provider(self.api_provider, &self.model)`（已 imported at `:639`），单一源 budget 计算，匹配生产 `mod.rs:1465` 的表达式。
- **wire-in**（`mod.rs:1242`）：`ReinjectProbe::new(...)` 加 `self.api_provider` 第 5 参（Copy，无 `.clone()`，匹配紧邻的 `CapacityProbe::new` at `:1262` 传 `self.api_provider` 的先例）。
- **call site**（`host_executor.rs` `run_compaction` Ok 臂，原 `:2735`）：`None` → `self.reinject.as_ref().and_then(|p| p.provider_input_budget())`。`probe = &self.compaction` 是不可变借用，`self.reinject` 也是不可变借用，`self.method()` 不可变——多重不可变借用无冲突（既有代码 `:2735` 已在此模式运行）。`None` 时（probe 不存在或 model 未知）budget trial 跳过——同 slice 25b 行为。
- **module 文档**（`host_executor.rs`）：`:412-414` "remains a refinement" → "absorbed ✅ (slice 31 §E)"；`run_compaction` doc string（`:2636`）"`None` budget; dedup + push only" → "provider budget from `ReinjectProbe::provider_input_budget()`"；inline 注释（`:2723`）"`None` budget" → "provider budget"；`reinject_compaction_attachments` 方法 doc（`:2544`）"`None` for the auto-compact path" → "provider's input-side token budget (slice 31 §E)"。
- **test helpers**：`populated_reinject_probe(sess, api_provider: ApiProvider)`（加第 2 参，4 个调用方各加 `ApiProvider::Deepseek`）；`read_file_reinject_probe(sess, api_provider: ApiProvider)`（加第 2 参，5 个调用方各加 `ApiProvider::Deepseek`）。既有调用方传 `Deepseek`（session model `"mock-v0"` 经核实是 DeepSeek 已知 model → `provider_input_budget()` 返 `Some(122880)` 而非 `None`，但既有测试调 `reinject_compaction_attachments` 时自带 budget 参数 `None`/`Some(1)`，不经 `run_compaction` 路径，故 budget 值不影响——零回归）。
- **3 个新测试**（`host_executor.rs` test module，新增 "reinject provider-budget (slice 31 §E)" 组）：
  - `reinject_provider_budget_known_returns_some`（单元：`Ollama`/`"llama2"` → `provider_input_budget()` `Some` 且 > 0——证 budget 计算接线）。
  - `reinject_provider_budget_matches_context_input_budget_for_provider`（单元：`probe.provider_input_budget()` == `context_input_budget_for_provider(probe.api_provider, &probe.model)`——证 helper 与生产同表达式）。
  - `reinject_auto_compact_respects_provider_budget`（集成 headline：`sess.model = "llama2"` + `Ollama` provider（budget 3072）+ 10 个 read_file 条目（每个 preview 1.2 KB → 合并候选 ≈ 12 KB ≈ 4.5K conservative tokens > 3072）→ Phase-2 auto-compact 触发后 read_files 候选被 budget trial 拒绝（不在 transcript 中）；plan 候选（小）仍 push——证 `run_compaction` Ok 臂现在传 `Some(budget)` 而非 `None`）。`record_read_file_result_into` 把 preview 截到 `RECENT_READ_FILE_SNIPPET_CHARS = 1200`，故需 10 条合并才能超 3072 预算。
- **已知设计取舍**：`provider_capability`（`config_types.rs:384`）对 `Ollama` 等 provider 返回固定 `context_window` 不论 model 名，故 `context_input_budget_for_provider(Ollama, any_model)` 恒返 `Some(3072)`——`None` 仅在 budget 算术下溢（window − output − headroom ≤ 0）时发生，标准 provider 不触达。auto-compact 路径 `None` budget 在 probe 不存在或 provider 未知时仍 fallback（dedup + push only，同 slice 25b 行为）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零 warning；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 零新 warning（11 均既有——`task_v2.rs`/`purge.rs`/既有 unused imports/deprecated/vars，changed-file 专项 grep 零命中）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 119 通过（116 既有 + 3 新 provider-budget）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1101 通过、1 flaky `mcp::streamable_http_stale_session_reconnects_and_retries_tool_call`（隔离重跑通过，既有偶发 MCP 重连，与本轮无关）、2 ignored（原 1099 + 3 新 = 1102，−1 flaky = 1101 passed）；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（未改——不动核心 trait）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 123 通过（0 失败、1 ignored——host wire-in live 路径零回归）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui reinject_compaction_attachments` 6 通过（slice-24 生产 reinject 零回归）；`cargo +1.90.0 build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- ~~**dead-code deletion 切片**~~ → ✅ 已落地（slice 32 §E，见下；orphan 项已删，superseded 方法按 re-wire 决策保留）。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：独立 opt-in 切片，仍低优先。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-14 §E dead-code deletion 落地，orphan 项删除，superseded 方法保留，slice 32，`feat/pluggable-framework-core`）：**

§E 的第三十二个切片落地——关闭 slice 20（wire-in cutover）遗留的 "dead-code deletion 切片" 项。slice 20 退役 `handle_deepseek_turn` 后用 `#[allow(dead_code)]` / `#![allow(dead_code)]` 压制了 17 项孤儿代码。本切片删掉其中**真正 orphan**（零用法、无 deferred re-wire）的三组，**保留** superseded 方法（按 ROADMAP "superseded 方法按 re-wire 决策保留或删" 的判断——每个都持有 deferred re-wire 逻辑或 paired-lifetime，删了会丢参考实现）。本轮纯删除（4 文件），零既有调用点行为改动；生产路径不受影响。

经全量 liveness grep（`rg` 跨 `agent-runtime`/`tui`/`agent`）核实每个 `allow(dead_code)` 项的用法后划定边界：
- **streaming.rs**：模块文档自己把模块分成 "orphan cluster"（`*_STREAM_CHUNK_TIMEOUT_SECS` / `stream_chunk_timeout_secs` / `ContentBlockKind` / `STREAM_MAX_*`）vs "remain live"（`filter_tool_call_delta` / `should_transparently_retry_stream` / `TOOL_CALL_*_MARKERS` / `ToolUseState`）。删除前核实：保留项全经 `pub use` re-export（`mod.rs:2827-2832`）或被 exported 项内部使用 → 删除 orphan cluster 后**不被 dead-code 标记** → 可安全 drop 模块级 `#![allow(dead_code)]`。`MAX_STREAM_ERRORS_BEFORE_FAIL` / `MAX_TRANSPARENT_STREAM_RETRIES` 归 "retry policy"（非 orphan cluster），保留——前者经 `pub use` re-export + tui regression-pin（`tests.rs:3282`），后者被 live `should_transparently_retry_stream` 使用。

**删除（三组 orphan）：**

- **`streaming.rs` orphan config cluster**（10 项 + 1 test）→ drop 模块级 `#![allow(dead_code)]`：
  - `ContentBlockKind` enum（零用法）。
  - `DEFAULT_STREAM_CHUNK_TIMEOUT_SECS` / `MIN_STREAM_CHUNK_TIMEOUT_SECS` / `MAX_STREAM_CHUNK_TIMEOUT_SECS` / `STREAM_IDLE_TIMEOUT_ENV`（仅被下面两 fn 用）。
  - `stream_chunk_timeout_secs()` / `stream_chunk_timeout_secs_from_env()`（sole consumer 是退役的 `handle_deepseek_turn`）+ 其 test `stream_chunk_timeout_defaults_and_clamps_env_values`（test module 唯一 test → 整个 `#[cfg(test)] mod tests` 移除）。
  - `STREAM_MAX_CONTENT_BYTES` / `STREAM_MAX_DURATION_SECS`（零用法）。
  - 模块文档改为 "slice 32 §E deleted that orphan cluster; the scrubbers / retry policy / `ToolUseState` remain live"。

- **`mcp_tool_approval_description`**（`dispatch.rs:397`）：零用法（不 re-export、不调用——`rg` 跨 workspace 零命中）。删 fn + doc + `#[allow(dead_code)]`。re-wire 进 `CallbackBridge` 是后续切片（会写新代码，非复用此 fn）。`mcp_tool_is_read_only`（被此 fn 调用）经 `mod.rs:2823` re-export，不受影响。

- **`emit_tool_audit` + 3 test**（`tool_execution.rs:128`）：production caller 随 `handle_deepseek_turn` 退役；仅其自身 `#[cfg(test)]` 的 3 个 test（`emit_tool_audit_writes_jsonl_line_when_env_var_set` / `_is_noop_when_env_var_unset` / `_creates_parent_directory`）调用。删 fn + doc + `#[allow(dead_code)]` + 3 test + `AUDIT_TEST_GUARD` static / `audit_test_guard()` fn（仅被这 3 test 用 → 同步删）。test module 的 `#![allow(unsafe_code)]`（仅 `set_var` unsafe 用）+ `use serde_json::json` + `use std::{sync::Mutex, ...}` 同步删（terminal-guard 2 test 不用它们）。非-test import `use std::{fs::OpenOptions, io::Write, ...}` → `use std::{sync::Arc, time::Duration}`（`OpenOptions`/`io::Write` 仅 `emit_tool_audit` 用；`Arc`/`Duration` 仍被非-test 代码用，核实 `Arc`@51 / `Duration`@59）。

- **cascade cleanup**：删 `emit_tool_audit` 后 `mod.rs:12` 的 `use std::path::PathBuf` 变 unused（sole transitive consumer 经 `use super::*` 链是 `emit_tool_audit` 的 `PathBuf::from`）→ 同步删（lib build 零 warning 确认无其他消费者）。

**保留（superseded 方法/字段——deferred re-wire / paired-lifetime，`#[allow(dead_code)]` 留存 + 既有 doc 注明）：**
- `layered_context_checkpoint`（`mod.rs:2102`，#159 nav-aids）——零 caller，但 `seam()` 有其他 live caller（`:2242` briefing / `:2382` reset），删此 fn 不会 cascade；保留因 #159 nav-aids 可能在 wire-in/executor pre-request re-wire 时复用参考。
- `Engine::recover_context_overflow`（`mod.rs:1833`）——executor 有自己的简化三阶段版，但此方法持有 capacity slice 11 显式 deferred 的 "responsive compact cascade (Phase 1)" 四步级联参考逻辑；`host_executor.rs:2942` 的 cross-ref 注释仍大致准确（引用此 KEPT 方法，注中 `mod.rs:1850` 实指 `:1852` Phase-1 cascade，±2 行可接受）。
- KoD cluster（`knowledge_prefetch` 字段 + `kod_prefetch_spawn` / `kod_prefetch_collect`）——distinct planned feature（Knowledge on Demand），re-wire 进 executor 是后续切片。
- `rx_user_input` 字段——paired-lifetime（tui 仍构造 `tx_user_input` sender，consumer `await_user_input` 退役）。
- `tool_exec_lock` 字段——couples to deferred Gate-A `CapacityController` + parallel-exec（取它为参的 fn 自身 deferred）。
- `EarlyToolResult` / `EarlyToolTask`（`turn_loop.rs`）——`dispatch.rs:61` 作 type 引用；speculative-dispatch re-wire deferred（turn_loop 模块级 `#![allow(dead_code)]` 保留）。
- `CancelReason` enum 变体（reserved for #1541，tui `handle.rs` live）/ `ToolExecGuard` RAII 字段（intentional）/ `ToolExecOutcome.index`（diagnostic）/ `Op::*` arms（defensive handlers）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime`（lib）**零 warning**（drop streaming 模块级 allow 后零新 warning——核实所有保留项经 `pub use` re-export 或被 exported 项用）；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 零新 warning（11 均既有——`callback_bridge.rs`/`mod.rs:41` cfg(test) `ToolCaller` import/`context.rs`/`task_v2.rs`/`framework_adapter.rs`/`host_executor.rs`/`purge.rs`，changed-file 专项 grep 零命中）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1098 通过（0 失败、2 ignored，原 1102 − 4 删 = 1098；既有 flaky MCP 重连本轮通过）；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（未改——不动核心 trait）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 123 通过（0 失败、1 ignored——streaming re-export `MAX_STREAM_ERRORS_BEFORE_FAIL`/`MAX_TRANSPARENT_STREAM_RETRIES`/`should_transparently_retry_stream`/`ToolUseState` 仍 resolve，零回归）；`cargo +1.90.0 build --workspace` 全绿（tui 143 warning 均既有死代码，与本轮无关）。

**下一聚焦工作：**
- **剩余 dead-code（superseded，低优先）**：`layered_context_checkpoint` / `Engine::recover_context_overflow` / KoD cluster / `rx_user_input` / `tool_exec_lock` / `turn_loop::EarlyToolResult|EarlyToolTask`——均 deferred re-wire 决策点，待各自 re-wire 切片接入时一并删（保 `#[allow(dead_code)]` + doc 留存）。另有 stray `#[cfg(test)] use crate::models::ToolCaller`（`mod.rs:41`，orphan test import，11 既有 warning 之一）可随手清理但本轮未动（超出 named scope）。
- **opt-in `CapacityController`**（Gate A + seam-4 post-tool + error-escalation）：Gate A seam-1+seam-4+error-escalation 已落地（slice 33/34 §E，见下），mid-loop transcript mutations 全闭合（sub-slice 3a §E `VerifyAndReplan` + sub-slice 3b §E `VerifyWithToolReplay` + sub-slice 3c §E `TargetedContextRefresh`，见下）。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-14 §E opt-in `CapacityController` Gate A 落地，probe + observe + decide + signal（post-run application），slice 33，`feat/pluggable-framework-core`）：**

§E 的第三十三个切片落地——opt-in `CapacityController`（Gate A，off-by-default since v0.8.11）的 seam-1（pre-request）+ seam-4（post-tool）观测/决策/信号路径 + post-run 干预应用。此前 Gate A 的三个 checkpoint 方法（`run_capacity_pre_request_checkpoint` / `run_capacity_post_tool_checkpoint` / `run_capacity_error_escalation_checkpoint`）已在 `capacity_flow.rs` 完整实现（`impl Engine`），但**零生产调用方**（仅 tui `tests.rs` 测试调用）。本切片通过 Arc-share controller + `CapacityGateProbe`（executor-path 探针）+ observe/decide mid-loop + apply post-run 模式，将 Gate A 接入 `HostAgentExecutor::run_inner` 的 seam-1 / seam-4 检查点，并在 `handle_send_message` post-`run` 应用 `impl Engine` 干预级联（`apply_targeted_context_refresh` / `apply_verify_with_tool_replay` / `apply_verify_and_replan`）。**Deferred**：error-escalation（sub-slice 2——需 `step_error_count` / `consecutive_tool_error_steps` / `ErrorCategory` tracking）；mid-loop transcript mutations（sub-slice 3——替换 post-run application 为 mid-loop `ChatHistory` mutations）。

**落地步骤（11 步）：**

1. **Arc-ify `Engine.capacity_controller`**（mirror slice 22 `working_set`）：`mod.rs:170` `CapacityController` → `Arc<std::sync::Mutex<CapacityController>>`；`mod.rs:1010` `mark_turn_start` + `capacity_flow.rs` 12 处 `self.capacity_controller.<method>` → `.lock().expect("poisoned").<method>()`；`tui/engine.rs:463` `CapacityController::new(...)` → `Arc::new(StdMutex::new(...))`。
2. **Extract observation helpers to `pub(crate)` free functions**（single source）：`recent_tool_call_count(messages, window)` + `recent_unique_reference_count(messages, window, tool_call_ids, working_set)` → `capacity_flow.rs`；Engine's 方法变 thin wrapper。
3. **`CapacityGateProbe`** struct + methods（mirror `CompactionProbe` / `CapacityProbe` / `TurnMetaProbe`）：`controller: Arc<Mutex<CapacityController>>` + `model` + `workspace` + `working_set` + `profile_window` + `turn_index`；`observe_pre_turn` / `observe_post_tool` / `decide` / `mark_intervention_applied` / `last_snapshot` — 全 lock + deref。
4. **Executor field + builder + slot**（mirror `take_pending_compaction_summary`）：`capacity_gate: Option<CapacityGateProbe>` + `with_capacity_gate(Option)` + `pending_capacity_decision: Mutex<Option<CapacityDecision>>` + `take_pending_capacity_decision()` one-shot drain。
5. **Track `tool_call_ids_this_run`** in `run_inner`：`Vec<String>` near `step`；`extend` after `tool_uses` extraction。
6. **Wire seam 1**（pre-request，after Gate B preflight，before LSP flush）：`observe_pre_turn` → `decide` → if non-`NoIntervention`：`mark_intervention_applied` + set slot + `emit_status`。
7. **Wire seam 4**（post-tool，after `flush_pending_steers`，before cancel check）：同 seam 1 但 `observe_post_tool`；更新 seam-4 注释 "still to come" → "absorbed ✅ (slice 33 §E, post-run application)"。
8. **Host post-run application**（`mod.rs`，after `take_pending_compaction_summary` + `take_pending_post_compact_cleanup` block）：`take_pending_capacity_decision()` → match `action` → `apply_targeted_context_refresh` / `apply_verify_with_tool_replay` / `apply_verify_and_replan`（`&mut self.session` back in host hands）。
9. **Wire-in `CapacityGateProbe` construction**（`mod.rs`，before `.with_tool_dispatcher`）：`CapacityGateProbe::new(Arc::clone(&self.capacity_controller), model, workspace, Arc::clone(&self.session.working_set), profile_window, turn_counter)` → `.with_capacity_gate(Some(probe))`。
10. **Drive-by**：删 stray `#[cfg(test)] use crate::models::ToolCaller`（`mod.rs:41`，orphan import——slice 32 ROADMAP 提到但未动）。
11. **6 个新测试**（`host_executor.rs` test module，"§E slice 33 — opt-in CapacityController Gate A" 组）：`gate_a_disabled_is_noop` / `gate_a_pre_request_observes_and_decides` / `gate_a_post_tool_observes_and_decides` / `gate_a_mark_prevents_double_intervention` / `gate_a_none_probe_is_noop` / `gate_a_emits_status_on_decision`。

**Deadlock fix（Step 1 后发现）**：`run_capacity_error_escalation_checkpoint` 的 `last_snapshot().cloned().or_else(|| lock().observe_pre_turn(...))` 模式在 Arc-ify 后变成死锁——第一个 `lock()` 的 guard 在 `or_else` 闭包内第二个 `lock()` 时仍存活（`std::sync::Mutex` 不可重入）。拆为两个 `let` 语句释放第一个 guard 后再 `or_else`。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime`（lib）**零 warning**（Step 10 删 stray `ToolCaller` import 后 lib 零 warning）；`cargo +1.90.0 build -p codesmith-tui` 零新 warning（143 均既有死代码）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 125 通过（119 既有 + 6 新 gate_a）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1103 通过 + 1 flaky（`mcp::tests::streamable_http_stale_session_reconnects_and_retries_tool_call`，隔离运行通过）+ 2 ignored（合计 1106）；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（未改）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 123 通过（Arc-ify + deadlock fix 后零回归）；`cargo +1.90.0 build --workspace` 全绿。

**Known by-design gaps（deferred）：**
- **Error escalation**（sub-slice 2）：需 `step_error_count` / `consecutive_tool_error_steps` / `ErrorCategory` tracking in `run_inner`。
- **Mid-loop transcript mutations**（sub-slice 3）：替换 post-run `apply_*` 为 mid-loop `ChatHistory` mutations（模型在同一 turn 内看到干预）。
- **Post-run timing**：干预在 `run` 返回后应用，下一 turn 可见（非同 turn）——behavior-equivalent 因 executor's system prompt 是 static snapshot（同 slice 25a/25c 论证）。

---

**进度（2026-07-14 §E opt-in `CapacityController` Gate A error-escalation 落地，probe + track + decide + signal（post-run application），slice 34，`feat/pluggable-framework-core`）：**

§E 的第三十四个切片落地——Gate A 的第三个 checkpoint（error-escalation）接入 `HostAgentExecutor::run_inner`。slice 33 落地 seam-1/seam-4 后将 error-escalation 标记为 "sub-slice 2: needs `step_error_count` / `consecutive_tool_error_steps` / `ErrorCategory` tracking in `run_inner`"。本切片闭合该缺口：模式镜像 slice 33（observe+decide+signal；host applies post-`run`）。Mid-loop transcript mutations 仍 deferred（sub-slice 3）。

**关键设计洞察**：controller 的 per-turn cooldown（`intervention_applied_turn`，由 seam 1/4 的 `mark_intervention_applied` 设置）**天然阻断** error-escalation——当更早的 checkpoint 已干预时，`decide` 在 cooldown check（`capacity.rs:228`）返回 `NoIntervention`，先于 `decide_policy`。这镜像生产的 "seam 4 fires → `continue` → error-escalation skipped"。**无需显式 `seam4_intervened` guard**；cooldown 在一个 turn 内强制 mutual exclusion。

**落地步骤（4 步）：**

1. **`capacity_flow.rs` — `CapacityGateProbe::decide_error_escalation`**：新 probe 方法镜像 `Engine::run_capacity_error_escalation_checkpoint` 的 observe/force/decide。签名 `decide_error_escalation(&self, messages, step, tool_call_ids, system, step_error_count, consecutive_tool_error_steps, error_categories) -> Option<CapacityDecision>`。Early-return `None` gating（生产 332–353）；`last_snapshot().or_else(|| observe_pre_turn(...))`（clone+释放锁后再 `or_else`，无 re-entrant deadlock——slice 33 的 deadlock-fix concern 不适用 probe path）；force `RiskBand::High`+`severe=true` when `repeated_failures && !(already High+severe)`；`decide(Some(&forced))`；仅 `VerifyAndReplan` 返回 `Some`；override `decision.reason` 为 `format!("error_escalation: step_errors={}, consecutive_steps={}, categories={}", ...)`（host 的 `apply_verify_and_replan(&turn, ..., &decision.reason)` 记录）。加 `use crate::error_taxonomy::ErrorCategory;`。
2. **`host_executor.rs` — tracking + wiring**：import `use crate::error_taxonomy::{ErrorCategory, ErrorEnvelope};`；`run_inner` 顶部 `consecutive_tool_error_steps: u32`（turn-level）；per-step `step_error_count: usize` + `step_error_categories: Vec<ErrorCategory>`；tool loop `if !blocked` block 加 `Err(e)` arm → `let envelope: ErrorEnvelope = e.clone().into(); step_error_count += 1; step_error_categories.push(envelope.category);`（镜像生产 2575–2576；仅 `Err` counts，非 `Ok(!success)`——faithful）；cancel check + loop-guard halt 之后（`on_step` 之前）更新 `consecutive_tool_error_steps`（生产 2642–2645：`> 0` → `+1` else `0`）→ `gate.decide_error_escalation(...)` → `Some` 则 `mark_intervention_applied` + set `pending_capacity_decision` slot + `emit_status`（镜像 slice 33 seam-1/4 signal block）；更新 seam-4 注释 "error-escalation still to come" → "absorbed ✅ (slice 34 §E)"。
3. **`mod.rs` — 无改动**：host post-`run` block（slice 33）已 match `decision.action` 全 4 arm 并调用 `apply_verify_and_replan(&turn, mode, snapshot.as_ref(), &decision.reason)`。Error-escalation 只产 `VerifyAndReplan`，流经 unchanged。
4. **8 个新测试**（`host_executor.rs` test module）：test doubles `ErrorSpec`（`Err(ToolError::execution_failed)` → `ErrorCategory::Tool`）/ `TimeoutErrorSpec`（`Err(ToolError::Timeout)` → `ErrorCategory::Timeout`）/ `InvalidInputErrorSpec`（`Err(ToolError::InvalidInput)` → `ErrorCategory::InvalidInput`）；helper `capacity_gate_probe_high_prior`（`fallback_default = 100.0`，seam 1/4 → `NoIntervention`/Low-risk，让 error-escalation fire）。`error_escalation_disabled_is_noop` / `error_escalation_none_probe_is_noop` / `error_escalation_no_errors_is_noop` / `error_escalation_fires_after_two_consecutive_tool_errors`（headline）/ `error_escalation_skipped_transient_only` / `error_escalation_blocked_by_intervention_cooldown`（proves cooldown mutual-exclusion）/ `error_escalation_context_overflow_category_in_reason` / `decide_error_escalation_skipped_when_cooldown_set`（probe-level unit）。

**By-design gaps（deferred，documented）：**
- **Post-run timing**（sub-slice 3）：干预在 `run` 返回后应用 → 下一 turn 可见（非同 turn）。Behavior-equivalent 因 executor's system prompt 是 static snapshot（同 slice 25a/25c/33 论证）。
- **Forced snapshot not persisted**：host post-`run` retrieves `last_snapshot()`（实际 observed，非 forced High+severe）。仅 record-keeping divergence；sub-slice 3（mid-loop）会传 forced snapshot。
- **Reason override**：`decision.reason` overridden 为 escalation format（生产用单独 `apply_verify_and_replan` arg）；流经 slice 33 既有 `&decision.reason` plumbing。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime`（lib）**零 warning**；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 133 通过（125 既有 + 8 新 error_escalation）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1112 通过 + 2 ignored；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（未改）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 123 通过（host wire-in 零回归）；`cargo +1.90.0 build --workspace` 全绿。

---

**进度（2026-07-14 §E `VerifyAndReplan` transcript reset mid-loop 落地，CapacityController sub-slice 3a，`feat/pluggable-framework-core`）：**

§E 的第三十五个切片落地——关闭 slice 33/34 反复标记的 "mid-loop transcript mutations（sub-slice 3）" 缺口的第一部分。slice 33/34 把 `CapacityController`（Gate A）的 seam-1/seam-4/error-escalation 接入 `HostAgentExecutor::run_inner`，但干预级联（`apply_*`）在 `run` 返回后由 host 应用——模型同一 turn 看不到干预，只在下一 turn 看到。本切片把 `VerifyAndReplan` 的**transcript 部分**（`clear` + `push(latest_user)` + `push(latest_verified)`）前移到 mid-loop：执行器在决策 fire 时经 `ChatHistory::clear`/`push`（`SessionChatHistory` 委托 `session.messages`，故是原地变更 host 的 transcript）重置 transcript，然后 `step += 1; continue;`，模型在**同一 turn**从干净基线 `{latest_user, latest_verified}` replan。`VerifyAndReplan` 是 sub-slice 3 三个 action 中最自包含的（纯 `ChatHistory` `clear`+`push`，不需 LLM/tool）；`VerifyWithToolReplay`（3b，需 mid-loop 工具重放）与 `TargetedContextRefresh`（3c，需 mid-loop LLM compaction + reinject）仍 deferred。

**关键正确性不变量**：mid-loop 重置后 turn 继续（模型 replan，transcript 增长）。若 post-`run` 的 `apply_verify_and_replan` 再次 `clear`+`push`，会**抹掉模型的 post-reset replanning 成果**（并可能误识 `latest_user`，如 steer 落入）。故 post-`run` 必须**跳过 transcript 重置**但保留**state work**（canonical persist / `merge_compaction_summary` / `refresh_system_prompt` / `emit_session_updated` / `mark_intervention_applied`）。不变量：`pending_capacity_decision == Some(VerifyAndReplan)` ⟹ mid-loop 重置已执行（slot 只由带 `capacity_gate` 的 seam 设置，本切片每个 `VerifyAndReplan` 决策都走 mid-loop 重置）。故 post-`run` VerifyAndReplan 臂传 `skip_transcript = true`。

**落地步骤（3 文件 + 测试）：**

1. **`capacity_flow.rs` — 提取 + 签名扩展**：新 `pub(crate) fn latest_user_and_verified(messages) -> (Option<Message>, Option<Message>)`（factored out 自 `apply_verify_and_replan` 内联的 latest_user/latest_verified 提取——last user `Text` msg + last user msg whose `ToolResult` content 含 `[verification replay]`）；新 `pub(crate) fn reset_history_to_latest_user_and_verified(history: &mut dyn ChatHistory)`（调 `latest_user_and_verified` + `history.clear()` + `history.push(...)`——`ChatHistory` 面，执行器 mid-loop 用）。`apply_verify_and_replan` 加 `skip_transcript: bool` 参：用 `latest_user_and_verified` 提取（行为保持 dedup），`skip_transcript == true` 时**跳过** `clear`+`push` 块，state work（canonical/persist/merge/refresh/emit/mark）总跑。两处 dead-code 调用点（`run_capacity_post_tool_checkpoint` / `run_capacity_error_escalation_checkpoint`，tui-test-only）传 `false`（faithful 旧生产路径）。加 `use codesmith_agent::memory::ChatHistory;`。
2. **`host_executor.rs` — mid-loop 接线**：seam-4（post-tool）`decision.action != NoIntervention` 块——当 `decision.action == VerifyAndReplan`，调 `reset_history_to_latest_user_and_verified(history)` + `step += 1; continue;`（跳过 (4) per-step seam + error-escalation + `on_step`；loop-top cancel gate 下一迭代兜底，cooldown 阻断 error-escalation——镜像生产 `turn_loop.rs:2628` + 既有 steer `step += 1; continue;` 惯用法）；其他 action（`VerifyWithToolReplay`/`TargetedContextRefresh`）保持原行为（set slot + emit_status，fall through——post-`run` 应用）。所有情况下 slot 仍设，post-`run` state work 跑。error-escalation（`decide_error_escalation` 返 `Some`——恒 `VerifyAndReplan`）同样调 `reset_history_to_latest_user_and_verified(history)` + `step += 1; continue;`（镜像 `turn_loop.rs:2658`）。seam-4 / error-escalation 模块注释更新 "post-run application" → "transcript reset mid-loop (slice 3a §E); state work still post-run"。import 加 `reset_history_to_latest_user_and_verified`。
3. **`mod.rs` — post-`run` 臂**：`VerifyAndReplan` 臂（slice 33 既有 match 块）传 `skip_transcript = true` + 注释不变量。其他臂（`TargetedContextRefresh`/`VerifyWithToolReplay`）不动。

**4 个新测试：**
- `host_executor.rs` test module（"§E slice 3a — VerifyAndReplan mid-loop transcript reset" 组）+ `has_tool_blocks` / `has_role_text` helpers：
  - `verify_and_replan_resets_transcript_mid_loop`（headline——两轮 `fail_tool`（ErrorSpec → `ErrorCategory::Tool`）force High+severe → VerifyAndReplan；mid-loop 重置抹掉两轮 tool turns，只剩 `latest_user`("hello") + 模型 post-reset replan("done")；断言无 tool blocks + "hello" 存活 + "done" present + slot=VerifyAndReplan）。
  - `verify_and_replan_disabled_is_noop`（disabled gate → 无重置，tool blocks 完整 + slot None）。
  - `verify_and_replan_none_probe_is_noop`（无 `.with_capacity_gate` → 无重置 + slot None）。
- `crates/tui/src/core/engine/tests.rs`（`build_engine_with_capacity` harness）：
  - `apply_verify_and_replan_skip_transcript_preserves_messages`（grown transcript 代表模型 post-reset 增长；调 `apply_verify_and_replan(..., skip_transcript=true)`；断言 return `true` + transcript **不变**（len + content）——证明 post-`run` 不抹增长）。

**By-design gaps（deferred，documented）：**
- **State work 仍 post-`run`**（system_prompt / canonical-state / persistence / emit）——执行器 mid-loop 无 `&mut self.session`；behavior-equivalent 因 system prompt 是 static snapshot（slice 25a/25c/33/34 论证）。
- **`skip_transcript` 按 action 键控**（非独立 slot）——依赖上述不变量；干净且免新 executor 字段。
- **canonical state divergence**：post-`run` 从 grown transcript 建 canonical state（生产 mid-loop 从 reset 后 transcript 建）——匹配 slice 34 既有 "Forced snapshot not persisted" gap。
- **3b/3c deferred**：`VerifyWithToolReplay`（mid-loop 工具重放，需 `tool_dispatcher` + lock/mcp plumbing）与 `TargetedContextRefresh`（mid-loop LLM compaction + reinject）——同 `skip_transcript` 模式延展。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime`（lib）**零 warning**；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 136 通过（133 既有 + 3 新 slice 3a）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1115 通过 + 2 ignored（1112 + 3）；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（未改）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 124 通过 + 1 ignored（123 + 1 新 skip_transcript，host wire-in 零回归）；`cargo +1.90.0 build --workspace` 全绿（tui bin 143 既有死代码 warning，无新增）。

**下一聚焦工作：**
- **sub-slice 3c**：`TargetedContextRefresh` transcript 部分 mid-loop（需 mid-loop `compact_messages_safe` LLM compaction + reinject——最 invasive；executor 有 LLM client）。sub-slice 3 的最后一部分。
- **3c 落地后**：sub-slice 3 完成，mid-loop transcript mutations 全闭合。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先。

---

**进度（2026-07-14 §E `VerifyWithToolReplay` transcript portion mid-loop 落地，CapacityController sub-slice 3b，`feat/pluggable-framework-core`）：**

§E 的第三十六个切片落地——关闭 slice 33/34 反复标记的 "mid-loop transcript mutations（sub-slice 3）" 缺口的第二部分（slice 35 / 3a 落地了 `VerifyAndReplan`）。`VerifyWithToolReplay` 此前把整个级联（candidate select → 工具重放 → `[verification replay]` note 构建 → push → state work）defer 到 post-`run`——模型同一 turn 看不到 note，只在下一 turn 看到。本切片把 `VerifyWithToolReplay` 的**transcript 部分**（select candidate → re-execute tool → build `[verification replay]` note → push via `ChatHistory`）前移到 mid-loop；**state work**（canonical persist / system-prompt fold / emit / `mark_intervention_applied` / `mark_replay_failed`）仍 post-`run`，用新 `skip_transcript` flag + carried `ReplayOutcome`——同 3a 的 split philosophy。

**关键不对称（vs 3a，决定设计）：**
- **生产镜像**（`Engine::run_capacity_post_tool_checkpoint`）：`VerifyWithToolReplay` 调全 `apply_verify_with_tool_replay` mid-loop 后返 **`false`** → 外层 loop **不** `next_step(); continue;`，fall through 到正常 step advance（contrast `VerifyAndReplan` 返 `true` → `next_step(); continue;`）。故执行器 3b 臂**不** `step += 1; continue;`——这是与 3a 臂的关键差异。
- **state work 依赖 replay outcome**：3a 的 state work outcome-independent（不需 reset 的输出）；3b 的 state work（canonical note "passed/failed"、`ReplayInfo{tool_id,tool_name,pass,diff_summary}`、`verification_note`、emit label）**需要** transcript 部分产的 `{pass, diff_summary, candidate.id, candidate.name, verification_note}`。故 outcome 须从 mid-loop（执行器）handoff 到 post-`run`（host）→ 新 executor slot `pending_replay_outcome`。

**重放路径（唯一 fork）：** mid-loop replay 经 **`tool_dispatcher.execute(name, input, None)`** 重放 candidate——执行器 `tool_dispatcher: Option<Arc<dyn ToolDispatcher>>` 字段（host `mod.rs:1289 .with_tool_dispatcher(plan.tool_registry.clone())` 接入）。这是 legacy `apply_verify_with_tool_replay` 在 `execute_tool_with_lock` 的 `ToolDispatcher::execute` 分支内用的**同一 dispatch 面**，减去 `ToolExecGuard` lock + `mcp_pool`。匹配 ROADMAP 提示（"执行器有 `tool_dispatcher`"）。无新 executor 字段接 lock/mcp。

**落地步骤（3 文件 + 测试）：**

1. **`capacity_flow.rs` — 新类型 + free fns + 签名扩展**：新 `pub struct ReplayOutcome { tool_id, tool_name, pass, replay_outcome, diff_summary, verification_note }`（`#[derive(Clone, Debug)]`，live in public `capacity` module——出现在 `pub` 签名 `apply_verify_with_tool_replay` / `take_pending_replay_outcome` 且 tui engine 测试直接构造）。新 `pub(crate) struct ReplayCandidate { id, name, input, original_result }`。新 `pub(crate) fn is_replayable_read_only(tool_name, tool_registry) -> bool`（factored 自 `Engine::tool_is_replayable_read_only`，body 不读 `self`；method 改为 delegate，行为保持）。新 `pub(crate) fn select_replay_candidate_from_messages(messages, tool_registry) -> Option<ReplayCandidate>`（`&[Message]` 版 `Engine::select_replay_candidate`——后者需 `turn: &TurnContext` 执行器不持有；逆序扫 assistant `ToolUse{id,name,input}` whose matching user `ToolResult{tool_use_id==id}` 有 `is_error != Some(true)` AND `is_replayable_read_only`）。新 `pub(crate) async fn replay_and_push_verification_note(history: &mut dyn ChatHistory, tool_registry) -> Option<ReplayOutcome>`（mid-loop transcript mutation，镜像 3a 的 `reset_history_to_latest_user_and_verified`：select → `tool_registry.execute` → 算 `(pass, replay_outcome, diff_summary)`（pure，复用 `summarize_text`）→ 构建 `verification_note` → `history.push(ToolResult{tool_use_id, content: verification_note, is_error: None})` → 返 `Some(ReplayOutcome{...})`；**不**调 `mark_replay_failed`——state work，post-`run`）。`apply_verify_with_tool_replay` 加尾参 `skip_transcript: bool, outcome: Option<ReplayOutcome>`：`skip_transcript==true` 时若 `outcome.is_none()` 返 `false`（no-op），else **跳过** candidate select / MCP/parallel/interactive flags / re-execute / pass-fail / note push，用 `outcome` 跑 state work（canonical note from `outcome.pass`、`ReplayInfo` from outcome、`verification_note` from outcome、`replay_outcome` label for emit）；`!pass` 时仍 `mark_replay_failed`（state）。dead-code 调用点 `run_capacity_post_tool_checkpoint` 的 `VerifyWithToolReplay` 臂传 `false, None`（faithful 旧路径）。不变量注释（镜像 3a 的 `VerifyAndReplan` 注释）。
2. **`host_executor.rs` — slot + accessor + seam-4 臂**：新字段 `pending_replay_outcome: std::sync::Mutex<Option<ReplayOutcome>>`（init `Mutex::new(None)`，镜像 `pending_capacity_decision`）。新 `#[must_use] pub fn take_pending_replay_outcome(&self) -> Option<ReplayOutcome>`（镜像 `take_pending_capacity_decision`）。seam-4（`run_inner`，`decision.action != NoIntervention` 块）在既有 `if VerifyAndReplan {...; step += 1; continue;}` 后加 `else if VerifyWithToolReplay { let outcome = replay_and_push_verification_note(history, self.tool_dispatcher.as_deref()).await; *self.pending_replay_outcome.lock()... = outcome; }`——**无** `step += 1; continue;`（生产返 `false`，fall through）；slot 仍设，post-`run` state work 跑 `skip_transcript = true`。import 加 `replay_and_push_verification_note` + `ReplayOutcome`。seam-4 注释更新 "VerifyWithToolReplay deferred" → "transcript portion mid-loop (slice 3b §E)"。
3. **`mod.rs` — post-`run` 臂**：`VerifyWithToolReplay` 臂（slice 33 既有 match 块）传 `skip_transcript = true` + `let outcome = executor.take_pending_replay_outcome();` + 注释不变量（mid-loop 重放+push 已执行，post-`run` 只跑 state work，**不** re-execute+re-push 否则 double-inject note）。

**4 个新测试：**
- `host_executor.rs` test module（"§E slice 3b — VerifyWithToolReplay mid-loop" 组）：
  - `replay_and_push_verification_note_pushes_note_and_outcome`（headline——**直接测 free fn**（seam-4 臂的 body）；`EchoSpec`（read-only，deterministic）注册为 `ToolRegistry`，既作 `tool_dispatcher` 又经 `as_ref()` 传给 free fn；seed transcript 含 echo tool_use + matching 成功 tool_result（content = `{workspace}|hi`，匹配 EchoSpec 重放输出 → `pass=true`）；断言 note pushed（`[verification replay]` + `tool=echo` + `pass=true`、`tool_use_id == "e1"`、`is_error == None`）+ `ReplayOutcome{tool_id:"e1", tool_name:"echo", pass:true, replay_outcome:"pass", diff_summary:"output_match", verification_note==note}`）。**为何不测 executor full-run**：seam-1（pre-turn）与 seam-4（post-tool）共享同一 `turn_index` + 同一 `decide` cooldown——任何 yield `VerifyWithToolReplay` 的 config 下 pre-turn seam-1 先 fire 设 cooldown → seam-4 返 `NoIntervention`（无 candidate → 无 note）。full-run 正测**结构性阻断**（非仅数值 fragile）；free fn 是 seam-4 臂的 body，直接测覆盖同一逻辑。
  - `verify_with_tool_replay_disabled_is_noop`（disabled probe → 无 note + `take_pending_replay_outcome() == None`）。
  - `verify_with_tool_replay_none_probe_is_noop`（无 `.with_capacity_gate` → slot `None`；镜像 `gate_a_none_probe_is_noop` for replay-outcome slot）。
- `crates/tui/src/core/engine/tests.rs`（`build_engine_with_capacity` harness）：
  - `apply_verify_with_tool_replay_skip_transcript_uses_outcome`（grown transcript 含既有 `[verification replay]` ToolResult（模拟 executor mid-loop push）+ 模型 post-replay assistant turn；构造 `ReplayOutcome{pass:true,...}`；调 `apply_verify_with_tool_replay(..., skip_transcript=true, Some(outcome))`；断言 return `true`（state work 跑）+ transcript **不变**（len + content）+ 恰好 1 个 replay note（无 double-push）——证明 `skip_transcript=true` 不 re-wipe/re-push 同时 state work 仍跑；镜像 3a 的 `apply_verify_and_replan_skip_transcript_preserves_messages`）。

**By-design gaps（deferred，documented）：**
- **State work 仍 post-`run`**（executor 无 `&mut self.session` for canonical persist / system-prompt fold）——同 3a。
- **Outcome handoff 经新 `pending_replay_outcome` slot**——3a state work outcome-independent；3b 不是，故 replay outcome carried 到 post-`run`。
- **MCP replay candidates degrade**：`tool_dispatcher.execute` 不处理 MCP 工具（mid-loop 无 `mcp_pool`）→ `NotAvailable` → `replay_error` → `pass=false`。legacy 经 `mcp_pool` 处理 MCP。（lock/mcp plumbing deferred，匹配 3a 模式。）
- **mid-loop 无 `ToolExecGuard` 序列化**——一致于执行器 normal tool loop（用 `Tool::run` 不上 lock）；replay 用 `tool_dispatcher.execute` 直接。
- **`mark_replay_failed` 仍 post-`run`**——`CapacityGateProbe` 不暴露它；state work 本来就是。
- **`before/after_tokens` emit delta 不反映 replay 的 token 影响**（replay 已 mid-loop 应用）——minor divergence，匹配 3a 的 "canonical state divergence" gap。
- **`mark_intervention_applied` 在 mid-loop replay 前 fire**（既有 seam-4 order）——若 replay 找无 candidate（outcome `None`），cooldown 仍设；acceptable（本 turn 阻断 re-intervention）。documented。
- **executor full-run 正测结构性阻断**（见上）——seam-1 先 fire 阻 seam-4；free fn 直测覆盖 logic，disabled/None 测覆盖 wiring。
- **3c（`TargetedContextRefresh`）仍 deferred**——需 mid-loop LLM compaction + reinject。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime`（lib）**零 warning**；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 139 通过（136 + 3 新 slice 3b）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1117 通过 + 1 flaky（`mcp::tests::streamable_http_stale_session_reconnects_and_retries_tool_call`——网络时序，隔离重跑通过，与本切片无关）+ 2 ignored（1115 + 3 新，host wire-in 零回归）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 125 通过 + 1 ignored（124 + 1 新 skip_transcript）；`cargo +1.90.0 build --workspace` 全绿（tui bin 143 既有死代码 warning，无新增）。

---

**进度（2026-07-15 §E `TargetedContextRefresh` transcript portion mid-loop 落地，CapacityController sub-slice 3c，`feat/pluggable-framework-core`）：**

§E 的第三十七个切片落地——关闭 slice 33/34 反复标记的 "mid-loop transcript mutations（sub-slice 3）" 缺口的最后一部分（slice 35/3a 落地了 `VerifyAndReplan`，slice 36/3b 落地了 `VerifyWithToolReplay`）。`TargetedContextRefresh` 此前把整个级联（`should_compact` → `compact_messages_safe` → `reinject_compaction_attachments` → local-trim fallback → state work）defer 到 post-`run`——模型同一 turn 看不到 compacted transcript，只在下一 turn 看到。本切片把 `TargetedContextRefresh` 的**transcript 部分**（LLM compaction + reinject + local-trim fallback）前移到 **seam-1（pre-request）**；**state work**（canonical persist / system-prompt fold / emit / `mark_intervention_applied`）仍 post-`run`，用新 `skip_transcript` flag + carried `TargetedRefreshOutcome`——同 3a/3b 的 split philosophy。**sub-slice 3 至此全闭合**：三个 action 的 transcript mutation 均已 mid-loop。

**关键设计（vs 3a/3b，决定 placement）：**
- **placement: seam-1（pre-request）only**：`TargetedContextRefresh` 是 `decide_policy` 的 Medium-risk action，生产里 retired `run_capacity_pre_request_checkpoint` 在 pre-request 应用它（post-tool checkpoint 显式 no-op 它）。故 mid-loop transcript 部分在 seam-1 fire，模型在**同一 step**的 request 看到 compacted transcript。seam-4 若 fire（rare，risk mid-turn 增长，seam-1 当时 Low）→ fall through 到 post-`run` full cascade（outcome `None` → `skip_transcript=false`）。seam-1 设的 cooldown 阻 seam-4。
- **`&self` 方法（非 free fn）**：transcript 部分需 `self.record_compaction_summary`（25a slot）/ `self.reinject_compaction_attachments`（25b）/ `self.emit_status`——镜像 `run_compaction(&self, client, history)` 先例。
- **fallback scope: full transcript 部分 mid-loop**：mid-loop 跑 `compact_messages_safe`（带 working-set pins/paths，`enhancements=None`——需 Engine state 不可 mid-loop，同 `run_compaction`）AND local-trim fallback。新 `pub(crate) fn trim_oldest_messages_to_budget_history`（`&mut dyn ChatHistory` 版 `Engine::trim_oldest_messages_to_budget`）。outcome 总 `Some({refreshed, before_tokens})`；post-`run` 只跑 state work（`skip_transcript=true`）——无冗余 LLM retry。

**落地步骤（4 文件 + 6 测试）：**

1. **`capacity.rs` — 新类型**：新 `pub struct TargetedRefreshOutcome { refreshed: bool, before_tokens: usize }`（`#[derive(Debug, Clone)]`，live in public `capacity` module——出现在 `pub` 签名 `apply_targeted_context_refresh` / `take_pending_targeted_refresh_outcome` 且 tui engine 测试直接构造）。`before_tokens` 在 transcript 部分顶部捕获（任何 mutation 前）；post-`run` state work 用它作 `emit_capacity_intervention` telemetry delta（`after_tokens` 现算）。`refreshed=false` 时 post-`run` cascade 返 `false`（无 state work），匹配 `apply_targeted_context_refresh` 的 `if !refreshed { return false; }`。
2. **`capacity_flow.rs` — free fn + 签名扩展**：新 `pub(crate) fn trim_oldest_messages_to_budget_history(history: &mut dyn ChatHistory, system, target) -> usize`（提取自 `Engine::trim_oldest_messages_to_budget`，body 是对 `self.session.messages` 的纯循环；镜像 `run_compaction` Phase-1 的 clone → mutate → clear+repush）。`apply_targeted_context_refresh` 加尾参 `skip_transcript: bool, outcome: Option<TargetedRefreshOutcome>`（镜像 3b `apply_verify_with_tool_replay`）：`skip_transcript==true` 时若 `outcome.is_none()` 返 `false`（no-op），else `before_tokens=outcome.before_tokens`、`refreshed=outcome.refreshed`、**跳过** transcript 部分（`should_compact` → `compact_messages_safe` → reinject → local-trim），`if !refreshed { return false; }` 后跑 state work（`after_tokens` 现算）；`!skip_transcript` 时不变（full cascade）。dead-code 调用点 `run_capacity_pre_request_checkpoint` 传 `false, None`（faithful 旧路径）。`CapacityGateProbe` 加 `pub(crate) fn working_set()` + `pub(crate) fn workspace()` accessor（mid-loop transcript 部分需 working set 算 pins/paths + workspace）。
3. **`host_executor.rs` — slot + accessor + 方法 + seam-1 臂**：新字段 `pending_targeted_refresh_outcome: std::sync::Mutex<Option<TargetedRefreshOutcome>>`（init `None`，镜像 `pending_replay_outcome`）+ `#[must_use] pub fn take_pending_targeted_refresh_outcome(&self)`（one-shot drain）。新 `async fn refresh_targeted_context_mid_loop(&self, client: &LlmClientHandle, history: &mut dyn ChatHistory, system) -> Option<TargetedRefreshOutcome>`（body 镜像 `apply_targeted_context_refresh` transcript 部分：`let (Some(compaction), Some(gate)) = (&self.compaction, &self.capacity_gate) else { return None };` → `before_tokens` → lock working set 算 pins/paths → `should_compact` → `compact_messages_safe(client.as_ref(), ..., None)` → Ok: `record_compaction_summary` + `history.clear()`/`push` + `reinject_compaction_attachments` → `refreshed=true`；Err: `emit_status` → local-trim fallback → `Some(TargetedRefreshOutcome{refreshed, before_tokens})`）。seam-1（`run_inner`，`decision.action != NoIntervention` 块）在 `emit_status` 后加 `if TargetedContextRefresh { let outcome = self.refresh_targeted_context_mid_loop(&client, history, system.as_ref()).await; *self.pending_targeted_refresh_outcome.lock()... = outcome; }`——**fall through**（无 `step += 1; continue;`），request 读 `history.messages()` = compacted。seam-4 注释更新 "`TargetedContextRefresh` still defers" → "seam-1 已跑 transcript 部分（slice 3c），cooldown 阻 seam-4；若 seam-4 fire 则 fall through full cascade"。import 加 `TargetedRefreshOutcome` + `trim_oldest_messages_to_budget_history`。
4. **`mod.rs` — post-`run` 臂**：`TargetedContextRefresh` 臂（slice 33 既有 match 块）`let outcome = executor.take_pending_targeted_refresh_outcome(); let skip_transcript = outcome.is_some();` + `apply_targeted_context_refresh(&turn, client, mode, snapshot.as_ref(), skip_transcript, outcome)` + 注释不变量（mid-loop compaction 已执行，post-`run` 只跑 state work，**不** re-compact/re-reinject 否则 double-mutate transcript）。

**6 个新测试：**
- `host_executor.rs` test module（"§E slice 3c — TargetedContextRefresh mid-loop" 组，镜像 3b 的 direct-method + full-run 风格）：
  - `targeted_refresh_compacts_transcript_mid_loop`（headline——**直接测方法**：compaction + capacity_gate + reinject probe + mock 返 summary + seed 12 条 over-threshold；断言 transcript compacted（len 缩 + summary recorded）+ reinject ran（plan/todo/readfile candidate resurface）+ outcome `Some({refreshed:true, before_tokens>0})` + `compaction_calls()==1`）。
  - `targeted_refresh_local_trim_fallback_on_compaction_failure`（mock compaction error + over-budget history → local-trim fire → `refreshed=true` + messages ≤ `MIN_RECENT_MESSAGES_TO_KEEP`(4) retained）。
  - `targeted_refresh_no_refresh_when_under_budget`（high threshold + small history → `should_compact` false → `refreshed=false` + `Some({refreshed:false})` + `compaction_calls()==0`）。
  - `targeted_refresh_disabled_is_noop`（full-run，disabled gate → observe 返 None → seam-1 不 fire → `take_pending_targeted_refresh_outcome()==None` + `take_pending_capacity_decision()==None`）。
  - `targeted_refresh_none_probe_is_noop`（full-run，无 `.with_capacity_gate` → seam-1 跳过 → slot `None`）。
- `crates/tui/src/core/engine/tests.rs`（`build_engine_with_capacity` harness）：
  - `apply_targeted_context_refresh_skip_transcript_uses_outcome`（grown transcript（compacted tail + post-compaction assistant turn）+ 构造 `TargetedRefreshOutcome{refreshed:true, before_tokens:12345}`；调 `apply_targeted_context_refresh(..., skip_transcript=true, Some(outcome))`；断言 return `true`（state work 跑：canonical persist / `mark_intervention_applied`）+ transcript **不变**（len + content）——证明 `skip_transcript=true` 不 re-compact/re-reinject；镜像 3a `…skip_transcript_preserves_messages` + 3b `…skip_transcript_uses_outcome`）。

**By-design gaps（deferred，documented）：**
- **`enhancements=None` mid-loop**：`build_compaction_enhancements`（PreCompact hooks + session-memory sidecar）需 `&mut self` Engine state（`self.host.hooks()` / `self.session_memory_compaction_content()`）——mid-loop 不可达。同 3a/3b state work gap 类。auto-compact（`run_compaction`）也用 `None`。
- **State work 仍 post-`run`**——executor 无 `&mut self.session`；behavior-equivalent（system prompt 是 static snapshot——同 25a/25c/33/34/3a/3b 论证）。
- **`circuit_breaker` 不用**：`apply_targeted_context_refresh` 不经 breaker（faithful）；`last_refresh_turn` cooldown 替代。
- **seam-4 `TargetedContextRefresh` → post-`run` full cascade**（outcome `None` → `skip_transcript=false`）：超出 legacy post-tool no-op，一致于 slice 33。
- **Forced snapshot not persisted**（匹配 slice 34 gap）：post-`run` retrieves `last_snapshot()`（observed，非 forced）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime`（lib）**零 warning**；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 144 通过（139 + 5 新 slice 3c）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1122 通过 + 1 flaky（`mcp::tests::legacy_sse_closed_stream_reconnects_and_retries_tool_call`——网络时序，隔离重跑通过，与本切片无关）+ 2 ignored；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（core trait 未触）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 126 通过 + 1 ignored（125 + 1 新 skip_transcript，host wire-in 零回归）；`cargo +1.90.0 build --workspace` 全绿（tui bin 既有死代码 warning，无新增）。

---

**进度（2026-07-15 §E mid-loop system-prompt refresh（seam 1）落地，折叠 compaction summary 进 per-step 快照，slice 38，`feat/pluggable-framework-core`）：**

§E 的第三十八个切片落地——关闭 seam-1 "system-prompt refresh still to come（top of the `loop`）" 缺口（`host_executor.rs:252` 模块文档）。生产退役的 `turn_loop.rs:320` 在每步循环顶部调 `self.refresh_system_prompt(mode)`（"Ensure system prompt is up to date with latest session states"），把 `session.compaction_summary_prompt` 折进重组的 base。执行器此前在 `run_inner` 循环前**一次性**快照 `let system = self.config.system.clone();`（`:3506`）且循环内不刷新——故 mid-turn 产出的 compaction summary 只在 `run` 返回后由 host 折进 `session.system_prompt`（`mod.rs:1333` → `merge_compaction_summary`），模型**下一 turn** 才看到，非同 turn。本切片把折叠加到循环顶部（mid-loop），模型在**同一 turn 的下一步**即看到 compaction summary，闭合 slice 25a 的 "post-`run` timing" 系统提示词半。

**关键设计洞察（窄而忠实的范围）**：完整的 `Engine::refresh_system_prompt`（`mod.rs:2521`）是 `&mut self` on `Engine`，重组 base 依赖 `self.config` + `crate::memory`/`skills`/`slop_ledger`——mid-`run` 不可达（执行器是 `&self`；`&mut Engine` 被借去构造+驱动）。但 base 在**一 turn 内稳定**（host 每 turn pre-`run` 在 `mod.rs:1127` 重组一次；config/memory/skills mid-turn 不变），故 mid-turn 唯一变化的输入是累积的 compaction summary。执行器侧 analog：把自己的 `pending_compaction_summary` slot（本 turn 至今的 compaction）折进 per-step 快照。**无新字段、无新 probe、无 host 改动**——纯执行器内部，复用 slice 25a 的 `pending_compaction_summary` slot + `crate::compaction::merge_system_prompts`。

**无双重折叠不变量**：`base` 是循环内每步的 fresh 稳定 local（从不 mutate），slot 只累积 summary（`record_compaction_summary` 的 `merge`），故每步 `merge(base, peek(cumulative))` 重算 `base + cumulative`，永不 `base + cumulative + cumulative`。peek **非排空**（`clone` 不 `take`），故 host post-`run` 的 `take_pending_compaction_summary` + `merge_compaction_summary`（`mod.rs:1333-1338`）不变。

**落地步骤（4 步）：**

1. **`host_executor.rs` — `peek_pending_compaction_summary`**：新私有方法镜像 `take_pending_compaction_summary`（`:1708`）但 `clone()`（非排空）。doc 注明背书 mid-loop refresh 且 post-`run` `take` 排空不变。
2. **`host_executor.rs` — `refresh_system_prompt_snapshot`**：新私有方法 `fn refresh_system_prompt_snapshot(&self, base: Option<&SystemPrompt>) -> Option<SystemPrompt>` → `merge_system_prompts(base, self.peek_pending_compaction_summary())`。doc 关联生产 `turn_loop.rs:320` + 稳定-base 论证 + "下一步看到 summary" 序。
3. **`host_executor.rs` — 接 seam-1（`run_inner`）**：`let system = self.config.system.clone();`（`:3506`，循环前）→ `let base = self.config.system.clone();`（turn 稳定 base）；循环内 `drain_steers`（`:3580`）之后、`run_compaction`（`:3585`）之前加 `let system = self.refresh_system_prompt_snapshot(base.as_ref());`——镜像生产 steer→refresh→compaction 序，故 step N 的 request 用 step N-1 的 compaction summary（refresh-before-compaction：模型在 summary 产出的**下一步**看到，非同步）。下游 `run_capacity_preflight` / `gate.observe_pre_turn` / request（`:3597`/`:3631`/`:3685`）均读该 per-step `system`。`continue` 路径（`CapacityPreflight::RetryStep`、reactive `recover_context_overflow` 重启）自然重跑 refresh；reactive summary 落 slot 后在重启时折入。slice 3c 的 `refresh_targeted_context_mid_loop` 不动——若其记 summary，下一迭代 refresh 折入。
4. **模块文档 + MockLlm test infra + 6 测试**：seam-1 行 "system-prompt refresh still to come（top of the `loop`）" → "✅ system-prompt refresh（折累积 compaction summary 进 per-step 快照，镜像生产 per-step `Engine::refresh_system_prompt` 折 `session.compaction_summary_prompt`——slice 38 §E）"；新增 "Known gaps in the system-prompt refresh (by design)" 小节（base 重组 host-side、mid-turn slop-ledger/memory/skills 落盘变化不反映）。`MockLlm` 加 `systems: Mutex<Vec<Option<SystemPrompt>>>` + `systems()` accessor（镜像既有 `requests`/`requests()`），`create_message_stream` push `request.system.clone()`。6 测试（"§E slice 38 — mid-loop system-prompt refresh" 组）：`system_prompt_refresh_folds_compaction_summary_next_step`（headline：step 0 compact → step 1 的 request system 含 summary、step 0 不含；refresh-before-compaction 序）、`system_prompt_refresh_no_summary_is_base_snapshot`（无 compaction → 各步 system == base）、`system_prompt_refresh_peek_does_not_drain_slot`（mid-loop peek 后 `take_pending_compaction_summary()` 仍 `Some`——host fold 不饿死）、`system_prompt_refresh_accumulates_multiple_summaries`（两条 summary 都折入，非 last-wins）、`system_prompt_refresh_first_step_uses_construction_snapshot`（step 0 system == 构造快照）、`system_prompt_refresh_no_double_fold`（连续两次 refresh 同一 cumulative，summary 恰好出现一次——守 fresh-base-per-step 不变量）。

**By-design gaps（deferred，documented）：**
- **base 重组是 host-side**：完整 `Engine::refresh_system_prompt` 重组 base（含 SlopLedger completion-gate 块）是 `&mut Engine`，mid-`run` 不可达。本切片只折 compaction summary（mid-turn 唯一变化输入），收窄 slice 25a 的静态快照论证为"base 静态；summary mid-loop 折入"，闭合 same-turn 可见性缺口。
- **mid-turn slop-ledger / memory / skills 落盘变化不反映**：mid-turn 写 slop ledger 的工具不会在本 turn 浮现其 completion-gate 块（base 是 per-turn 快照），下次 pre-`run` 重组才看到。niche 且一致于稳定-base 假设；未来可用 resolver-closure probe（类 `TurnMetaProbe`）重新调 `Engine::refresh_system_prompt`。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime`（lib）**零 warning**；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 11 均既有 warning（零新增——本切片初稿 4 个 `unused history` 经删除未用 sess/history 消除）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 150 通过（144 slice 3c + 6 新 slice 38）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1128 通过 + 1 flaky（`mcp::tests::streamable_http_stale_session_reconnects_and_retries_tool_call`——网络时序，隔离重跑通过，与本切片无关）+ 2 ignored；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（core trait 未触）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 126 通过 + 1 ignored（host wire-in 零回归）；`cargo +1.90.0 build --workspace` 全绿（tui bin 143 warning 均既有死代码，无新增）。

**下一聚焦工作：**
- seam-1 至此全闭合（steer / compaction / capacity preflight / LSP flush / system-prompt refresh）。seam-2 剩 **thinking-only handling**（stream 解析后、tool 抽取/turn 结束前）、seam-3 剩 **parallel dispatch**（tool `for` 循环内）——二者是模块文档 "still to come" 的余项。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先；DeepSeek `client.rs`/`chat.rs` 残件删除（blocked on cache-warmup/debug-inspect 迁出 tui）维持。

---

**进度（2026-07-15 §E thinking-only handling（seam 2）落地，issue #1727 的 thinking-only 守卫吸收进 `HostAgentExecutor`，slice 39，`feat/pluggable-framework-core`）：**

§E 的第三十九个切片落地——关闭 seam-2 "thinking-only handling still to come（after the stream resolves, before tool extraction / turn end）" 缺口（`host_executor.rs:271-273` 模块文档）。生产退役的 `handle_deepseek_turn`（`turn_loop.rs:1267-1293` + `:1549-1567` + pure helper `:2893-2901`，issue #1727）在 stream 解析后、`tool_uses.is_empty()` 尾巴前处理 "thinking-only" 回合：模型只产 `Thinking` block（无 `Text`、无 `ToolUse`，如 gpt-oss via ollama 的 harmony→OpenAI shim 映射到 `reasoning_content`）时，**不持久化**该 thinking-only assistant 消息（DeepSeek chat API 拒收只含 thinking block 的 assistant 消息），并在**干净回合尾**发一条 `Event::status("Model returned reasoning but no answer or tool call; turn ended without output. Send a follow-up to retry.")`。执行器此前在 `run_inner` 的 `callback.on_llm_end` 之后**无条件** `history.push(assistant)`（`:3877-3880`）、不算 thinking-only flag、在 `NoToolCalls` 尾巴直接 `on_complete` 无 status。本切片把生产守卫吸收：算 flag、guard 持久化、在干净尾发 status，闭合 slice 20 以来 seam-2 反复标记的 "thinking-only handling still to come"。

**关键设计洞察（deferred-decide）**：生产在持久化点（`turn_loop.rs:1283`）**capture** flag 但 **defer** 到 `tool_uses.is_empty()` 尾巴才 **decide**——中间还有 steer flush / subagent drain / goal-continuation / REPL 等 resume 分支；若在 capture 点即发 status，一个马上要 resume 的回合会先看到一条虚假 "turn ended" 提示。执行器侧同序：flag 在持久化前算（`on_llm_end` 后），status 在 no-tool-calls 尾巴发——steer flush（`flushed > 0` → `continue`）与 subagent blocking-hold（completion arm → `continue`；cancel arm → `return Interrupted`；steer arm → `continue`）均先于尾巴，故 resume 时尾巴不到、status 不发——faithful。

**干净尾守卫**：`should_emit_thinking_only_status(tool_uses_empty, turn_error_is_none, cancelled, steers_pending, holding_for_subagents)` 镜像生产 pure helper（`turn_loop.rs:2893-2901`）签名一字不差。到尾巴时四参平凡真：`tool_uses_empty`（在 no-tool-calls arm）、`turn_error_is_none`（此路径无 error）、`steers_pending`（`flush_pending_steers` 已排空 `pending_steers` → `!pending_steers.is_empty()` = false）、`holding_for_subagents`（未进 blocking-hold，否则已 `continue`/`return`）——唯一 live check 是 `self.is_cancelled()`（取消 status 已覆盖）。flag = `!has_sendable_assistant_content`，其中 `has_sendable = content.iter().any(Text|ToolUse)`——**覆盖空 content**（生产注释明文 "empty content, no tool calls"）：一个干净空 stream（`MockRound::Events(vec![])`）也是 `!has_sendable` → 不持久化空 assistant + 发 status，faithful。

**落地步骤（4 步）：**

1. **`host_executor.rs` — `should_emit_thinking_only_status` free fn**：新私有 free fn（置 `should_hold_turn_for_subagents` 后），签名/实现镜像 `turn_loop.rs:2893-2901`：`tool_uses_empty && turn_error_is_none && !cancelled && !steers_pending && !holding_for_subagents`。doc 关联 issue #1727 + deferred-decide + 指向 `run_inner` 落地点。
2. **`host_executor.rs` — 算 flag + guard 持久化（`run_inner`）**：`callback.on_llm_end(&content)`（`:3874`）后、持久化（`:3877`）前加 `let has_sendable_assistant_content = content.iter().any(|b| matches!(b, ContentBlock::Text{..}|ContentBlock::ToolUse{..}));` + `let thinking_only = !has_sendable_assistant_content;`（镜像 `turn_loop.rs:1267-1283`，issue #1727 注释）；持久化 `history.push(assistant)` 包进 `if has_sendable_assistant_content { … }`（镜像 `turn_loop.rs:1286-1293`——DeepSeek 拒收只含 thinking 的 assistant）。`content` 仍 move 进下游 `tool_uses` 抽取（`:3888`），guard 只包 push。
3. **`host_executor.rs` — 接尾巴（`run_inner` no-tool-calls arm）**：`callback.on_complete(&StopReason::NoToolCalls)`（`:4067` → 现 `:4110`）前加 `if thinking_only && should_emit_thinking_only_status(true, true, self.is_cancelled(), !pending_steers.is_empty(), false) { self.emit_status("Model returned reasoning but no answer or tool call; turn ended without output. Send a follow-up to retry.".to_string()).await; }`（镜像 `turn_loop.rs:1549-1567`，复用既有 `emit_status` `:1919` + `is_cancelled` `:1928`）。注释列四平凡真 + deferred-decide 不变量。
4. **模块文档 + 5 测试 + 1 既有测试更新**：seam-2 行 "thinking-only handling still to come" → "✅ thinking-only handling（issue #1727：stream 只产 `Thinking` block 时不持久化 + 干净尾发 status via `should_emit_thinking_only_status`，deferred-decide 过 steer/subagent resume 分支——slice 39 §E）"；drain 注释 `:3919` "no thinking-only / goal-continuation / REPL branches" → "no goal-continuation / REPL resume branches（thinking-only 现为 terminal status，非 resume）"；新增 "Known gaps in thinking-only handling (by design)" 小节（goal-continuation / inline REPL resume 分支 deferred，infra 在 `tool_state/goal.rs` / `repl/` 未接；placeholder-thinking-for-tool-call-turns 是逆向缺口，非本切片）。5 新测试（"§E slice 39 — thinking-only handling" 组）：`should_emit_thinking_only_status_only_on_clean_end`（pure-fn，6 断言镜像退役 `turn_loop.rs:3121-3184`——干净尾发；tool_uses 待决 / turn_error / cancelled / steer pending / subagents running 各 suppress）、`thinking_only_turn_not_persisted_and_emits_status`（headline：thinking-only round → `NoToolCalls`、`history.len()==1`（assistant 不持久化）、event 通道收 `Status` 含 "reasoning but no answer"）、`text_only_turn_persisted_no_thinking_status`（纯 text 回合不变——assistant 持久化、无 thinking-only status，回归守卫）、`thinking_plus_text_turn_persisted_no_thinking_status`（thinking+text 同回合 `has_sendable` 真→持久化含双 block、无 status，守卫 flag = `!has_sendable` 非 "has thinking"）、`thinking_only_with_mid_stream_steer_resumes_no_status`（deferred-decide 集成：`with_steer_on_stream` mid-stream 注入 steer → `reduce_stream` `try_recv` 捕获 → post-stream flush resume，thinking-only 尾巴不到、status 不发——spurious-"turn ended"-before-resume 守卫）。1 既有测试更新：`transparent_retry_skips_clean_empty_stream` 的次级断言 `statuses().is_empty()` → 期望恰好一条 thinking-only status（空 stream 是 `!has_sendable`，issue #1727 注释明文覆盖 "empty content"——guard 吸收后空 stream 正确发 status；主断言 no-retry/1-request/`NoToolCalls` 不变）。

**By-design gaps（deferred，documented）：**
- **goal-continuation / inline REPL resume 分支 deferred**：生产 `tool_uses.is_empty()` 尾巴在 thinking-only status 之前还跑两个 *resume* 分支——goal-continuation（`goal_continuation_message_if_needed`——active `update_goal` 时注入 continuation prompt 并 resume，cap `MAX_GOAL_CONTINUATIONS_PER_TURN=3`）与 inline REPL（` ```repl fenced blocks 经 `PythonRuntime` 执行、反馈 `<turn_meta>`）。执行器两者皆无：infra 仍 live（`tool_state/goal.rs`、`repl/sandbox.rs`+`repl/runtime.rs`）但未接，故一个本会因这两者 resume 的 thinking-only 回合现在直接 `NoToolCalls` + status。二者是更大、更不自包含的切片（各需 mid-loop host state / runtime），仍 deferred。
- **placeholder thinking for tool-call turns 不注入**：逆向缺口——模型产 tool calls 但无 reasoning 时，生产注入 `"(reasoning omitted)"` 占位 `Thinking` block（DeepSeek thinking-mode API 要求每个 tool-call assistant 消息带 `reasoning_content`，`turn_loop.rs:1202-1212`）。执行器 `finalize_blocks` 逐字持久化 stream blocks，无占位。独立缺口（非 thinking-only *handling*——后者是 thinking-*only* 回合），本切片不动。
- **cancelled / steer-pending / subagents-running suppression 由 pure-fn 覆盖**：生产 issue #1727 仅有 pure-fn 测试（`turn_loop.rs:3121-3184`），无端到端 tail 测试。本切片同——pure-fn `should_emit_thinking_only_status_only_on_clean_end` 覆盖五 suppress 条件（incl. cancelled、steer-pending、subagents-running），集成测试只覆盖 pure-fn 不可达部分（持久化 guard + status 发出）。mid-stream steer 集成测覆盖 deferred-decide；cancelled 端到端难定序（pre-`run` cancel → stream phase `Interrupted` 先返，不到尾巴），由 pure-fn 覆盖，faithful 于生产。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime`（lib）**零 warning**；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 10 均既有 warning（零新增——均在非本切片代码：`callback_bridge.rs`/`context.rs`/`task_v2.rs`/`framework_adapter.rs`/`purge.rs` + `host_executor.rs` 既有 `multiple_compactions_accumulate_summary` 的 `unused history`）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 155 通过（150 slice 38 + 5 新 slice 39；`transparent_retry_skips_clean_empty_stream` 更新后仍过）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1134 通过 + 2 ignored（零失败，slice 38 的 flaky `mcp::tests::streamable_http_stale_session_reconnects_and_retries_tool_call` 本次通过）；`cargo +1.90.0 test -p codesmith-agent --lib` 79 通过（core trait 未触）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 126 通过 + 1 ignored（host wire-in 零回归）；`cargo +1.90.0 build --workspace` 全绿（tui bin 143 warning 均既有死代码，无新增）。

**下一聚焦工作：**
- seam-2 至此全闭合（inline stream reduction / transparent-retry / early-tool-start / subagent post-stream drain + blocking hold / **thinking-only handling**）。模块文档 "still to come" 余唯一项：seam-3 的 **parallel dispatch**（tool `for` 循环内）——更大切片（plan 构造 `ToolExecutionPlan`、`FuturesUnordered` 并发、index-preserving outcomes、`record_outcome` 移到 post-batch、可选 `tool_exec_lock`、`multi_tool_use.parallel` 合成 fanout），独立切片。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先；DeepSeek `client.rs`/`chat.rs` 残件删除（blocked on cache-warmup/debug-inspect 迁出 tui）维持。

---

**进度（2026-07-15 §E seam-3 parallel dispatch 落地，tool-dispatch 循环吸收 `plan_tool_execution_batches` + `FuturesUnordered`，slice 40，`feat/pluggable-framework-core`）：**

§E 的第四十个切片落地——关闭 seam-3 "parallel dispatch（tool `for` 循环内）still to come" 缺口（`host_executor.rs:271-273` 模块文档，slice 39 遗留的唯一 "still to come" 项）。执行器此前把 `tool_uses`（`reduce_stream` 抽出）逐条**严格顺序** dispatch（`for (i, (id, name, input)) in tool_uses.into_iter().enumerate()`，~`:4180-4330`）——即使连续多个 read-only 工具也串行。生产侧（`tool_execution.rs:157`）早已用 `FuturesUnordered` 并发跑 batched read-only 工具，复用既有 batch 分类器（`dispatch.rs` 的 `plan_tool_execution_batches` / `ToolExecutionPlan` / `ToolExecutionBatch`，§E 早先切片落地）。本切片把分类器接进执行器的 dispatch 循环：连续 read-only、非 approval 的 `tool_uses` 分类进 `Parallel` batch、`FuturesUnordered` 并发跑；serial 工具（approval-required / 非 parallel）各成独立 `Serial` batch、顺序跑。outcomes index-preserving、`on_tool_start`/`on_tool_end` 每 batch LIFO、`record_outcome` / LSP collect / read-file observe / error-escalation / push `ToolResult` 延后到 sequential post-batch pass。

**关键设计决策：**
- **`DispatchedTool` local struct（非复用 `dispatch::ToolExecOutcome`）**：后者带 `started_at` / `context_patch` 字段（生产侧 post-batch 用），执行器 post-batch phase 不需要；`DispatchedTool` 只带 `{ index, id, name, input, result, blocked }`——`blocked` flag 让 post-batch 跳过 `record_outcome` / LSP / read-file for loop-guard 拦截的调用（与原 inline loop 一致：`if !blocked { record_outcome / LSP / read-file }`）。
- **index-preserving outcomes**：`outcomes: Vec<Option<DispatchedTool>>`（n×None 预分配），`FuturesUnordered` drain 时 `outcomes[o.index] = Some(o)`——按 tool_use index 写，不按完成顺序。post-batch phase 按 index 顺序遍历 `ordered`，保证 `record_outcome` / push `ToolResult` 的顺序与 `tool_uses` 原序一致（slow tool 先 dispatch 但后完成，ToolResult 仍排在前——faithful 于生产 index-preserving 语义）。
- **per-batch LIFO callbacks**：每个 `Parallel` batch 在 dispatch 前 fire 全部 `on_tool_start`（index order）、drain 后 fire 全部 `on_tool_end`（reverse index order，LIFO）——栈式嵌套不跨 batch 边界（batch1 的 start/end 栈在 batch2 开始前清空）。`Serial` batch 单工具平凡 LIFO（start → end）。faithful 于生产 `tool_execution.rs` 的 per-batch start/end 模式。
- **`tool_exec_lock` 不引入**：单循环 dispatch 下，`Parallel` batch（read-only）在下一个 `Serial` batch（write/approval）开始前**完全 drain**——同 turn 无 read/write 并发冲突，锁是多余复杂度。`multi_tool_use.parallel` 合成 fanout 是 host/production 侧 concern（`tool_execution.rs` / TUI `registry.rs`），执行器从 `reduce_stream` 收到的是 flat `tool_uses`——deferred。
- **all-serial 行为等价**：若每个工具都是 serial（write/approval/blocked），各自成独立 `Serial` batch——per-batch sequential walk 复刻原 inline loop（start → guard → approval → early/run → end → post-process in order），零回归。

**落地步骤（4 步）：**

1. **imports**：`use futures_util::stream::FuturesUnordered;`（`StreamExt` 既有 :699）；`use super::dispatch::{ToolExecutionBatch, ToolExecutionPlan, plan_tool_execution_batches};`（`engine/mod.rs:2930-2932` re-export，`super::` 从 `host_executor` 解析）。
2. **`DispatchedTool` local struct**（置 `EarlyToolTask` 后）：`{ index: usize, id: String, name: String, input: serde_json::Value, result: Result<ToolResult, ToolError>, blocked: bool }`。doc 注明 per-tool dispatch outcome 从 batch-dispatch phase 携到 sequential post-batch phase，`blocked` 标 loop-guard 拦截。
3. **替换 sequential `for` loop（4 phases）**：
   - **Phase 1 — Planning**（sequential）：预分配 `outcomes: Vec<Option<DispatchedTool>>`（n×None）+ `early_for_plan` / `tool_for_plan` / `plans`。对每个 `(i, (id, name, input))`：loop-guard `record_attempt` → `Block(msg)` 标 `blocked_flags[i]=true` + `guard_result=Some(block_tool_result(msg))`；`Proceed` → `false`/`None`。pop `early_tasks.remove(&id)`；resolve `tool = tools.get(&name).cloned()`；算 `read_only` / `approval_required`（dispatcher override `approval_requirement_for` → `Some(req) => req != Auto`，否则 static `requires_approval(&caps)`——镜像 `request_approval` 的 :3529-3536）；构造 `ToolExecutionPlan`（`supports_parallel=true`、`stream_early_start_safe=early_start_safe(&caps)`、`interactive=false`）。
   - **Phase 2 — Batch classification**：`let batches = plan_tool_execution_batches(plans);`（连续 parallel-safe plan 进 `Parallel`、unsafe 各成 `Serial`）。
   - **Phase 3 — Per-batch dispatch**（`for batch in batches`）：`Parallel` → fire `on_tool_start`（index order）→ `FuturesUnordered<Pin<Box<dyn Future<Output=DispatchedTool> + Send>>>`（每 plan 一个 `Box::pin(async move {...})` `'static` future——owns `Arc<dyn Tool>`，early-start spawn site :2268 已证此 pattern `'static`）→ body：`guard_result.is_some()` → `Ok(guard)`（blocked，不 run）；else early reuse（`early.name==name && early.input==input` → `handle.take().expect().await` → `Ok`/`Err(execution_failed)`；`Some(_revised)` → Drop aborts + `tool.run`；`None` → `tool.run`）→ 返回 `DispatchedTool { blocked: guard_result.is_some() }`。index-preserving drain `while let Some(o) = futs.next().await { outcomes[o.index] = Some(o); }`。fire `on_tool_end`（reverse index order，LIFO）。`Serial(plan)` → start → guard/approval/early/run → end → `outcomes[idx] = Some(...)`（原 inline loop 逐字搬入 batch match arm）。
   - **Phase 4 — Post-batch processing**（sequential, index order）：`ordered: Vec<DispatchedTool> = outcomes.into_iter().map(|o| o.expect("all slots filled")).collect()`。`for o in &ordered`：`if !o.blocked { record_outcome → Continue/Warn/Halt }`；`if !o.blocked { Ok&&success → collect_lsp_diagnostics + record_read_file_result; Err → step_error_count+=1, step_error_categories.push }`；push `Message { role:"user", content:[ToolResult {...}] }`。保留 `early_tasks.clear()`（defensive，orphan 清理）。
4. **模块文档**：seam-3 行 "parallel dispatch still to come" → "✅ parallel dispatch（slice 40 §E：`plan_tool_execution_batches` 批连续 read-only 工具进 `Parallel` batch；`FuturesUnordered` 并发跑、index-preserving outcomes；`record_outcome` / LSP / read-file / error-escalation / push 延后到 sequential post-batch pass；`on_tool_start`/`on_tool_end` per-batch LIFO。Deferred：`multi_tool_use.parallel` 解析（host concern）、`tool_exec_lock`（单循环不必要））"。模块文档 "still to come" 至此**全清**。

**By-design gaps（deferred，documented）：**
- **`multi_tool_use.parallel` 合成 fanout deferred**：host/production concern——`tool_execution.rs` / TUI `registry.rs` 负责把模型产的单个 `multi_tool_use.parallel` 合成调用展开成多个 flat tool_uses，执行器从 `reduce_stream` 收到的已是 flat 列表。本切片不动 host 侧展开逻辑。
- **`tool_exec_lock` 不引入**：单循环 dispatch 下 `Parallel` batch（read-only）在 `Serial` batch（write/approval）前全 drain，同 turn 无 read/write 并发冲突。生产侧 `tool_exec_lock` 是为跨 turn / 跨 dispatch-loop 的并发保护（如 subagent children 与 parent 共享 workspace），执行器单循环不需要。

**测试（7 新，"§E slice 40 — parallel dispatch" 组）：**`parallel_readonly_tools_run_concurrently`（两 read-only 工具 oneshot "started" 后等 release——测试收两信号后才 release 任一，证明真并发，确定性无 timing）；`parallel_batch_outcomes_index_preserved`（slow 80ms + fast 5ms，collect 所有 message 的 ToolResult——各 ToolResult 各成独立 user message，flat-map 收——断 index 序非完成序）；`parallel_batch_lifo_callbacks`（recording callback 断 `["start:A","start:B","end:B","end:A"]` LIFO）；`mixed_batch_parallel_serial_parallel`（read-only + write(approval) + read-only → 三 batch，全跑对、ToolResult 序对）；`parallel_batch_early_task_reuse`（read-only + early-start speculatively spawned → tool run-counter=1 非 2，speculative task 复用）；`parallel_batch_blocked_tool_produces_block_result`（3 次同调用，3rd loop-guard block → run-counter=2、3rd ToolResult is_error=true）；`all_serial_tools_match_sequential_behavior`（全 write 工具 → 全 `Serial` batch → 行为匹配原顺序 loop，回归守卫）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime`（lib）**零 warning**；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 10 均既有 warning（零新增）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 162 通过（155 slice 39 + 7 新 slice 40）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1139 通过 + 2 ignored（零失败，2 个 `mcp::tests` flaky SSE/HTTP reconnect 重跑通过——与本切片无关）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui engine::` 126 通过 + 1 ignored（host wire-in 零回归）；`cargo +1.90.0 build --workspace` 全绿（tui bin 143 warning 均既有死代码，无新增）。

**下一聚焦工作：**
- seam-3 至此全闭合（parallel dispatch）。模块文档 "still to come" 至此**全清**——§E（host executor parity）的四条 seam（inline stream reduction / transparent-retry / early-tool-start / subagent post-stream drain + blocking hold / thinking-only handling / **parallel dispatch**）全部落地。§E 主体完成，余 §A（provider extraction）/ §D2 deferred 项 / B3（`ApiProvider`→`ProviderKind`）等独立工作线。
- E4（声明式 `providers.toml` + lazy）、§D2 deferred 项、B3（`ApiProvider`→`ProviderKind`）仍低优先；DeepSeek `client.rs`/`chat.rs` 残件删除（blocked on cache-warmup/debug-inspect 迁出 tui）维持。

---

**进度（2026-07-16 §A slice 41 inspect/warmup 集群迁出 tui → `codesmith-agent-runtime`，关闭 §A "blocked on cache-warmup/debug-inspect 迁出 tui" 缺口，`feat/pluggable-framework-core`）：**

§E 主体（host executor parity）闭合后转回 §A。slice 40 遗留的 "DeepSeek `client.rs`/`chat.rs` 残件删除（blocked on cache-warmup/debug-inspect 迁出 tui）" 本切片落地——把 tui 仅存的 LLM-邻接代码（prompt inspection + cache-warmup，`client.rs` 329 行 + `client/chat.rs` 1427 行 = 1756 行）整体迁入 `codesmith-agent-runtime` 新模块 `prompt_inspect`，rewire 4 个 consumer，删除 tui 文件 + 顺带清掉因此致死的 `prompt_runtime` re-export shim。行为保持（纯搬迁，无新逻辑；既有测试随代码搬迁作安全网）。

**关键设计决策：**
- **搬迁而非重写**：`cp chat.rs → prompt_inspect.rs` 逐字保留 1427 行，再施加 3 处定点替换（见下），避免 1400 行手工转录的抄写错误。bulk 内容由 `cp` 精确保留，风险集中在替换处。
- **低摩擦根因**：chat.rs 的 4 条 `crate::` 耦合里有 **3 条在 agent-runtime 内已同 crate 解析**，搬迁后零改动——`crate::prompt_runtime`（agent-runtime 自有）、`crate::tools::truncate`（agent-runtime 自有）、`crate::models`（经 `lib.rs` `pub use codesmith_agent::models` 解析）。唯二真正改动：`crate::logging::warn`（5 处）→ `tracing::warn!`（agent-runtime 全栈用 `tracing`）；`use super::{system_to_instructions, to_api_tool_name}` → 搬入模块作本地 private `fn`（二者仅 chat.rs 用，外部零引用已核实）。外部 deps（`sha2`/`serde`/`serde_json`/`tracing`）agent-runtime Cargo.toml 已有。
- **visibility `pub(crate)`→`pub`**：tui 是**不同 crate**，`pub(crate)` 在 agent-runtime 内不可达；host-facing 面（2 entry fn + 6 inspection 类型 + 其字段 + `CacheWarmupKey::{from_inspection,hash_short}` + `PromptLayerStability::label`）一律 `pub(crate)`→`pub`。内部 helper（`inspect_wire_request`/`build_chat_messages_with_reasoning`/`stable_system_prompt`/`sha256_hex` 等）保持 private `fn`；`CACHE_WARMUP_USER_TAIL` 保 `pub(crate)`；`build_chat_messages_for_request` / `tool_to_chat` 由 `pub(super) fn`→private `fn`（仅模块内用）。
- **`sha256_hex` 保本地副本**：agent-runtime 已有 4 份私有 `sha256_hex`（`prefix_cache.rs`/`prompt_zones.rs`/`tools/handle.rs`/`rlm/session.rs`）；搬入模块作第 5 份本地副本，与既有模式一致——去重为单独 follow-up（不属本切片）。
- **单切片（无 throwaway re-export shim）**：搬 + rewire + 删一次做完，而非 "搬+shim" → "删 shim" 两步。shim 是一次性 throwaway，搬迁低风险（同 crate 引用居多），全量 build+test 覆盖正确性。
- **顺带清死 shim**：tui 的 `prompt_runtime.rs`（`pub use codesmith_agent_runtime::prompt_runtime::*`，文档自述 "until later steps rewire them onto the runtime crate directly"）的唯一 consumer 就是刚搬走的 chat.rs——搬迁后该 shim 致死（新增 `unused import` warning）。本切片一并删除 `prompt_runtime.rs` + `mod prompt_runtime;`，消除该 warning，tui warning 数回到 baseline 143（零新增）。注：`prompt_zones.rs` 同型死 shim 但为**既有** warning（非本切片致死），超出范围，留 follow-up。

**落地步骤（5 步）：**
1. **新模块 `crates/agent-runtime/src/prompt_inspect.rs`**（1734 行）：`cp` chat.rs 逐字拷贝；顶部 module doc + imports 块替换（新 doc + 去 `use crate::logging;` + 去 `use super::{...}` + 内联 `to_api_tool_name`/`system_to_instructions` 两 helper）；`replace_all pub(crate)→pub`；`replace_all pub(super) fn→fn`；5 处 `logging::warn(...)`→`tracing::warn!(...)`（`format!(...)` 包装去掉，tracing 宏直收 format args）；末尾追加 `inspect_entry_tests` 模块（4 测试从 client.rs 搬入，`use crate::client::chat::build_chat_messages_for_request` → `use super::*`）。
2. **wire**：`crates/agent-runtime/src/lib.rs` 加 `pub mod prompt_inspect;`（字母序 `project_context`/`prompt_runtime` 之间）。
3. **rewire 4 consumer**（import 行换，零逻辑改动）：`tui/ui.rs`、`tui/app.rs`、`commands/debug.rs`、`commands/core.rs` 的 `use crate::client::{...}` → `use codesmith_agent_runtime::prompt_inspect::{...}`。`debug.rs` 经 `PromptInspection.layers` 字段访问 `PromptLayerInspection`/`PromptLayerStability`（无需 type-name import，字段 `pub` 即可达）。
4. **删 tui 残件**：`rm crates/tui/src/client.rs`（329）+ `crates/tui/src/client/chat.rs`（1427）+ 空 `client/` 目录；`main.rs` 去 `mod client;`。`rm crates/tui/src/prompt_runtime.rs`（7）+ `main.rs` 去 `mod prompt_runtime;`（清死 shim）。
5. **核实零 `crate::client` 残留**：`grep crate::client crates/tui/src/` 唯一命中在 client.rs 自身（随删消失）；其余 `client::` 命中均为 `llm_client`（无关模块）。

**测试（11 测试随代码搬迁 tui → agent-runtime，组成 `prompt_inspect` 三 test mod）：**`inspect_entry_tests`（4，原 client.rs：stable layers/dynamic user task、static base hash 跨 user task、tool catalog 入 static prefix hash、cache warmup 复用 stable prefix + 固定 user tail）；`stream_decoder_tests`（2，原 chat.rs：turn-meta dedup 元数据、tool-result budget 元数据）；`alias_thinking_detection_tests`（5，原 chat.rs：DeepSeek alias reasoning_content 检测 + effort off override）。11 测试零修改随代码迁入，作行为保持安全网。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime`（lib）**零 warning**；`cargo +1.90.0 test -p codesmith-agent-runtime --lib prompt_inspect` 11 通过（`inspect_entry_tests` 4 + `stream_decoder_tests` 2 + `alias_thinking_detection_tests` 5）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1152 通过 + 2 ignored（零失败，含迁入的 11 prompt_inspect，`prompt_inspect.rs` 自身零 warning）；`cargo +1.90.0 build -p codesmith-agent-runtime --tests` 10 均既有 warning（零新增）；`cargo +1.90.0 build -p codesmith-tui` tui bin **143 warning**（baseline，零新增——baseline diff 核实唯一新增 `unused import: codesmith_agent_runtime::prompt_runtime::*` 经删 prompt_runtime shim 消除）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui` 2840 通过 + 2 ignored（零失败，consumer 测试全绿：`debug::cache_inspect_*` 7、`debug::warmup_status_*` 3、`commands::cache_inspect/warmup_dispatches` 2、`context_inspector` 等——exercises `inspect_prompt_for_request`/`CacheWarmupKey::from_inspection`/`format_verbose_diff`/`changed_static_layers` 自新路径）；`cargo +1.90.0 build --workspace` 全绿。

**By-design gaps（deferred，documented）：**
- **`sha256_hex` 去重 deferred**：agent-runtime 现有 5 份私有 `sha256_hex` 副本（含本切片搬入的）；promote 一份到共享 util 是单独 follow-up，不属本切片范围。
- **reasoning predicates 去重 deferred**：`requires_reasoning_content`/`should_replay_reasoning_content`/`has_deepseek_r_series_marker` 现在 agent-runtime（`prompt_inspect`，供 inspect/warmup 路径）与 providers crate（`rig_adapter/reasoning.rs`，供 rig adapter）各一份——搬迁不使重复恶化（搬迁前是 tui + providers 两份，现 agent-runtime + providers 两份，同数）。lift 到 `codesmith-agent` core 使两侧共享一份（providers `reasoning.rs:11-18` 已 flag）是单独 follow-up。
- **`prompt_zones.rs` 死 shim 未清**：tui 的 `prompt_zones.rs`（`pub use codesmith_agent_runtime::prompt_zones::*`）是**既有** unused warning（非本切片致死），超出范围，留 follow-up。

**下一聚焦工作：**
- §A 主体（provider extraction）至此**基本闭合**——tui 不再持任何 LLM 邻接代码（client.rs/chat.rs/prompt_runtime.rs 全删），inspect/warmup 落 agent-runtime。余 §A 残件为上述两个去重 follow-up（`sha256_hex`、reasoning predicates）。
- B3（`ApiProvider`→`ProviderKind`）：provider seam 已经 `&str` 解耦（providers crate 零 `ApiProvider` 依赖），残件是 agent-runtime 的 budget/capability 路径（`context.rs` 的 `context_input_budget_for_provider(provider: ApiProvider, …)`）+ ~20 tui config 站点——仍低优先。
- §D2（custom provider config 逃逸口）、E4（声明式 `providers.toml` + lazy）维持。

**进度（2026-07-16 §A slice 42 残件去重收口——reasoning predicates lift 到 `codesmith-agent` core + `sha256_hex` dedup 到 agent-runtime `utils`，穷尽 slice 41 "下一聚焦工作" 列的两个 §A 去重 follow-up，`feat/pluggable-framework-core`）：**

slice 41 的 "下一聚焦工作" 列出两个 §A 残件去重 follow-up（`sha256_hex`、reasoning predicates）；本切片一次性闭合两者——**纯去重，零行为变更，无新逻辑**（镜像 slice 19 的两段式机械前置项结构）。Part A + Part B 同切片落地。

**关键设计决策：**
- **Part A — lift 3 predicates 到 core**：`requires_reasoning_content`/`should_replay_reasoning_content`/`has_deepseek_r_series_marker` 在 providers（`rig_adapter/reasoning.rs`）与 agent-runtime（`prompt_inspect.rs`）两份 byte-identical（body）。三者纯 `&str`/`Option<&str>`，零类型依赖 → lift 到 `codesmith-agent` core（新 `reasoning.rs`）不引 dep edge（core 仅依赖 `codesmith-config`/`codesmith-tools` + serde；两 consumer 均已依赖 `codesmith-agent`）。此举落地 `providers/reasoning.rs` 早已 flag 的 "Follow-up: extract the shared predicates into `codesmith-agent`"（其文本同时 stale——点名已删的 `crates/tui/src/client/chat.rs`）。provider-aware wrapper（`should_replay_reasoning_content_for_provider`，持 provider-name allowlist）+ `apply_reasoning_effort`（`serde_json` `Map` 翻译）留 providers——是 provider-shaping concern，非 model-name heuristic。
- **Part B — dedup `sha256_hex` 到 `utils`**：5 份 byte-identical 私有 `fn sha256_hex`（`prefix_cache`/`prompt_zones`/`tools/handle`/`rlm/session`/`prompt_inspect`），body 均为 `Sha256::new()` + `format!("{:x}", hasher.finalize())`。natural home 是 agent-runtime `utils.rs`（既有 `pub mod utils`，明文 "shared helpers"，当前无 hash code）。`sha2` 已是 workspace + agent-runtime dep；`hex` 未用，故沿用 `format!("{:x}", …)` 不引新 dep。scope 限 agent-runtime 5 份（匹配 slice 41 framing；tui/cli 的 2 份 cross-crate follow-up，超出范围）。
- **零行为变更核实**：两 part 均 verbatim 搬 body——`reasoning.rs` 三 fn body 逐字来自 providers 副本（最 documented 那份）；`utils::sha256_hex` body 逐字来自 5 份副本。call sites 零改动（predicate 调用点 `prompt_inspect.rs:93/103`、`providers/reasoning.rs` wrapper；`sha256_hex` 调用点均传 `&[u8]`）。新增 `sha256_hex` smoke test 钉 canonical 向量（`b""`==`e3b0c44…`、`b"abc"`==`ba7816bf8f01…`——经 `openssl`/`python hashlib`/`shasum`/sha2 crate 四个独立实现交叉核实，函数确产 canonical SHA-256）。
- **test coverage 不丢**：Part A canonical merged 测试取两 consumer 的并集——正例（v4/chat/reasoner/r-series/reasoner/-reasoning/-thinking + suffix 变体 `deepseek-chat:free`/`deepseek-reasoner-2025-05` + `deepseek-reasoner-v2` pin）、负例（v3/v3.1/plain deepseek/qwen3-235b/gpt-4o + `deepseek-coder`/`qwen3-coder`/`claude-sonnet-4-6`）、`has_deepseek_r_series_marker` trailing-digit、`should_replay_reasoning_content` off/disabled/none/false override + None/medium model-driven。providers 删 3 冗余 predicate 测试（canonical 覆盖于 core）、保 `replay_truth_table`（exercises 留此的 wrapper）+ 全 `apply_reasoning_effort` 测试。prompt_inspect 删 `alias_thinking_detection_tests`（5 测试，全被 core canonical 覆盖）。

**落地步骤（Part A 4 步 + Part B 2 步）：**
1. **新 `crates/agent/src/reasoning.rs`**（单文件，镜像 source module 名）：3 fn——`pub fn requires_reasoning_content`、private `fn has_deepseek_r_series_marker`、`pub fn should_replay_reasoning_content`（body + doc-comment 逐字来自 providers 副本）。附 `#[cfg(test)] mod tests`（canonical merged coverage）。
2. **`crates/agent/src/lib.rs`**：`pub mod reasoning;` 字母序插 `provider`/`retry` 之间；"## What lives here" 加 `reasoning` bullet。
3. **`crates/providers/src/rig_adapter/reasoning.rs`**：删 3 本地 predicate def；加 `use codesmith_agent::reasoning::{requires_reasoning_content, should_replay_reasoning_content};`（`has_deepseek_r_series_marker` 留 core private——仅 `requires_reasoning_content` 调它）。保 provider-shaping locals（`provider_accepts_reasoning_content`/`should_replay_reasoning_content_for_provider`/`apply_reasoning_effort`）。module doc 替换 stale follow-up 文本。删 3 冗余 predicate 测试、保 `replay_truth_table` + `apply_reasoning_effort` 测试。`shaper.rs:144/161` 调用点（wrapper + apply）零改动。
4. **`crates/agent-runtime/src/prompt_inspect.rs`**：删 3 本地 predicate def；加 `use codesmith_agent::reasoning::should_replay_reasoning_content;`（非测试代码仅调用此一个——`build`/`inspect` 各一处）。删 `alias_thinking_detection_tests` 模块（全被 core canonical 覆盖）。
5. **`crates/agent-runtime/src/utils.rs`**：加 `use sha2::{Digest, Sha256};` + `pub fn sha256_hex(bytes: &[u8]) -> String`（verbatim body）于 "=== Hashing ===" 节；附 `#[cfg(test)] mod sha256_hex_tests`（2 smoke test 钉 canonical 向量）。
6. **5 文件 rewire**：删本地 `fn sha256_hex` def；加 `use crate::utils::sha256_hex;`；删 dead `use sha2::{Digest, Sha256};` import（per-file 核实仅 helper 用）。`prompt_zones.rs`/`tools/handle.rs` 的 `#[allow(dead_code)]` prologue 随副本删（`#[allow]` 在 fn 上；周边 unwired module 逻辑保各自 allow）。call sites 零改动。

**测试：** Part A 新 `reasoning` canonical 测试 9 个（`codesmith-agent` lib：`requires_reasoning_content_matches_deepseek_family`/`explicit_v4_ids_still_require_reasoning_content`/`alias_prefix_handles_suffixed_variants`/`reasoning_alias_remains_reasoning_when_suffixed`/`requires_reasoning_content_rejects_non_reasoning`/`non_thinking_aliases_remain_excluded`/`r_series_marker_requires_trailing_digit`/`explicit_reasoning_off_overrides_alias_detection`/`replay_is_model_driven_when_effort_unset`）；providers 保 `replay_truth_table` + 9 `apply_reasoning_effort` 测试（删 3 冗余 predicate 测试）；prompt_inspect 11→6（删 `alias_thinking_detection_tests` 5，`inspect_entry_tests` 4 + `stream_decoder_tests` 2 不变，含 `stream_decoder_tests` line 1325 的 `sha256_hex` caller）；utils 新 `sha256_hex_tests` 2 smoke test。

**验证：** `cargo +1.90.0 build -p codesmith-agent`（lib）**零 warning**；`cargo +1.90.0 test -p codesmith-agent --lib` 88 通过（含 9 新 `reasoning` 测试）；`cargo +1.90.0 build -p codesmith-providers --features openai,deepseek,openai-compat` 零 warning；`cargo +1.90.0 test -p codesmith-providers --features openai,deepseek,openai-compat` 28 通过（`replay_truth_table` + 9 `apply_reasoning_effort` 测试，3 冗余 predicate 测试删——canonical 覆盖于 core）；`cargo +1.90.0 build -p codesmith-agent-runtime`（lib）**零 warning**；`cargo +1.90.0 test -p codesmith-agent-runtime --lib prompt_inspect` 6 通过（11→6，`alias_thinking_detection_tests` 删）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` 162 通过（回归，未触）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib 'utils::sha256_hex_tests'` 2 通过；`cargo +1.90.0 test -p codesmith-agent-runtime --lib --no-run` 10 均既有 warning（零新增——grep 核实 6 changed 文件零 warning 命中）；`cargo +1.90.0 build --workspace` 全绿（tui bin 143 + tool-impls 15 warning 均既有死代码；stash-compare 核实 `prompt_zones` glob unused warning 在 HEAD baseline 即 1 次，零新增）。

**By-design gaps（deferred，documented）：**
- **`sha256_hex` 在 tui/cli**（2 份，`tui/tool_output_receipts.rs`、`cli/update.rs`）：cross-crate follow-up——需 tui/cli 依赖 `codesmith-agent`/`agent-runtime` 得一 hash helper；超出 slice 41 framing。
- **provider-aware wrapper / `apply_reasoning_effort` 留 providers**：provider-shaping concern（provider-name allowlist + `serde_json` `Map`）——非 3 pure predicates 一部分。留本地 + feature-gated。
- **inspect/warmup 采用 provider-aware wrapper**：今 `PromptBuilder` 调 model-only `should_replay_reasoning_content`（scope 无 provider name）；采用 wrapper 需穿 provider name 过 `PromptBuilder::for_request`——是 behavior question，非 dedup。Deferred。

**下一聚焦工作：**
- §A 残件去重（`sha256_hex`、reasoning predicates）本切片穷尽——§A 主体（provider extraction + 残件去重）至此**闭合**。tui 不持 LLM 邻接代码、inspect/warmup 落 agent-runtime、3 reasoning predicate 单源在 core、`sha256_hex` 单源在 agent-runtime `utils`。
- B3（`ApiProvider`→`ProviderKind`）、§D2（custom provider config 逃逸口）、E4（声明式 `providers.toml` + lazy）维持。

**进度（2026-07-16 §E4 slice 43 declarative `providers.toml` manifest（schema + loader in `codesmith-config`，E4 第一子切片），`feat/pluggable-framework-core`）：**

slice 42 闭合 §A 后复查余项状态：§D2（custom provider config 逃逸口）**已落地**于 commit `9d47942c`（`custom_provider` selector + `[[providers.custom]]` 表，本文件 `:138-194`；`ARCHITECTURE.md:282` 行 stale 仍标 "⏳ deferred"——doc-debt follow-up）；§B3（`ApiProvider`→`ProviderKind`）已降级低优先（本文件 `:21,133` + `ARCHITECTURE.md:281`：rig adapter 按 `&'static str` provider name 分支，`codesmith-providers` 零 `ApiProvider` 依赖、零 `codesmith-agent-runtime` dep edge，B3 仅剩 cosmetic 的 `DeepseekCN`→`Deepseek` 折叠，边际价值极低）。故本切片转开 **§E4**（声明式 `providers.toml` + lazy）——E4 主体 greenfield 但 plumbing 已就绪（`ProviderRegistry`/`ProviderFactory`/`ProviderId`/`ProviderConfig` seam 闭合并经生产路径 `engine.rs:324` 验证；`codesmith-config` 既有 `[[providers.custom]]` 表为先例 shape）。

按本仓小切片惯例，E4 切两子切片：**slice 43 = schema + loader（config 层，不接 registry）**；slice 44 = registry 接线 + lazy cache 半。

**关键设计决策：**
- **manifest 是"内建目录"声明式清单，与既有两概念正交**：`config.toml` 的 `[providers]` 表（每 provider 运行时 override：api_key/base_url/model）+ `[[providers.custom]]`（§D2 用户逃生口）**不变**；新 `providers.toml` 把当前硬编码在 `default_registry`（`register(mock…)`/`register(openai…)`/`openai_compat::register`）+ `COMPAT_KINDS`（`openai_compat.rs:63-77`，13 个 openai-compat id）的 provider 目录外置成数据文件。secrets（`api_key`）**不进**清单——留 `config.toml`/env。
- **`backend` 用 closed `FactoryBackend` enum（5 变体：`Mock`/`Openai`/`Anthropic`/`Deepseek`/`OpenaiCompat`，kebab-case serde）**，非自由 string：parse 期即拒 typo（serde unknown-variant），镜像 `ProviderKind` 的 closed-enum 做法。语义：runtime manifest 只能"在已编译进 factory 中选择"（Cargo feature 是编译期）；`backend="openai-compat"` 但 `openai-compat` feature 未编译 → resolve 期报错（providers 层，slice 44）。
- **校验最小集**：slice 43 只做 `validate` 的 dup-id 拒绝 + 空/whitespace id 拒绝；`backend` 已由 serde parse 期约束。builtin-id 关系（manifest id 可等于 `ProviderKind::as_str`——它描述 builtin→backend 映射，与 §D2 `[[providers.custom]]` 的"不得撞 builtin"规则相反、各自正确）留 registry 接线切片处理。
- **loader 三层分离以可测**：`ProvidersManifest::parse(toml_str)`（pure）+ `load_providers_manifest_from(path)`（读文件 + parse + validate，pure，tempfile-free 用 `std::env::temp_dir` 测）+ `resolve_manifest_path()`（读 `CODESMITH_PROVIDERS_MANIFEST` env，pure，每次 fresh 读无 init 问题）+ `providers_manifest()`（`OnceLock` 缓存全局，"读一次"半——env unset/路径缺省 → 空 manifest，零行为变更直到 slice 44 接线）。全局 accessor 的可测属性 = 引用稳定（两次调用同 `&'static` ref，`std::ptr::eq` 钉），不依赖 init 顺序；content/parse/file-reading 由 pure fn 各自测。
- **layering**：`FactoryBackend` 定义在 `codesmith-config`（最低层；`codesmith-providers`→`codesmith-agent`→`codesmith-config`），config 不依赖 providers——`backend` 是 config 层的 closed string 集，非 providers 类型引用。**by-design gap**：加新 backend factory 需同时加 config 变体 + providers factory（镜像既有 `ProviderKind`↔factory 耦合）。

**落地步骤（6 步，纯 config 层）：**
1. `crates/config/src/lib.rs` 于 `impl ProvidersToml`（line 282）后、`ConfigToml` 前插 manifest 集群：`FactoryBackend` enum + `as_str`、`ProviderDescriptor`（id/backend/base_url/model）、`ProvidersManifest`（`Vec<ProviderDescriptor>`，`#[serde(default)]`，derive `Default` 空 manifest）+ doc 含 toml 示例。
2. `impl ProvidersManifest`：`parse(toml_str) -> Result<Self>`（`toml::from_str` + `.context("parsing providers.toml manifest")`）、`validate(&self) -> Result<()>`（`HashSet` 检 dup-id + 空 id，`bail!`）。
3. `fn load_providers_manifest_from(path: &Path) -> Result<ProvidersManifest>`（`fs::read_to_string` + parse + validate）。
4. `const PROVIDERS_MANIFEST_ENV: &str = "CODESMITH_PROVIDERS_MANIFEST"` + `fn resolve_manifest_path() -> Option<PathBuf>`（读 env，非空则 `Some`）。
5. `pub fn providers_manifest() -> &'static ProvidersManifest`（`static MANIFEST: OnceLock<ProvidersManifest>`，`get_or_init`：`resolve_manifest_path()` → `load` → 成功入、失败 `tracing::warn!` 后 `default()`；None → `default()`）。
6. `mod tests` 尾加 8 测试（见下）。`env::set_var`/`remove_var` 在 edition-2024 unsafe，按既有 pattern（`lib.rs:2779`）包 `unsafe { }` + `env_lock()` 串行。

**测试：** `manifest_round_trip`（parse sample → 字段断言 → serialize → re-parse 等价）、`manifest_rejects_unknown_backend`（typo `openai-compt` → parse 期 serde 拒，`chain()` 含 typo）、`manifest_rejects_duplicate_ids`（dup `openrouter` → validate 拒 "duplicate id"）、`manifest_rejects_empty_id`（`"   "` → validate 拒 "empty id"）、`manifest_minimal_entry_ok`（仅 id+backend，base_url/model optional）、`resolve_manifest_path_reads_env`（env unset → None / set → Some(path)）、`load_providers_manifest_from_reads_file`（`temp_dir` 写 fixture → load → 2 条目 + backend 断言）、`providers_manifest_is_cached`（两次调用 `std::ptr::eq` + empty default）。

**验证：** `cargo +1.90.0 build -p codesmith-config`（lib）**零 warning**；`cargo +1.90.0 test -p codesmith-config --lib` 85 通过（含 8 新 manifest 测试，77 既有回归）；`cargo +1.90.0 test -p codesmith-config --lib manifest` 8 通过；`cargo +1.90.0 build --workspace` 全绿（tui bin 143 + tool-impls 15 warning 均**既有死代码**，与 slice 42 baseline 逐一对齐，零新增——grep 核实零 warning 命中 `codesmith-config` 或新代码）。

**By-design gaps（deferred，documented）：**
- **registry 接线（slice 44）**：`default_registry` 读 `providers_manifest()` 按 `FactoryBackend` 注册 factory；`FactoryBackend`→factory 映射落 providers；实际 `providers.toml` 文件 ship（providers crate `include_str!`，外置 `COMPAT_KINDS` + 4 dedicated factory）。
- **lazy cache 另半（slice 44）**：`default_registry()` 背 `OnceLock`（消除 `engine.rs:324` 每请求重建）——本 slice 只交付"manifest 读一次"半。
- **feature 编译期校验（slice 44）**：`backend` 未编译进 → resolve 期报错。
- **manifest 错误处理**：当前 `providers_manifest()` 加载失败 `tracing::warn!` 后回空 manifest（infallible `&'static`，无 consumer 前可接受）；slice 44 接线时按 registry 语义重审（空 manifest = 不额外注册 vs fallback Rust 注册）。
- **`ARCHITECTURE.md:282` §D2 行 stale**（标 "⏳ deferred" 实已落地于 `9d47942c`）：doc-debt follow-up，超出本 E4 slice 范围。

**下一聚焦工作：**
- §E4 slice 44：registry 接线（`default_registry` 读 `providers_manifest()` 按 `FactoryBackend` 注册 factory）+ lazy cache 半（`default_registry()` 背 `OnceLock`）+ ship 实际 `providers.toml`（外置 `COMPAT_KINDS`）。本 slice 的 schema/loader 为其前置。
- §B3（`ApiProvider`→`ProviderKind`，cosmetic `DeepseekCN` 折叠）、§D2 残件 polish（CLI flag / per-entry `config set` / bare `provider=` 形）维持低优先。

**进度（2026-07-16 §E4 slice 44 registry 接线 + lazy cache 半（`default_registry` 读 manifest 按 `FactoryBackend` 注册 + `OnceLock` + ship 实际 `providers.toml`），`feat/pluggable-framework-core`）：**

接 slice 43（schema/loader 落 `codesmith-config`，`:2050`）。本 slice 把 manifest 接进 providers 层：`default_registry()` 由"每请求手注册 4 dedicated + `openai_compat::register` 13 compat"改为 manifest 驱动 + `OnceLock` 缓存。slice 43 的 `providers_manifest()`（env override 通道，env unset → 空）此前零行为变更；本 slice 使之成 override 通道（非空 override **替换** bundled 清单，失败回退 bundled）。

**关键设计决策：**
- **bundled `providers.toml` ship 在 providers crate**（`include_str!("../providers.toml")`），17 条目（4 dedicated `mock`/`openai`/`anthropic`/`deepseek` + 13 `openai-compat`，verbatim 取自原 `COMPAT_KINDS` `openai_compat.rs:63-77`）。catalog（哪些 id→哪个 backend）是 providers-crate 知识（镜像其 Cargo feature）；`FactoryBackend` closed enum 仍在 `codesmith-config`（最低层）——config 不依赖 providers，无环。
- **`default_registry() -> &'static ProviderRegistry`**（`OnceLock`，"registry built once"半；manifest "read once"半 = slice 43 的 `providers_manifest()`）。sole production caller `engine.rs:324` `default_registry().build(&cfg)` 无改动（`build(&self)`）。
- **`ProviderRegistry` 加 `Clone`**（`#[derive(Clone, Default)]`，`Arc` 值浅拷贝）：cached `&'static` 不可变，host 定制走 `default_registry().clone()` + `register(&mut self)`，保住 pi-mono "freely replace" seam（production caller 不 mutate，故 derive 安全、向后兼容）。
- **feature 编译期校验 = 诊断 stub**：manifest 条目 `backend` 对应 Cargo feature 未编译进 → 注册 `UncompiledBackendFactory` stub，其 `build()` `bail!` 清晰报错（指明 id + 缺失 feature + `--features <backend>` 修复），而非泛 "not registered"。不在 manifest 的 id（如 `acme-llm`）仍走既有 "no provider factory registered for '<id>'" 路径（不变）。statement-cfg（非 `match`）保任意 feature 子集编译、无 `unreachable_patterns` warning。
- **env override 语义 = replace**：`CODESMITH_PROVIDERS_MANIFEST` 非空 → 替换 bundled catalog（ship 自定义 provider 集免重编译）；加载失败 `tracing::warn!` + 回空 → 回退 bundled（优雅降级）。

**落地步骤：**
1. `crates/providers/Cargo.toml` 加 `codesmith-config` 直 dep（已 transitively 在图里经 agent→config；直声明以直接 `use FactoryBackend`/`ProvidersManifest`/`providers_manifest()`；无环）。
2. `crates/providers/providers.toml`（新，17 条目，id+backend only）。
3. `crates/agent/src/provider/mod.rs`：`ProviderRegistry` 加 `Clone`（doc 注 cached-seam 用法）。
4. `crates/providers/src/lib.rs`：`default_registry()` 重写（`&'static` + `OnceLock` + `active_manifest()`/`bundled_manifest()`/`build_registry_from()` + `leak_str()`（`openai-compat` gated，bounded leak 喂 `GenericShaper::new` 的 `&'static name`）+ `UncompiledBackendFactory` stub）；crate-doc "Registering" 示例改 `.clone()` 形；删 `#[cfg_attr(...allow(unused_mut))]`（stub register 无条件 → `registry` 总被 mutate，无 unused_mut）。
5. `crates/providers/src/openai_compat.rs`：删 `COMPAT_KINDS` + `pub fn register()`（外置到 manifest，唯一 caller 已删）；加 `impl OpenAiCompatFactory { pub(crate) fn new(id, name) }`（lib.rs 不同 module，碰不到私有 field）；import 去 `ProviderRegistry`；module doc 更新。
6. `ARCHITECTURE.md`：§E4 状态行（:255）"deferred"→"landed (slice 43/44)"；providers 行（:276）注 manifest-driven + 加 `providers.toml` 路径；§D2 行（:282）stale "⏳ deferred"→"✅ done (9d47942c)"（doc-debt，slice 43 标记）；"host seeds" snippet（:309）`default_registry()`→`default_registry().clone()`。

**测试：** `manifest_tests`（新 `#[cfg(test)]` module，不 gate rig——stub 路径不建 rig client，每个 feature config 都跑）：`bundled_manifest_has_full_catalog`（17 条目 + 4 dedicated/2 compat id 抽检 + `validate().is_ok()`）、`default_registry_is_cached`（`std::ptr::eq`）、`uncompiled_backend_factory_errors_clearly`（stub 直测，任意 config）、`uncompiled_backend_resolves_to_stub`（`#[cfg(not(feature="deepseek"))]` 端到端：deepseek off → bundled deepseek 条目落 stub → resolve 报 `--features deepseek`）。既有 `rig_registry_tests`（`let registry = default_registry()` 作为 `&'static` 仍 work）+ mock tests 全保留。

**验证：** `cargo +1.90.0 build -p codesmith-providers`（default mock / `--no-default-features` 17 stubs / `--no-default-features --features openai-compat` leak 路径 / `--all-features`）**零 warning**；`cargo +1.90.0 test -p codesmith-providers --all-features --lib` 32 通过（含 3 新 manifest_tests，`not(deepseek)`-gated stub 端到端在 all-features 正确 filtered out）；`--lib`（mock-only）9 通过 + `--no-default-features` 4 通过（stub 端到端在两 config 均 fire）；`cargo +1.90.0 build -p codesmith-agent --lib` 零 warning（Clone derive）；`cargo +1.90.0 build -p codesmith-tui` 绿（`engine.rs:324` 对 `&'static` 编译）；`cargo +1.90.0 test -p codesmith-config --lib` 85 通过（未触）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui provider` 153 通过；`cargo +1.90.0 check --workspace --all-features` 零 error（既有 unused-import warning 均在 agent-runtime/executor，与 slice 42 baseline 对齐，零新增命中 providers/agent/config）。

**By-design gaps（deferred，documented）：**
- **`base_url`/`model` 列 + 消费**：`ProviderDescriptor` 有 optional `base_url`/`model`，但本 slice shipped `providers.toml` 仅 `id`+`backend`（"外置 COMPAT_KINDS" = id+backend 最小集）。factory 当前不消费 manifest 默认（host 经 `ProviderConfig` 传 `base_url`/`default_model`，rig factory 容空 `base_url` 回退 rig 编译期默认；`default_model` 透传不默认）。populate + wire 消费（manifest 作 per-provider 默认源）是后续 behavioral 切片。
- **env override = replace 语义**（非 augment）：`CODESMITH_PROVIDERS_MANIFEST` 非空整体替换 bundled；augment 若需是后续。
- **`ARCHITECTURE.md:282` §D2 行 stale**：本 slice 顺带修（doc-debt，slice 43 标记超出其范围）。

**下一聚焦工作：**
- §E4 残件：`base_url`/`model` 列 populate + factory 消费 manifest 默认（host 空 `base_url`/`default_model` 时回退 manifest）——使 manifest 成完整 per-provider 默认源。
- §B3（`ApiProvider`→`ProviderKind`，cosmetic `DeepseekCN` 折叠）、§D2 残件 polish（CLI flag / per-entry `config set` / bare `provider=` 形）维持低优先。

---

**进度（2026-07-16 §E4 slice 45 `providers.toml` `base_url`/`model` 列 populate + factory 消费 manifest 默认，闭合 slice 44 "下一聚焦工作" 的 §E4 残件，`feat/pluggable-framework-core`）：**

接 slice 44（registry 接线 + lazy cache 半，`:2086`）。slice 44 shipped `providers.toml` 仅 `id`+`backend`（"外置 COMPAT_KINDS" 最小集），factory 不消费 manifest 默认——host 经 `ProviderConfig` 传 `base_url`/`default_model`，rig factory 容空 `base_url` 回退 rig 编译期默认，`default_model` 透传不默认。本 slice 使 manifest 成完整 per-provider 默认源：populate `base_url`/`model`（16 非慢条目，mock 不加），4 类 factory 在 host 传空时回退 manifest 默认。纯增量、零既有调用点行为改动（builtin host 路径仍由 config 常量解析非空 `base_url`，manifest 回退不触发；受益者是 custom-provider 空路径 + 直接 factory 调用）。

**关键设计决策：**
- **populate 取值 verbatim 自 `codesmith-config` `DEFAULT_*` 常量**（`lib.rs:17-69`）：16 非 mock 条目加 `base_url`+`model`，值逐字取自 `DEFAULT_*_BASE_URL`/`DEFAULT_*_MODEL`。manifest 是 providers 层声明 config 层常量的并行同值副本——layering 决定 config 不能依赖 providers 的 bundled manifest，故两份并行（dedup 需 config 读 manifest，跨层不可达，留 follow-up）。
- **共享 helper（dedup）**：lib.rs 新增 `#[cfg(feature="rig")] pub(crate) fn resolve_with_manifest_default(cfg_val: &str, manifest: Option<&str>) -> String`（非空 cfg→cfg；空→manifest；都空→`String::new()`），4 factory 共用。gate `rig`（4 provider feature 的 aggregate）避免 mock-only build 的 dead_code。
- **构造期注入**（镜像既有 `name` 注入）：`build_registry_from`（`lib.rs:135`）读 `desc.base_url`/`desc.model`，clone 传入 4 类 factory 构造器。factory 存 `Option<String>`（owned；registry OnceLock 构造一次，clone 16 字符串可忽略）。`name` 仍 `&'static str`（`leak_str`，`GenericShaper::new` 要求）。
- **factory build 回退**：每 `build()` 先解析 `base_url`/`default_model`（helper），再喂 builder guard + `RigLlmClient::new`。host 非空→host 值（无行为变化）；host 空→manifest；都无→空（rig 编译期默认，现状）。
- **deepseek × `resolve_base_url`**：先 `base_url = resolve_with_manifest_default(...)`（builder guard 用之），再 `resolved_base_url = resolve_base_url(&base_url)` 喂 RigLlmClient（FIM/translate shim 用）。manifest 填了 deepseek URL → `resolve_base_url` 见非空原样返回；manifest 空 → 仍回 `DEFAULT_DEEPSEEK_BASE_URL`（defense-in-depth 保留）。顺带修 latent 不一致：原 builder guard 用 raw `cfg.base_url`（空→rig api.openai.com 误用于 deepseek chat 路径），现统一用 manifest-resolved 值。

**落地步骤：**
1. `crates/providers/providers.toml`：16 非 mock 条目加 `base_url`+`model`（verbatim 取自 config `DEFAULT_*` 常量）；header 注释改"intentionally omitted"→"consumed as fallback"。
2. `crates/providers/src/lib.rs`：新增 `resolve_with_manifest_default`（`#[cfg(feature="rig")] pub(crate)`）；`build_registry_from` 4 处 register 改 `XFactory::new(base_url, model)`（compat 传 4 参：id/name/base_url/model）。
3. `crates/providers/src/openai_compat.rs`：`OpenAiCompatFactory` 加 `base_url`+`model: Option<String>` 字段；`new` 扩 4 参；`build()` 用 helper。
4. `crates/providers/src/openai.rs`：`OpenAiFactory` unit→struct 持两字段 + `new(base_url, model)`；`build()` 用 helper。
5. `crates/providers/src/anthropic.rs`：同 openai。
6. `crates/providers/src/deepseek.rs`：同理 + `resolve_base_url` 交互（喂 resolved 值）。
7. `crates/agent/src/provider/mod.rs`：无改动（`ProviderConfig`/`Factory` trait 不变）。
8. `ARCHITECTURE.md`：§E4 状态段（:255）补 slice 45（base_url/model populated + consumed）；providers 行（:278）补 manifest 作默认源。

**测试：**（1）`manifest_tests::bundled_manifest_populates_base_url_and_model`（无 rig，每 feature config 跑）：mock 的 base_url/model 为 None；openrouter/ollama/openai/anthropic/deepseek 抽检非空且匹配 `DEFAULT_*` 常量值。（2）helper 单元测试（`manifest_default_tests`，`#[cfg(all(test, feature="rig"))]`）：`resolve_prefers_non_empty_host_value`、`resolve_falls_back_to_manifest_default_when_host_empty`、`resolve_yields_empty_when_both_empty`（含 `Some("")` 当作无默认）。（3）rig-gated factory 测试（同 module，`#[cfg(feature="openai-compat")]`）：`factory_falls_back_to_manifest_default_when_host_empty`（empty cfg + manifest default → `handle.base_url()`/`model()`==manifest）、`factory_host_value_overrides_manifest_default`（非空 cfg→host 值）、`factory_empty_cfg_and_no_manifest_default_falls_through`（都空→`handle.base_url()==""`，rig 编译期默认由 builder 保留）。既有 32 测试全保留。

**验证：** `cargo +1.90.0 build -p codesmith-providers`（default / `--no-default-features` / `--no-default-features --features openai-compat` / `--all-features`）**零 warning**；`cargo +1.90.0 test -p codesmith-providers --all-features --lib` **39 通过**（slice 44 的 32 + 7 新：1 manifest populate + 3 helper unit + 3 compat factory fallback）；`cargo +1.90.0 test -p codesmith-config --lib` 85 通过（未触）；`cargo +1.90.0 build -p codesmith-tui` 绿（`resolve_llm_client` 零回归）；`cargo +1.90.0 build --workspace` 全绿（tui bin 143 warning 均既有死代码，与 baseline 对齐，零新增命中 providers/agent/config）。

**By-design gaps（deferred，documented）：**
- **config.rs `DEFAULT_*` 常量保留**：host builtin 路径仍用（layering：config 不能依赖 providers 的 bundled manifest）。manifest 与 config 常量并行同值；dedup 需 config 读 manifest 但跨层不可达，留 follow-up。
- **moonshot kimi-code 变体**（`auth_mode` 条件 URL/model）留 host concern；manifest 只放 primary（`https://api.moonshot.ai/v1` + `kimi-k2.6`）。
- **flash 模型变体**（`DEFAULT_*_FLASH_MODEL`）留 host 选择；manifest 只放 primary model。
- **`CODESMITH_PROVIDERS_MANIFEST` env override**：若填 base_url/model，factory 同样消费（override replace 语义不变，slice 44）。

**下一聚焦工作：**
- §E4 主线闭合（slice 43 schema/loader + slice 44 registry 接线 + slice 45 默认源消费——manifest 现为完整 per-provider 默认源）。后续若有 env override augment 语义或 flash/kimi-code 变体下沉，另开切片。
- §B3（`ApiProvider`→`ProviderKind`，cosmetic `DeepseekCN` 折叠）、§D2 残件 polish（CLI flag / per-entry `config set` / bare `provider=` 形）维持低优先。

---

**进度（2026-07-17 §D2 slice 46 custom provider 残件 polish——`--custom-provider` CLI flag + per-entry `config set/get/unset` + bare `provider=` form by-design 拒绝收口，`feat/pluggable-framework-core`）：**

接 slice 45（§E4 主线闭合，`:2120`）。slice 45 的 "下一聚焦工作" 列 §D2 残件 polish（CLI flag / per-entry `config set` / bare `provider=` 形）维持低优先；本切片闭合其中两项、第三项按设计拒绝收口。§D2（`9d47942c`）落了 `custom_provider` selector + `[[providers.custom]]` 表，`ARCHITECTURE.md:287` 标三残件 polish；本切片使 custom provider 可经 CLI flag 选 + 经 `config set` 管理单条目。纯增量（config 层 get/set/unset + masking + cli flag + tui env 消费），零既有调用点行为改动（builtin host 路径仍由 config 常量解析，custom 路径不变）。

**关键设计决策：**
- **CLI flag = 独立 `--custom-provider <id>`**（非扩展 `--provider`）：镜像 §D2 自己的 "dedicated `custom_provider` selector over bare `provider=`" 选择。保 `--provider ProviderArg` value_enum 校验完整（零回归）；`conflicts_with = "provider"`（builtin 与 custom 互斥）。经 env `CODESMITH_CUSTOM_PROVIDER`（legacy `DEEPSEEK_CUSTOM_PROVIDER` 经 `codesmith_env_var`）转发至 TUI，并行 `--provider` → `DEEPSEEK_PROVIDER`。builtin-id 碰撞在 cli parse 期拒（`ProviderKind::parse(id).is_some()` → bail 指 `--provider`）；entry-existence 延后至 TUI `validate`（镜像 `--provider` 的延后校验）。
- **per-entry `config set` = find-or-create by id**：键形 `providers.custom.<id>.<field>`。`split_custom_provider_key` 按**最后** `.` 切——尾段 ∈ {api_key/base_url/model/auth_mode/http_headers/id} 视为字段、其余为 id（dotted id 如 `my.co` 正确切：字段恒为末段）。set find-or-create 条目（缺则 push `CustomProviderToml{id, ..Default}`）置字段；`.id` 拒（id 是 key 非 value）；whole-array/whole-entry bail 指 hand-edit。unset 清字段 / 删条目（`retain`）/ 清全表；`.id` unset bail。get 返单条目（serialize）/ 单字段。镜像既有 per-builtin `providers.anthropic.<field>` 形。
- **masking**：`is_sensitive_config_key` 扩——whole array（既有）+ whole entry（新，因 serialize 含 api_key）+ per-entry `.api_key`（既有 `.ends_with(".api_key")` 规则覆盖）；non-secret 字段（base_url/model/auth_mode/http_headers/id）不 mask（per-field get 可见，whole entry 则整体 redact 作 blob）。
- **bare `provider=` form = by-design 拒绝收口**：`9d47942c` 已明确拒绝（closed `ProviderKind` enum 经 ConfigToml/Overrides/Env + 每 match 臂级联，破 layering）。`ARCHITECTURE.md:287` 原列其为 "deferred polish" 实为 stale；本切片改标 "by-design rejected (see 9d47942c)" 收 doc-debt，不实现。

**落地步骤：**
1. `crates/config/src/lib.rs`：新增 `CUSTOM_PROVIDER_FIELDS` const + `split_custom_provider_key` helper（`fn(&str) -> (&str, Option<&str>)`，`rsplit_once('.')` + 字段集 contains）；`get_value` 的 `providers.custom` 精确臂改 guard 臂（`key == "providers.custom" || starts_with`），内分 whole-array / whole-entry（`toml::to_string(entry)`）/ per-field；`set_value` 同 guard 臂替换原 bail——find-or-create + 置字段（http_headers 走 `parse_http_headers`）+ `.id` 拒 + whole bail；`unset_value` 的 `providers.custom` 臂改 guard 臂——clear field / retain-remove entry / `.id` bail / clear all；`is_sensitive_config_key` 扩 whole-entry + 保留 non-secret 字段可见。
2. `crates/cli/src/lib.rs`：`Cli` 加 `custom_provider: Option<String>`（`--custom-provider`/`value_name="ID"`/`conflicts_with="provider"`）；`build_tui_command` 在 `cli.provider` env 块后加 custom-provider 块——builtin 碰撞 guard（`ProviderKind::parse`）+ `cmd.env("CODESMITH_CUSTOM_PROVIDER", id)`。
3. `crates/tui/src/config.rs`：`apply_env_overrides` 在 `CODESMITH_PROVIDER` 块后加 `CODESMITH_CUSTOM_PROVIDER`（legacy `DEEPSEEK_CUSTOM_PROVIDER`）读取 → trim → 非空置 `config.custom_provider`（env 胜 file，镜像 `DEEPSEEK_PROVIDER` > file `provider`）；既有 `validate`（`:1578`）已拒 builtin 碰撞 + 缺 entry，故 env 消费端保持简。
4. `ARCHITECTURE.md` + ROADMAP §D2：状态行更新（bare form by-design rejected + slice 46 closed CLI flag/per-entry set）。

**测试：** config **10 新**（per-entry set 更新既有/创建缺失、`.id` 拒、whole-entry bail、http_headers 字段、get entry+field、unset field/entry/`.id` bail、is_sensitive masking per-entry api_key + whole-entry redacted + non-secret 字段可见）+ **1 既有改**（`..._is_readonly_and_secret` → `..._get_unset_and_secret`：whole-array set 仍 bail，per-entry set 不再拒——headline 反转）。cli **3 新**（`build_tui_command_forwards_custom_provider_env`、`build_tui_command_custom_provider_builtin_collision_bails`、`custom_provider_conflicts_with_provider_flag` clap conflict）。tui **3 新**（`apply_env_overrides_sets_custom_provider`、`apply_env_overrides_custom_provider_env_overrides_file_value` trim + env 胜 file、`apply_env_overrides_custom_provider_empty_is_noop` 空 env 不 clobber file）。

**验证：** `cargo +1.90.0 build -p codesmith-config` 零 warning；`cargo +1.90.0 test -p codesmith-config --lib` **95 通过**（85 既有 + 10 新）；`cargo +1.90.0 build -p codesmith-cli` 零新 warning；`cargo +1.90.0 test -p codesmith-cli --lib` **86 通过**（83 既有 + 3 新）；`cargo +1.90.0 build -p codesmith-tui` 绿（143 既有 warning，零新增）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui custom_provider` **12 通过**（9 既有 + 3 新）。

**By-design gaps（deferred，documented）：**
- **id 含与字段同名尾段**（如 id "model"）歧义：`providers.custom.model.base_url` 切为 (id="model", field="base_url") 而非 whole-entry for id="model.base_url"；文档建议避开。
- **`--custom-provider` 不在 cli parse 期校验 entry-existence**：延后至 TUI `validate`（镜像 `--provider` 延后校验）。
- **`CODESMITH_CUSTOM_PROVIDER` env 优先级** = CLI-sourced env 胜 file `custom_provider`（镜像 `DEEPSEEK_PROVIDER` > file `provider`）。
- §B3（`ApiProvider`→`ProviderKind`）不变——仍低优先。~~§A1（DeepSeekClient 抽取）不变——仍低优先/deferred~~ → slice 48 复核 stale：§A1 已 retirement+rig 完成（见 2026-07-07 §A1 checkpoint + slice 41/42）。

**下一聚焦工作：**
- §D2 残件 polish 全部收口（CLI flag + per-entry config set 落地，bare form by-design 拒绝收 doc-debt）。§D 主线闭合。
- §B3（cosmetic `DeepseekCN` 折叠）仍低优先。~~§A1（DeepSeekClient 抽取，需 replay bridge）仍低优先/deferred~~ → slice 48 复核 stale：§A1 已 retirement+rig 完成（replay bridge 不必要）。§E4 follow-up（env override augment / flash-kimi-code 变体下沉）按需另开切片。

---

**进度（2026-07-17 §E slice 47 residual dead-code deletion——`turn_loop::EarlyToolResult`/`EarlyToolTask` 死结构 + `ToolExecutionPlan.early_result` 死字段 + tui `prompt_zones` 死 shim，`feat/pluggable-framework-core`）：**

接 slice 46（§D2 残件 polish 收口，`:2157`）。§E 主线（slice 43-45 §E4 providers.toml + slice 40 §E parallel dispatch）已闭合，本切片清 §E 框架核心迁移遗留的纯死代码——`turn_loop` 模块在 slice 20 §E（`handle_deepseek_turn` retirement）后已缩为两个 live helper（`messages_with_turn_metadata` + `subagent_completion_runtime_message`），但 `EarlyToolResult`/`EarlyToolTask` 两结构因 `dispatch.rs::ToolExecutionPlan.early_result` 字段的类型引用而保留、并加 `#![allow(dead_code)]` 静音。slice 15 §E early-tool-start + slice 40 §E parallel dispatch 把 speculative dispatch 重接到框架执行器（`HostAgentExecutor` 自带 distinct `EarlyToolTask` 类型 + `early_tasks` map），`turn_loop` 那份纯成死重。纯删除（2 结构 + 1 字段 + 1 shim + `#![allow(dead_code)]`），零既有调用点行为改动。

**关键设计决策：**
- **两个 distinct `EarlyToolTask` 类型**：框架执行器的（`host_executor.rs:1272`，`Drop` abort `JoinHandle` 防 orphan 任务泄漏 + `handle: Option<..>` 包裹使 reuse 路径可 `Option::take` 出来 `.await`）vs 删除的 `turn_loop::EarlyToolTask`（plain struct，字段从未被读，仅被死字段 `early_result` 引用占位）。删 turn_loop 份安全——框架执行器从不查 `plan.early_result`，它在自己的 `early_tasks: HashMap<String, EarlyToolTask>` side map（keyed by tool-call id）+ `early_for_plan` parallel array 里维护 speculative 任务。
- **`early_result` 字段为何 vestigial**：speculative early-start 原计划经 plan struct 串接 early task（turn_loop 设计），但 slice 15/40 的重接改用 side map + parallel array，struct 字段成残肢（两构造点恒 `None`：`host_executor.rs:4283` + tui `tests.rs:382`）。框架执行器自己另有 distinct `EarlyToolTask`（host_executor.rs:1272），不靠此字段。
- **prompt_zones shim 是过渡件**：Phase 6 §6b-2 加 `pub use codesmith_agent_runtime::prompt_zones::*;` glob 把 runtime 模块 public 项展平到 `crate::prompt_zones::` 路径，migration 期保 TUI 旧路径存活；后续步骤把 TUI 路径直接接到 runtime crate，shim 成死（grep 零 `crate::prompt_zones::` 引用 + build 确认）。

**落地步骤：**
1. `crates/agent-runtime/src/engine/turn_loop.rs`：删 `EarlyToolResult`（`{ result, elapsed }`）+ `pub struct EarlyToolTask`（`{ name, input, handle }`）两结构（11 行）；重写模块 doc（residual→两 live helper + 删除理由，注明结构经 slice 15/40 重接后删、字段随之）；drop `#![allow(dead_code)]`（模块现仅两 live helper，无需静音）。
2. `crates/agent-runtime/src/engine/dispatch.rs`：`ToolExecutionPlan` 删 `pub early_result: Option<super::turn_loop::EarlyToolTask>` 字段。
3. `crates/agent-runtime/src/engine/host_executor.rs`：plan 构造点（`:4283`）删 `early_result: None,`；`:4226` 注释更新（原 "struct's own `early_result` / `blocked_error` fields are left `None`" → "struct's own `blocked_error` field is left `None` + 框架执行器在自己 distinct `EarlyToolTask` 类型 + `early_tasks` map 维护 speculative early-start 任务，非在 plan 上"）。
4. `crates/tui/src/core/engine/tests.rs`：`make_plan_at` helper 删 `early_result: None,`（`:382`）。
5. `crates/tui/src/main.rs`：删 `mod prompt_zones;`（`:59`）；删 `crates/tui/src/prompt_zones.rs` shim（7 行）。

**测试：** 零测试改动——删的符号均死代码（字段恒 `None` 从未被读），无测试覆盖；`make_plan_at` helper 机械删字段（构造点对齐）。162 host_executor 测试 + 1 turn_loop 测试 + 126 tui engine 测试全过不改。

**验证：** `cargo build -p codesmith-agent-runtime` **零 warning**；`cargo test -p codesmith-agent-runtime --lib host_executor` **162 通过**；`cargo test -p codesmith-agent-runtime --lib turn_loop` **1 通过**；`cargo build -p codesmith-tui` 绿（**142 warning**，baseline 143，-1 自 prompt_zones shim 删除）；`cargo test -p codesmith-tui --bin codesmith-tui core::engine::tests` **126 通过 + 1 ignored**；`cargo build --workspace` 全绿。stash-pop baseline 对照（HEAD slice 46 不含本改动）：总测试数 **1151**（1148+1 flaky MCP+2 ignored）两侧一致——零测试被删；flaky MCP 测试既有文档化、隔离跑过。

**By-design gaps（deferred，documented）：**
- **`turn_loop` 模块仍非 §E1 `AgentExecutor` 抽取**：模块现仅两 live helper（`messages_with_turn_metadata` test-referenced + `subagent_completion_runtime_message` consumed by `HostAgentExecutor`），~~production `Engine`/`turn_loop` 迁移仍 deferred（§E1 "接真引擎"步）~~ → slice 48 复核 stale：§E1 production 迁移经 slice 20 cutover 完成（`HostAgentExecutor` 为 live production path、`handle_deepseek_turn` 已删），`turn_loop` 仅余 2 live helper 是迁移后残件、非 pending。本切片只删死结构，不动 live 代码。
- **tui 142 既有 warning 均死代码**：与 baseline 对齐（仅 -1 自本切片删的 shim），零新增命中 agent-runtime/dispatch/tui main。

**下一聚焦工作：**
- §E 死代码清完（`turn_loop` 死结构 + `early_result` 死字段 + `prompt_zones` 死 shim 全删）。~~§E1 production `Engine`/`turn_loop`→`AgentExecutor` 迁移仍 deferred（需 replay bridge）~~ → slice 48 复核 stale：§E1 production 迁移经 slice 20 cutover 完成（`HostAgentExecutor` 为 live production path、`handle_deepseek_turn` 已删）；"replay bridge" 实为 §A1 阻塞且已解除（rig compat 层原生序列化 `reasoning_content`），非 §E1 阻塞。
- §B3（cosmetic `DeepseekCN` 折叠）仍低优先。~~§A1（DeepSeekClient 抽取）仍低优先/deferred~~ → slice 48 复核 stale：§A1 经 retirement+rig 完成（非抽取；replay bridge 不必要），tui `client.rs`/`chat.rs` 已删（slice 41）。§E4 follow-up（env override augment / flash-kimi-code 变体下沉）按需另开切片。

---

**进度（2026-07-17 §A1/§E1 doc-debt cleanup slice 48——修正 "replay bridge deferred" stale 状态标记 + `handle_deepseek_turn remains live` / `DeepSeekClient` 死引用文档，`feat/pluggable-framework-core`）：**

接 slice 47（§E 死代码清完，`:2189`）。复查 slice 47 的 "下一聚焦工作" 列发现其依据已 stale：该列把 §A1（DeepSeekClient 抽取）与 §E1（production `Engine`/`turn_loop`→`AgentExecutor` 迁移）均标 "deferred（需 replay bridge）"，但经代码核实两者实际均已完成——"replay bridge" 阻塞属 §A1 且已解除（rig compat 层原生序列化 `reasoning_content`，2026-07-07 §A1 checkpoint 对照 rig-core 0.39.0 源码核实），§E1 production 迁移经 slice 20 §E cutover 完成（`HostAgentExecutor` 为 `handle_send_message` 的 live production path、`handle_deepseek_turn` 已删、`fn handle_deepseek_turn` 定义不存在）。本切片修正全部 stale 标记，纯文档、零行为改动（匹配 slice 32/42/47 的 cleanup-slice 惯例）。

**关键设计决策：**
- **strawman "replay bridge" 是 §A1 阻塞、非 §E1 阻塞**：slice 47 的 "下一聚焦工作" 把 "需 replay bridge" 括注附在 §E1 行后（`:2214`），但 replay bridge 从来是 §A1（DeepSeek `reasoning_content` 回放协议搬迁）的阻塞。2026-07-07 §A1 checkpoint 核实 rig 的 compat 层原生把 `AssistantContent::Reasoning` 序列化为 `reasoning_content`，故回放桥接在 adapter 层即完成——阻塞解除，§A1 改走 retirement（DeepSeek 切 rig + 删 `DeepSeekClient`）而非抽取。§E1（production Engine→AgentExecutor 迁移）从未依赖 replay bridge；它依赖 guardrail 逐步吸收，slice 11–19 全部吸收、slice 20 cutover 上线。
- **strkethrough-correct 而非改写历史条目**：slice 47 的 "下一聚焦工作" 与 "By-design gaps" 中 stale 的 §A1/§E1 断言用 `~~…~~ → slice 48 复核 stale：…` 收口（镜像 `:128` 的 `~~LlmClient trait 文档…~~ → §D2 已清理` 既有 pattern）。保历史记录（slice 47 当时认为 deferred）的同时就地标记纠正，避免下一读者被 "deferred — needs replay bridge" 误导。
- **§A/§E1 section 用 Status 追加、非改写原计划**：§A1（`:2226`）与 §E1（`:2398`）的 deferred-work catalog 描述 "搬运/抽取/接真引擎" 原计划，实际经 retirement/cutover 完成（路径不同）。镜像 §D2 的 `**Status (9d47942c + slice 46):**` 追加 pattern——原计划文本作历史记录保留，追加 Status 段说明实际完成路径。
- **ARCHITECTURE.md status table 是 current-status doc，直接改**：表行 277/278（tui-local `DeepSeekProviderFactory`/`DeepSeekClient::from_parts` 标 "✅ done"）描述 §D1 partial 期状态、§A1 后已退役；行 282（"seeds from default_registry for all non-DeepSeek"）描述 §D1 partial；行 285/288 标 deferred/in-progress。这些是 current-status 断言（非历史记录），直接改：retired 行改 "retired"（镜像行 283 `AnthropicClient retired` 的 template）、282 改 "all providers"、285/288 改 "done"。ASCII diagram（`:53`/`:77`）同步删 `DeepSeekProviderFactory`/`DeepSeekClient`。
- **代码 doc comment assert current state，直接改**：`host_executor.rs` 模块 doc（"carries the real turn loop (handle_deepseek_turn)…will absorb…eventually replacing"）+ `:238` "not yet wired into handle_send_message; handle_deepseek_turn remains the live path" + `dispatch.rs:4` "high-level ordering still lives in Engine::handle_deepseek_turn" + `mod.rs:2757` "TUI-concrete types (… DeepSeekClient …)" + `tui/tests/README.md:28` "refactored to take Arc<dyn LlmClient> instead of Option<DeepSeekClient>" 均断言已不成立的状态，直接改 past-tense / 删死引用。`handle_deepseek_turn` 的**历史 provenance 引用**（guardrail doc 中 "mirroring handle_deepseek_turn's X"）保留——它们说明迁移出处、不断言当前 live。
- **`tui/tests/README.md` 对齐 in-file comment**：`integration_mock_llm.rs:25-29` 的 in-file doc 已准确（engine holds `Option<LlmClientHandle>` = `Option<Arc<dyn LlmClient>>` since v0.8.48；tests remain ignored pending `EngineConfig`/`Config` harness），但 README 仍 stale（"unblock when Engine refactored to take Arc<dyn LlmClient> instead of Option<DeepSeekClient>"）。本切片把 README 改为对齐 in-file doc 的准确 framing——refactor 已落地、tests 仍 ignored 因 test-harness 缺口。

**落地步骤：**
1. `ARCHITECTURE.md` ASCII diagram：`:53` 删 "tui-local DeepSeekProviderFactory (wraps DeepSeekClient)" 行（status table 行 277/282 已显式说明 tui 不持 provider factory，diagram 无需重复）；`:77` "MockClient / RigLlmClient / DeepSeekClient / ..." → "MockClient / RigLlmClient / ..."。
2. `ARCHITECTURE.md` status table：行 277/278 改 retired（"TUI-local DeepSeekProviderFactory retired — rig DeepSeekFactory replaces it (§A1)" / "DeepSeekClient retired — rig RigLlmClient replaces it (§A1); from_parts deleted"，where 列指向 deleted 文件）；行 282 改 "seeds from default_registry for all providers (§D1 partial → §A1 full cutover)"；行 285 改 "✅ done (superseded — retired, not extracted; replay bridge found unnecessary...)"；行 288 改 "HostAgentExecutor is the live production path (slice 20 cutover)... production Engine migration done"。
3. `crates/agent-runtime/src/engine/host_executor.rs`：模块 doc（`:1-12`）改 past-tense（"replaced the retired handle_deepseek_turn in slice 20 cutover... handle_deepseek_turn is deleted"、"absorbed the production guardrails slice by slice"）；`:238-242` 改 "wired into handle_send_message (slice 20 §E cutover) and is the live production turn path; handle_deepseek_turn is deleted"。
4. `crates/agent-runtime/src/engine/dispatch.rs:4`："high-level ordering still lives in Engine::handle_deepseek_turn" → "now lives in HostAgentExecutor (slice 20 §E cutover — handle_deepseek_turn retired/deleted)"。
5. `crates/agent-runtime/src/engine/mod.rs:2757`："TUI-concrete types (Config, EngineHost, DeepSeekClient, …)" → "(Config, EngineHost, …)"。
6. `crates/tui/tests/README.md:26-29`：改对齐 in-file doc（refactor 已落地 v0.8.48、tests 仍 ignored pending `EngineConfig`/`Config` harness）。
7. `ROADMAP.md`：slice 46 "下一聚焦工作"（`:2181`/`:2185`）+ slice 47 "By-design gaps" `:2210` + "下一聚焦工作" `:2214`/`:2215` 共五处 stale §A1/§E1 "deferred" 断言 strkethrough-correct（`~~…~~ → slice 48 复核 stale：…`）；§A1 section + §E1 section 各追加 `**Status:**` 段说明实际完成路径。

**测试：** 零测试改动——纯文档（doc comment + markdown），无代码逻辑变更。doc comment 改动经 `cargo build` 核实仍编译。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` 零新 warning（doc comment 改动不触 warning）；`cargo +1.90.0 build -p codesmith-tui` 绿（README/dispatch/host_executor/mod 改动均为 doc，零代码影响）；`cargo +1.90.0 build --workspace` 全绿；grep 核实 `replay bridge` 命中仅剩 slice 48 entry + §A1 Status 段（说明已解除）+ slice 46/47 strikethrough 纠正注（历史），零 current-status 断言；`handle_deepseek_turn remains the live path` / `not yet wired into handle_send_message` 零命中；`DeepSeekClient` 命中仅剩 §A1 历史进度条目（2026-07-07 checkpoint，记录 retirement 史）+ §A1 Status 段（说明 retired）+ §A deferred-work catalog（原计划记录）。

**By-design gaps（deferred，documented）：**
- **§A/§E deferred-work catalog 原计划文本保留**：§A1-A4 / §E1-E4 的 deferred-work catalog 描述原 "extract" / "接真引擎" 计划，实际经 retirement/cutover 完成（路径不同）。原文本作历史记录保留，仅追加 Status 段——不改写原计划（保 "考虑过的方案" 记录）。§A2-A4（AnthropicClient / 共享 helper / per-client helper 去重）原计划亦经 retirement（AnthropicClient 删、slice 42 dedup）完成，Status 段仅在 §A1（retirement 起点）+ §E1（迁移起点）追加——§A2-A4 不逐条追加（§A1 Status 段已涵盖 "retirement 而非 extraction" 全局路径，§A2 同型）。
- **`handle_deepseek_turn` 历史 provenance 引用保留**：host_executor.rs 的 guardrail doc 中 "mirroring handle_deepseek_turn's X (turn_loop.rs:NNN)" 引用说明 guardrail 的迁移出处，不断言当前 live，保留（line refs 指向已删代码属已知 doc-debt，但 provenance value 高于 line 准确性）。
- **§B3 / §E4 follow-up 不变**：仍低优先 / 按需另开切片（slice 47 framing 不变，仅 §A1/§E1 从该列移除）。

**下一聚焦工作：**
- §A（provider extraction + 残件去重）+ §E（framework core traits + HostAgentExecutor cutover + 10 guardrails + §E4 manifest）主体至此**闭合并经文档核实**。pluggable framework core 迁移实质完成。
- 残项均为低优先 / by-design / 按需：§B3（cosmetic `DeepseekCN`→`Deepseek` 折叠，mitigated）、§E4 follow-up（env override augment / flash-kimi-code 变体下沉，按需）、~~`turn_loop` 模块仅余 2 live helper 的进一步收敛（可考虑并入 host_executor 或保留——非阻塞）~~ → slice 49 完成：`turn_loop.rs` 删除、2 live helper 并入 `host_executor`/`mod.rs`（模块收敛闭合，retired 代码原宿主文件不复存在）。

**进度（2026-07-17 §E slice 49 `turn_loop` 模块收敛——删 `turn_loop.rs` + 2 live helper（`messages_with_turn_metadata` / `subagent_completion_runtime_message`）并入 `mod.rs`/`host_executor.rs`，`feat/pluggable-framework-core`）：**

接 slice 48（§A1/§E1 doc-debt cleanup，`:2219`）。slice 48 的 "下一聚焦工作" 把 `turn_loop` 模块的进一步收敛列为非阻塞残项（"仅余 2 live helper，可考虑并入 host_executor 或保留"）。本切片执行该收敛——`turn_loop.rs` 是 retired `handle_deepseek_turn`（~2.4k 行，slice 20 §E cutover 删除）的原宿主文件，cutover 后仅余 2 live helper（80 行）。删该文件、把 2 helper 并入各自消费方模块，是 §E1 迁移的结构性收尾——retired 代码的原宿主文件不复存在。纯结构重构（文件内搬运 + 删除），零行为改动（匹配 slice 32/42/47/48 的 cleanup-slice 惯例）。

**关键设计决策：**
- **逐 helper 选定并入目标、非整体并入 host_executor**：`messages_with_turn_metadata` 是 `impl Engine` 的 session 访问器、6 处调用点全在 tui 测试（跨 crate 的 `Engine` 方法调用），并入 `engine/mod.rs` 主 `impl Engine` 块——路径无关的方法调用使 6 调用点零改动；`subagent_completion_runtime_message` 是 free fn、唯一生产调用点在 `host_executor.rs:4146`，并入 `host_executor.rs` 作 module-private `fn`（drop `pub(crate)`——同模块内单一消费者，surface 收缩）。两 helper 各自并入 "最近消费方" 而非整体并入 host_executor，匹配 Rust "就近定义" 惯例。
- **test 随 fn 走、保 1:1 搬运**：`subagent_completion_handoff_is_internal_user_message` 测试随 `subagent_completion_runtime_message` 搬入 `host_executor.rs` 的 `#[cfg(test)] mod tests`（"subagent post-stream completion drain" 节首，带 `// §E slice 49 — relocated from turn_loop.rs` 头注）。`messages_with_turn_metadata` 无专属单测（其 6 调用点本身就是 tui 测试的断言 fixture）。零测试逻辑改动。
- **~104 历史 provenance 引用保留不重写**：`host_executor.rs` / `engine/mod.rs` / `crates/tui/src/tools/subagent/mod.rs:29` / `crates/tool-impls/src/tools/plan_mode.rs:132` 等共约 104 处 `turn_loop.rs:NNN` / `turn_loop::item` 注释引用（"mirroring turn_loop.rs:NNN" 之类），均指向已删 `handle_deepseek_turn` 代码、属 slice 48 已建档 doc-debt（provenance value 高于 line 准确性）。删 `turn_loop.rs` 使 `turn_loop.rs:` 前缀指向不存在的文件，但注释仍传达 "镜像 retired handle_deepseek_turn 的 X 行为" 的 provenance；重写 ~104 条注释属超范围 churn，未来 doc-debt 切片可另做（未来读者可经 `git show <pre-retire-commit>:crates/agent-runtime/src/engine/turn_loop.rs` 查历史代码）。
- **ARCHITECTURE.md:189 仅 1-line 可读性修**：该段 broader prose（"What is not here yet: absorbing guardrails"）是 pre-slice-11 framing 的 stale doc-debt（guardrails 已吸收），但全段重写超范围；本切片仅把 "`Engine`/`turn_loop.rs` guardrails" → "`Engine` guardrails (formerly in the now-deleted `turn_loop.rs`)" 保可读，status table（行 277/278/282/285/288，slice 48 更新）仍为 authoritative current-status doc。

**落地步骤：**
1. `crates/agent-runtime/src/engine/host_executor.rs`：删 `:736` `use super::turn_loop::subagent_completion_runtime_message;`；在 `should_emit_thinking_only_status` 之后的 free-fn helper 簇插入 `fn subagent_completion_runtime_message(payload: &str) -> Message`（module-private，drop `pub(crate)`），doc comment 逐字保留 + 追加 relocation 注；`#[cfg(test)] mod tests` 的 "subagent post-stream completion drain" 节首插入 `subagent_completion_handoff_is_internal_user_message` 测试（带 `// §E slice 49 — relocated from turn_loop.rs` 头注）。调用点 `:4146` 字节不变、现解析为局部 fn。
2. `crates/agent-runtime/src/engine/mod.rs`：主 `impl Engine` 块（`:195`）首部插入 `pub fn messages_with_turn_metadata(&self) -> Vec<Message>`（保 `pub fn`——tui 跨 crate 调用），doc comment 逐字保留 + 追加 relocation 注；删 `:2930` `mod turn_loop;` 声明。
3. 删 `crates/agent-runtime/src/engine/turn_loop.rs`（整 80 行：17 行模块 doc + 2 live helper + 1 测试）。
4. `ARCHITECTURE.md:190`："`Engine`/`turn_loop.rs` guardrails" → "`Engine` guardrails (formerly in the now-deleted `turn_loop.rs`)"（1-line 可读性修，broader 段不重写）。
5. `ROADMAP.md`：slice 48 "下一聚焦工作"（`:2251`）的 `turn_loop` 残项 strikethrough-correct（`~~…~~ → slice 49 完成：…`，镜像 `:128` / slice 48 的既有 pattern）；§E1 section（`:2472`）Status 段已准确（不提 `turn_loop` 残项）、无需改；追加本 slice 49 进度条目。

**测试：** 零测试逻辑改动——1 测试随 fn 模块间搬运（`turn_loop::tests` → `host_executor::tests`），6 处 `messages_with_turn_metadata` 调用点零改动（`Engine` 方法、路径无关）。

**验证：** `cargo +1.90.0 build -p codesmith-agent-runtime` **零 warning**（删文件 + crate 内搬运，无新 surface）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib host_executor` **163 通过**（relocated 测试现归 host_executor；pre-relocation 162 + 1 搬入）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` **1149 通过 + 2 ignored，0 failed**（测试模块间搬移、非删除，总数 1151 不变）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui core::engine::tests` **126 通过 + 1 ignored**（6 `messages_with_turn_metadata` 调用点不变）；`cargo +1.90.0 build --workspace` 全绿（`codesmith-tui` 142 warning 为 slice 47 baseline、非本切片新增）；grep 核实 `mod turn_loop` / `use ...turn_loop` **零命中**（仅余 1 处 `//!` doc comment provenance 引用 `turn_loop::early_tool_start_safe`，属 slice 48 已建档 doc-debt）。

**By-design gaps（deferred，documented）：**
- **~104 历史 provenance 引用保留**：见上 "关键设计决策"。删 `turn_loop.rs` 扩展 staleness 但 slice 48 policy 覆盖（provenance > line 准确性）。~~未来 dedicated doc-debt 切片可重写这 ~104 条注释 + ARCHITECTURE.md:189 broader 段——非阻塞。~~ → slice 50 完成：~106 `turn_loop.rs:NNN`/`turn_loop::item` provenance 注释全部重写为 durable `handle_deepseek_turn` fn-name + one-time `git show ab4f4fc5` pointer，ARCHITECTURE.md:189 broader 段 reframed 为 "What is here (§E cutover done)"。
- **ARCHITECTURE.md:189 broader 段**：本切片仅修 `turn_loop.rs` mention 保可读，~~全段（pre-slice-11 "not here yet" framing）重写为单独 doc-debt。~~ → slice 50 完成：全段 reframed 为 "What is here (§E cutover done)"，header + L221-224 "remaining four guardrails... after which handle_deepseek_turn retires" + L236 "live handle_deepseek_turn still covers it" 三处 stale 一并修正。
- **§B3 / §E4 follow-up 不变**：仍低优先 / 按需另开切片（slice 48 framing 不变）。

**下一聚焦工作：**
- §A + §E 全部闭合并**结构性收敛**——`turn_loop` 残件清零、retired `handle_deepseek_turn` 原宿主文件已删，pluggable framework core 迁移的最后一个结构 loose end 收口。
- 残项仅余低优先 / by-design / 按需二项：§B3（cosmetic `DeepseekCN`→`Deepseek` 折叠，mitigated）、§E4 follow-up（env override augment / flash-kimi-code 变体下沉，按需）。
- ~~未来可选 dedicated doc-debt 切片：重写 ~104 `turn_loop.rs:NNN` provenance 注释 + ARCHITECTURE.md:189 broader 段——非阻塞。~~ → slice 50 完成：~106 provenance 注释 → durable `handle_deepseek_turn` fn-name + ARCHITECTURE.md reframed + latent `protocol_recovery.rs` compile-break 修复（slice 49 漏过——验证跑 `--lib` 非 `--test`）。

**进度（2026-07-17 §E slice 50 `turn_loop` 删除 doc-debt cleanup——重写 ~106 `turn_loop.rs:NNN`/`turn_loop::item` provenance 引用 → durable `handle_deepseek_turn` fn-name + 修 latent `protocol_recovery.rs` compile-break，`feat/pluggable-framework-core`）：**

接 slice 49（`turn_loop.rs` 删除 + 2 live helper 模块收敛，`:2253`）。slice 49 删 `turn_loop.rs` 后遗留 ~106 处 `turn_loop.rs:NNN`/`turn_loop::item` provenance 注释指向已删文件、ARCHITECTURE.md:189 stale "What is not here yet" 段、3 "is a later slice" framing 注释、3 stale path/caller refs，以及 1 处 latent compile-break（`protocol_recovery.rs:35` ungated `include_str!` of deleted `turn_loop.rs`——slice 49 验证跑 `--lib` 非 `--test protocol_recovery`，漏过）。本切片是 slice 49 "未来可选 dedicated doc-debt 切片" 预期的 cleanup 切片——重写全部 provenance 引用为 durable fn-name、修 compile-break、reframe stale doc。纯 doc + 1 test-source 行，零行为改动。

**关键设计决策：**
- **drop line numbers, use durable fn-name**：`turn_loop.rs:NNNN-NNNN` → `handle_deepseek_turn`；保 semantic label（spawn/reuse）若 present；drop redundant line-ref 当 `handle_deepseek_turn` 已在同注释内；simplify "retired"-qualified ref。
- **one-time `git show` pointer**：host_executor.rs module doc 加 `git show ab4f4fc5:crates/agent-runtime/src/engine/turn_loop.rs` pointer（verified ancestor of HEAD，3373-line full retired body）——给 provenance 指向已删代码一个 durable lookup 路径，免逐条注释加。
- **relocated-single-source framing**：`turn_loop::MAX_APPROVAL_INTENT_SUMMARY_CHARS`/`turn_loop::approval_intent_summary` 的 "mirrors/duplicated/later cleanup can lift" framing → "the `turn_loop::` original was deleted in slice 49 — this is now the single source"（host_executor.rs:814/821）。
- **ARCHITECTURE.md:189 broader 段 reframe**：header "not here yet: absorbing" → "What is here (§E cutover done): absorbed"；L221-224 "remaining four growing... after which handle_deepseek_turn retires" → "have since grown... retired in slice 20"；L236 "the live handle_deepseek_turn still covers it" → dropped（HostAgentExecutor 经 host_executor.rs:2658 明确 defer apply_patch path derivation，"still covers it" reassurance 不可验证/stale，drop 而非 over-claim "HostAgentExecutor covers it"）。status table（行 287）slice 48 已 authoritative-current，不动。

**Landing steps：**
1. `crates/agent-runtime/tests/protocol_recovery.rs:35`：删 `include_str!("../src/engine/turn_loop.rs")`（deleted file），替换为 `include_str!("../src/engine/host_executor.rs")`（restore coverage intent；经 `engine_source_file_still_exists_and_is_non_trivial` + `engine_marker_counts_stay_paired` 测试确认 marker pairing 仍 green）。
2. `crates/agent-runtime/src/engine/host_executor.rs`：module doc 加 one-time `git show` pointer；perl pass 重写 99 `turn_loop.rs:NNN` → `handle_deepseek_turn`；targeted Edits 修 5 `turn_loop::item` refs + 2 split refs 的 "mirroring the retired handle_deepseek_turn" 冗余（L1575/L6329）+ L1030。
3. `session.rs:255` + `engine/mod.rs:1187` + `session_history.rs:14-18` + `callback_bridge.rs:69-71` + `tools/framework_adapter.rs:12-13` + `tools/truncate.rs:25` + `tui/src/tools/subagent/mod.rs:29` + `tool-impls/src/tools/plan_mode.rs:132`：各 stale ref 重写（3 "is a later slice" framing → "done (slice 20 §E cutover)"；3 path/caller ref）。
4. `ARCHITECTURE.md:189-268`：header reframe + L221-224 + L236 stale refs 修正。
5. `ROADMAP.md`：slice 49 "未来可选 dedicated doc-debt 切片" note（`:2275`/`:2276`/`:2282`）strikethrough-correct；追加本 slice 50 进度条目。

**验证：** `cargo +1.90.0 build --workspace` 全绿（1m01s；`codesmith-tui` 142 warning 为 slice 47 baseline、非本切片新增）；`cargo +1.90.0 test -p codesmith-agent-runtime --test protocol_recovery --no-run` **编译通过**（11.97s——slice 49 漏过的 latent compile-break 已修，本切片关键验证）；`cargo +1.90.0 test -p codesmith-agent-runtime --test protocol_recovery` **9 通过、0 failed**（`engine_source_file_still_exists_and_is_non_trivial` + `engine_marker_counts_stay_paired` 确认 host_executor.rs swap 保 coverage intent）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` **1149 通过 + 2 ignored，0 failed**（13.90s，匹配 slice 49 baseline）；grep 核实 `turn_loop.rs:` **零命中**、`turn_loop::` 仅余 host_executor.rs:814/821 两处 intentional relocated-single-source framing（"the `turn_loop::` original was deleted in slice 49"），其余 `turn_loop` mention 均为 intentional（L7 git-show pointer + L891/L13657/mod.rs:203 slice-49 relocation notes + `integration_mock_llm.rs:152` test fn name `full_turn_loop_streams_text_chunks`）。

**By-design gaps（out of scope）：**
- **4 pure-provenance item name 不 rename**：`early_tool_start_safe`/`loop_guard_block_tool_result`/`MAX_APPROVAL_INTENT_SUMMARY_CHARS`/`approval_intent_summary`——前 3 删 `turn_loop.rs` 时被删、行为在 host_executor.rs 下 possibly-different name duplicate（`APPROVAL_INTENT_SUMMARY_MAX_CHARS` 等）。本切片仅重写 provenance doc、不 rename host_executor.rs items（renaming 是 behavior change 非 doc-debt）。`approval_intent_summary` 保名（已 1:1 搬运）。
- **§B3 / §E4 follow-up 不变**：仍低优先 / 按需另开切片。
- **ROADMAP.md 历史 slice 条目的 `turn_loop.rs:NNN` ref 不动**：slice 11-40 进度条目引用 `turn_loop.rs:NNN` 是历史记录（当时 live 文件的 provenance），非 stale pointer；本切片 grep 0-hit 验证仅 scope `*.rs`（不含 `.md` 历史条目，matching slice 48 policy）。

**下一聚焦工作：**
- §A + §E 全部闭合、**结构性收敛 + doc-debt 清零**——`turn_loop` provenance 引用全重写为 durable fn-name、retired 代码原宿主文件的所有 doc 残留收口。pluggable framework core 迁移的结构 + doc 双线收尾完成。
- 残项仅余低优先 / by-design / 按需二项：§B3（cosmetic `DeepseekCN`→`Deepseek` 折叠，mitigated）、§E4 follow-up（env override augment / flash-kimi-code 变体下沉，按需）。

**进度（2026-07-18 §B3/§E4 doc-debt cleanup slice 51——§B3 stale `chat.rs:80`/`chat.rs:1915` 引用 + §B3/§E4 Status 段补全 + ARCHITECTURE.md §E4 follow-up 显式化，`feat/pluggable-framework-core`）：**

接 slice 50（§E `turn_loop` 删除 doc-debt cleanup，`:2284`）。复查 §B3/§E4 deferred-work catalog 发现两者均缺 Status 段（不像 §A1/§D2/§E1 在 slice 48/50 已补），且 §B3 的 "two places" bullet 引用 `chat.rs:80` `apply_provider_token_limit`（XiaomiMimo）+ `chat.rs:1915` `provider_accepts_reasoning_content`（9-variant allowlist）——slice 41 已删 `crates/tui/src/client.rs` + `crates/tui/src/client/chat.rs`，branching 迁至 `crates/providers/src/rig_adapter/{shaper.rs:219, reasoning.rs:30}` 且 keyed on `&str`（非 `ApiProvider`）；§E4 catalog 描述 manifest 计划但未记 slices 43–45 落地路径 + 两 follow-up（env override augment / flash-kimi-code 变体下沉）当前状态——两 follow-up 仅在 ROADMAP 进度条目提及、源码无 `TODO`/`FIXME`、ARCHITECTURE.md status table 行 280 + narrative 段 254-259 均未提。本切片修正全部 doc-debt：§B3/§E4 各追加 Status 段、ARCHITECTURE.md §E4 行 280 + 段 254-259 显式化 follow-up。纯文档，零 `.rs` 改动，零行为改动（匹配 slice 48/50 的 doc-debt cleanup-slice 惯例）。

**关键设计决策：**
- **strkethrough-correct 不适用于 §B3 `chat.rs:NNN` 引用**：slice 48 的 `~~…~~ → slice N 复核 stale：…` pattern 应用于 prior slice 的 "下一聚焦工作" / "By-design gaps" 断言（progress 条目内），非 deferred-work catalog 段（§A1-A4 / §E1-E4 的原计划文本）。§B3 catalog 段是 "考虑过的方案" 历史记录，matching slice 48 "§A/§E deferred-work catalog 原计划文本保留" policy——不动原文本，仅追加 Status 段说明 `chat.rs:80`/`chat.rs:1915` stale + 当前实际 branching 路径。
- **§B3 Status 段不 over-claim "折叠已完成"**：decoupling goal 已完成（`crates/providers` 无 `codesmith-agent-runtime` dep edge + branching keyed on `&str`，非 `ProviderKind` switch——路径不同但 §C6 目标达成，`reasoning.rs:16-19` module doc 显式建档），但 `ApiProvider::DeepseekCN` 变体 cosmetic 折叠未完成——Status 段明确 deferred，scope 精准。
- **§E4 Status 段两 follow-up 精准定位**：env override augment = resolver chain `ConfigToml::resolve_runtime_options_with_secrets` `crates/config/src/lib.rs:1620-1787` 仍 fallback 硬编码 `DEFAULT_*` 常量（`:1650-1672`/`:1992-2032`）非 manifest + env 路径 `EnvRuntimeOverrides` `:2640-2746` 与 manifest 路径 `resolve_with_manifest_default` `crates/providers/src/lib.rs:206-217` 是两条 disjoint fallback chain + dedup/augment cross-layer-unreachable（§C6 layering）；flash/kimi-code 变体下沉 = 无 `Flash`/`KimiCode` enum variant / 无 manifest 条目 + 变体住 host-side `crates/config/src/lib.rs` 常量 + 选择逻辑（`DEFAULT_*_FLASH_MODEL` + `normalize_model_for_provider` flash-alias arms `:1884-1932` / `DEFAULT_KIMI_CODE_*` `:53-54` + `auth_mode_uses_kimi_oauth` `:2107-2116` / `moonshot_base_url_uses_kimi_code` `:2034-2039` in `Moonshot` arms `:1662-1668`/`:1722-1730`）。两 follow-up 均无 in-source `TODO`/`FIXME`——Status 段首次建档 in-catalog current-state。
- **ARCHITECTURE.md §E4 行 280 + 段 254-259 augment**：行 280 "done" silent on follow-ups → augment 为 "follow-ups (env override augment + flash/kimi-code variant sinking) deferred — tracked in ROADMAP §E4 (slice 51)"；段 254-259 描述 slices 43–45 落地，末尾追加 "Two follow-ups are deferred ..." 一句。§B3 行 285 "⏳ deferred — mitigated: reasoning is `&str`-keyed" 已准确，不动（matching slice 50 "行 287 authoritative-current 不动" pattern——已准确行不 churn）。
- **§E2/§E3 Status 段 gap 不补**：§E2（tool abstractions）/§E3（memory/callback abstractions）catalog 段亦缺 Status 段，但 framework-core traits landing 经 ARCHITECTURE.md 行 287（"framework-core traits landed (E1/E2/E3); `ToolSpec`→`Tool` adapter landed (§E); `Event`/`HookHost`→`Callback` bridge landed (§E); `Session`→`ChatHistory` bridge landed (§E)"）+ §E1 Status 段已涵盖。matching slice 48 "§A2-A4 不逐条追加" policy——§E1 Status 段为 framework-core traits 全局起点、§E2/§E3 同型、不逐条补。本切片 scope 仅 §B3/§E4（用户选定方向）。
- **slice 50 "下一聚焦工作" 不 strikethrough-correct**：slice 50 `:2308-2310` 列两残项（§B3 + §E4 follow-up）为 "低优先 / 按需"，slice 51 仅补 Status 段、未闭合两残项，framing 仍准确。无 stale 断言需 strikethrough-correct。

**落地步骤：**
1. `ROADMAP.md` §B3 catalog（`:2401-2413`）：在 `:2413` 末尾追加 `**Status (slice 51):**` 段（decoupling 已完成 via `&str`-keying 非 `ProviderKind` switch + `chat.rs:80`/`:1915` stale post-slice-41 + 当前 `crates/providers/src/rig_adapter/{shaper.rs:219, reasoning.rs:30}` keyed-on-`&str` 路径 + `ProviderKind` 已折叠 alias + 仅余 `DeepseekCN` 变体 cosmetic 折叠 deferred low-priority/mitigated + 窄 read-path regression 说明）。
2. `ROADMAP.md` §E4 catalog（`:2520-2524`）：在 `:2524` 末尾追加 `**Status (slices 43–45 + slice 51):**` 段（manifest 计划落地路径 + 两 follow-up 精准定位：env override augment + flash/kimi-code 变体下沉，均无 in-source `TODO`/`FIXME`）。
3. `ARCHITECTURE.md` status table 行 280：augment §E4 行的 Status 列，末尾追加 "follow-ups (env override augment + flash/kimi-code variant sinking) deferred — tracked in ROADMAP §E4 (slice 51)"。
4. `ARCHITECTURE.md` §E4 narrative 段（`:254-259`）：在 `:259` 末尾追加一句 "Two follow-ups are deferred (tracked in ROADMAP §E4, slice 51): the resolver chain still falls back to the hardcoded `DEFAULT_*` constants rather than the manifest (env override augment — cross-layer-unreachable per §C6), and flash/kimi-code model variants stay host-side (no manifest entry)."
5. `ROADMAP.md`：追加本 slice 51 进度条目（位于 slice 50 entry `:2284` 之后、`:2312` `---` separator 之前）。

**测试：** 零测试改动——纯文档（markdown + table），无 `.rs` 代码逻辑变更。

**验证：** `cargo +1.90.0 build --workspace` 全绿（零 `.rs` 改动，匹配 slice 50 baseline）；grep 核实 `chat.rs:80` / `chat.rs:1915` 仍命中 §B3 catalog `:2406-2408`（intentional，历史 "考虑过的方案" 文本，matching slice 48 catalog policy）+ 命中 slice 51 Status 段（说明 stale）；§B3 catalog `:2413` 后现 `**Status (slice 51):**` 段；§E4 catalog `:2524` 后现 `**Status (slices 43–45 + slice 51):**` 段；ARCHITECTURE.md 行 280 Status 列含 "follow-ups ... deferred — tracked in ROADMAP §E4 (slice 51)"；ARCHITECTURE.md 段 254-259 末尾含 "Two follow-ups are deferred ..." 句；§E4 follow-up 在源码仍无 `TODO`/`FIXME`（slice 51 Status 段为首次 in-catalog 建档，non-regression on source）；两 Status 段全部 17 条 file:line 引用逐条对照源码核实（13 条准确，3 条 off-by-N 已修正为 `EnvRuntimeOverrides` `:2640-2746`、flash-alias arms `:1884-1932`、Moonshot arms `:1662-1668`/`:1722-1730`，1 条本已精确），0 条内容失实。

**By-design gaps（out of scope）：**
- **§B3 cosmetic `DeepseekCN` 折叠本身仍 deferred**：本切片仅补 Status 段、不执行折叠。折叠有窄 read-path regression（手工编辑 `[providers.deepseek_cn]` 存储）+ `display_name` 失 "(legacy alias)" 后缀——future structural slice 可执行（matching slice 47/49 结构切片惯例，需 read-side fallback 缓解）。
- **§E4 两 follow-up 本身仍 deferred（按需）**：本切片仅补 Status 段、不执行 augment/sinking。env override augment 需 cross-layer refactor（config 读 manifest 或 manifest loader 下沉至 `codesmith-config`）；flash/kimi-code 变体下沉需新 manifest 字段/条目或 host-resolver 重构。两 follow-up 均按需另开切片。
- **§E2/§E3 Status 段 gap 不补**：见上 "关键设计决策"。future doc-debt 切片可补（非阻塞）。
- **slice 50 "下一聚焦工作" 不 strikethrough-correct**：见上 "关键设计决策"，framing 仍准确。

**下一聚焦工作：**
- §A + §E 全部闭合、结构性收敛 + doc-debt 清零（slice 50）+ §B3/§E4 catalog Status 段补全（slice 51）——pluggable framework core 迁移的结构 + doc + catalog-status 三线收尾完成。
- ~~残项 §B3 cosmetic `DeepseekCN`→`Deepseek` 折叠（mitigated，待 future structural slice）~~ → slice 52 完成：`DeepseekCN` 变体删除 + `deepseek-cn`/`deepseek_cn` 等 alias 折叠到 `Deepseek` + read-side `legacy_deepseek_cn_api_key()` fallback 落地，§B3 闭合。残项仅余 §E4 follow-up（env override augment / flash-kimi-code 变体下沉，按需另开切片）。

**进度（2026-07-18 §B3 slice 52 `ApiProvider::DeepseekCN` cosmetic fold + read-side fallback——变体删除 + alias 折叠 + `[providers.deepseek_cn]` read-only legacy sink，`feat/pluggable-framework-core`）：**

接 slice 51（§B3/§E4 doc-debt cleanup，`:2312`）。slice 51 Status 段明确 §B3 decoupling goal 已完成（`&str`-keyed 非 `ProviderKind` switch）、仅余 `ApiProvider::DeepseekCN` 变体 cosmetic 折叠 deferred（窄 read-path regression：手工编辑 `[providers.deepseek_cn]` 存储 + `display_name` 失 "(legacy alias)" 后缀）。本切片执行该折叠——mirror `ProviderKind`（`crates/config/src/lib.rs:76-79` serde aliases + `:138-139` `parse()` collapse `deepseek-cn` family → `Deepseek`）到 `ApiProvider`，并加 read-side fallback 缓解 documented regression (a)。§B3 闭合，pluggable framework core 迁移结构 + doc + catalog-status + 最后 cosmetic fold 四线全收尾。

**关键设计决策：**
- **Mirror `ProviderKind` alias pattern on `ApiProvider::Deepseek`**：删 `DeepseekCN` 变体（`config_types.rs:202`），在 `Deepseek` 加 `#[serde(alias = "deepseek-cn", alias = "deepseek_china", alias = "deepseekcn", alias = "deepseek-china", alias = "deepseek_cn")]`（末 alias 覆盖旧 snake_case rename → `ProviderCapability` JSON backward-compat）；`parse()` CN arm 折进 `"deepseek"` arm + 加 `"deepseek_cn"`（关闭 latent gap——serde 接受 `deepseek_cn` 但 `parse()` 之前拒绝）。
- **Read-side fallback mitigates regression (a)**：`providers.deepseek_cn: ProviderConfig` field 保留为 deprecated read-only legacy sink（serde 仍反序列化 `[providers.deepseek_cn]` table——无 silent drop）；新增 private helper `Config::legacy_deepseek_cn_api_key()`（`config.rs:1762`）返回 `providers.deepseek_cn.api_key`；两 api_key-read predicates（`has_api_key_for` `:4484` + `active_provider_has_config_api_key` `:4327`）在 `Deepseek` branch `providers.deepseek.api_key` miss 后 consult fallback。Surgical——精准覆盖 documented `api_key`-storage regression；`base_url`/`model` default-const arms 已解析到 identical values（`DEFAULT_DEEPSEEKCN_BASE_URL == DEFAULT_DEEPSEEK_BASE_URL`），无需 fallback。
- **`DEFAULT_DEEPSEEKCN_BASE_URL` const 删除**（`config.rs:116`，`== DEFAULT_DEEPSEEK_BASE_URL`）；2 arms repoint 到 `DEFAULT_DEEPSEEK_BASE_URL`。
- **Env override repoint**（`:3140`）：`DEEPSEEK_HTTP_HEADERS` 写 `providers.deepseek_cn`（for `DeepseekCN`）→ 折进 `Deepseek => &mut providers.deepseek` arm。此后 `deepseek_cn` field 无任何 code path 写入（pure legacy read sink）——匹配已有 TUI save flow（`save_api_key_for` 对两 DeepSeek variant 均 bail）。
- **35 grouped match arms mechanical fold**：`Deepseek | DeepseekCN =>` / `matches!(... Deepseek | DeepseekCN)` 跨 `config_types.rs` + `tui/config.rs`（~25 sites）+ `tui/{core/engine.rs, tui/provider_picker.rs, tui/app.rs, tui/ui.rs, main.rs, commands/balance.rs}` + `provider_config_for`（separate CN arm 删除——`Deepseek` 返回 `&providers.deepseek`，fallback 在 key-read path per decision 2）。
- **`codesmith-providers` `&str`-keyed allowlists `"deepseek-cn"` arms 保留**（`reasoning.rs:34/100/133/180`）：defensive——接受 input strings，保留 alias accepted 正确（其他 path 可能仍传 hyphen string）。Out of scope（matching slice 48/50 provenance-preserves policy）。
- **`display_name()` 失 "(legacy alias)" 后缀**（`config_types.rs:279` 删 CN arm）：`Deepseek` display 仅 `"DeepSeek"`。documented regression (b)，接受。
- **`config.example.toml:22,25` 不动**：`deepseek-cn` 仍 parse（to `Deepseek` now），comments 仍准确。

**落地步骤：**
1. `crates/agent-runtime/src/config_types.rs`：`:202` 删 `DeepseekCN,`；`:201` `Deepseek` 加 serde aliases；`:223-224` fold CN arm 进 `"deepseek"` arm + 加 `deepseek_cn`；`:255` 删 `as_str()` CN arm（仅 emit `"deepseek"`）；`:279` 删 `display_name()` CN arm；`:452`/`:493` grouped arms fold（删 `| ApiProvider::DeepseekCN`）。
2. `crates/tui/src/config.rs`：`:116` 删 `DEFAULT_DEEPSEEKCN_BASE_URL`；`:1364-1370` 保留 `deepseek_cn` field + deprecation comment；`:1762` 加 `legacy_deepseek_cn_api_key()` helper；`:1708` repoint `api.deepseeki.com` sniff 到 `Deepseek`；`:1738` 删 `provider_config_for` CN arm；`:4323`/`:4480` 加 fallback（comment + call at `:4327`/`:4484`）；`:1931`/`:3632` repoint const；`:3140` repoint env override；~25 grouped arms fold。
3. 其他 TUI files mechanical fold：`core/engine.rs:253`、`tui/provider_picker.rs:99`、`tui/app.rs:1703`、`tui/ui.rs`（6 sites）、`main.rs`（3 sites）、`commands/balance.rs:17`。
4. Tests（`crates/tui/`）：rewrite `config.rs:8435` → `has_api_key_for_deepseek_falls_back_to_legacy_deepseek_cn_table`（assert `Deepseek` 经新 fallback 在 `providers.deepseek_cn` 找到 key）；`:8444` 删 `DeepseekCN` assert；`:8561` rename `save_api_key_for_deepseek_cn_uses_root_deepseek_storage` → `…deepseek_uses_root_storage`，`DeepseekCN` → `Deepseek`；`:8954` 删 `DeepseekCN` `is_available_for` assert；`ui/tests.rs:6716` `DeepseekCN` → `Deepseek`；`main.rs:6414` `target.provider` assert `"deepseek-cn"` → `"deepseek"`；新增 `config.rs:8973` `api_provider_accepts_legacy_deepseek_cn_aliases`（mirror `provider_kind_accepts_legacy_deepseek_cn_aliases` `config/lib.rs:3747`——parse() 7 forms incl. `deep-seek` typo-tolerant + serde 6 forms excl. `deep-seek` + canonical serialization `"deepseek"`）。
5. `ROADMAP.md`：追加本 slice 52 进度条目；§B3 catalog Status 段（`:2448`）augment `**Status (slice 52):**` 段；slice 51 "下一聚焦工作"（`:2343`）strikethrough-correct §B3 residual。
6. `ARCHITECTURE.md`：§B3 status table 行 289 `⏳ deferred — mitigated: reasoning is &str-keyed` → `✅ done — DeepseekCN folded onto Deepseek (slice 52); &str-keying was the §C6 decoupling path`。

**测试：** `cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui` 2844 passed + 2 ignored（baseline slice 51: 2843 + 1 new test `api_provider_accepts_legacy_deepseek_cn_aliases`，net +1，原 1 failing 已修——parse/serde 容错 form 集差异，split loops）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1149 passed + 2 ignored（config_types tests pass）；`cargo +1.90.0 test -p codesmith-config --lib` 95 passed（`provider_kind_accepts_legacy_deepseek_cn_aliases` mirror regression green）。

**验证：** `cargo +1.90.0 build --workspace` 全绿；grep `DeepseekCN` 跨 `*.rs` → 仅 4 hit（slice-52 provenance comments in `config_types.rs:201`/`config.rs:1364`/`:8456`/`:8974`，documenting the fold——variant 本身已删）；grep `DEFAULT_DEEPSEEKCN_BASE_URL` → 仅 1 hit（`config.rs:1760` doc comment referencing its deletion，const 已删）；grep `deepseek_cn` field on `ProvidersConfig` → present at `:1370` w/ deprecation comment + `legacy_deepseek_cn_api_key()` helper at `:1762` + 2 fallback call sites `:4327`/`:4484`；grep `"deepseek-cn"` arms in `reasoning.rs` → retained at `:34`/`:100`/`:133`/`:180`（defensive，decision 6）。

**By-design gaps（out of scope）：**
- **`base_url`/`model`/`http_headers` in legacy `[providers.deepseek_cn]` 不被 surgical api_key fallback 覆盖**：`base_url` default == CN default（无 observable diff）；`model` defaults `DEFAULT_TEXT_MODEL`（两者同）；`http_headers` in `deepseek_cn` 是 undocumented 窄 edge。fallback 精准覆盖 documented `api_key`-storage regression。
- **`codesmith-providers` `"deepseek-cn"` `&str` arms 保留**（decision 6）——defensive，future cleanup slice optional。
- **`config.example.toml` 不 trim**——`deepseek-cn` 仍 parse accurately。
- **§E4 两 follow-up 不变**——仍 deferred（env override augment / flash-kimi-code sinking），按需。

**下一聚焦工作：**
- §B3 闭合。pluggable framework core 迁移的结构（§A/§E）+ doc + catalog-status + 最后 cosmetic fold（§B3 slice 52）四线全收尾完成。
- 残项仅余 §E4 两 follow-up（env override augment / flash-kimi-code 变体下沉，按需另开切片）——on-demand，非阻塞。

**进度（2026-07-18 §E slice 53 stale-absorbed doc-debt cleanup——narrative/inline 仍标 `deferred` 的已 absorbed 项对齐模块文档 + steer mutex std→tokio 同步 + §E Known-gaps 覆盖指针段，`feat/pluggable-framework-core`）：**

接 slice 52（§B3 cosmetic fold，`:2345`）。本会话先做 doc-vs-code gap audit（4 并行审查 agent + 关键发现人工复核），发现 §E narrative + scattered inline 注释存在 stale-absorbed 漂移：slices 25a/25b/25c（compaction）/ slice 33（CapacityController）/ cancel-token Checkpoint B/C/D wiring / steer→`tokio::sync::Mutex` 迁移（为 subagent blocking-hold `biased select!` 跨 `recv().await`）均已落地于 `host_executor.rs` 模块文档（`✅ absorbed`），但 ARCHITECTURE.md 对应 narrative 段 + 5 处 inline 注释仍写 `deferred to wire-in`。本切片纯 doc + 注释对齐模块文档，零行为改动（matching slice 50/51/52 doc-debt cleanup 惯例）。

**关键设计决策：**
- **Stale-absorbed = 模块文档权威**：`host_executor.rs` 模块文档（`//!` lines `128-130`/`410-416`/`444`/`459`/`491`/`572-586`/`609-615`/`659-671`）是 §E 各 guardrail 吸收状态的权威源（带 `✅ absorbed (slice N §E)` 标注）；narrative + inline 注释滞后于模块文档。本切片对齐 narrative/inline 到模块文档，不动模块文档（已是权威）。
- **steer `tokio::sync::Mutex` 三处同步 + "first" framing 修正**：`host_executor.rs:1491`/`:1734` steer 字段实为 `tokio::sync::Mutex`（模块文档 `:665-666` + 字段 doc `:1482-1486` 已建档——为 subagent blocking-hold `biased select!` 跨 `recv().await`，与 approval 同理），但 ARCHITECTURE.md 三处（`:167-168` narrative §1 / `:207-208` narrative §2 / `:291` status table）仍写 `std::sync::Mutex`，且 `:174`/`:213` "approval is the **first** `tokio::sync::Mutex`" 框定过时（steer 现亦 tokio）。同步三处 + drop "first" + `:228` "steer follows the same pattern"（imply std）修正。
- **5 inline 注释对齐模块文档**：`host_executor.rs` 5 处 inline 注释与同文件模块文档自相矛盾（模块文档标 `✅ absorbed`，inline 仍写 `deferred`）：`:3003` `post_compact_cleanup` (25c) 从 "Still deferred" 列表移除（模块文档 `:491` 标 absorbed ✅ slice 25c）；`:2270` ToolCallStarted `deferred` → `absorbed ✅`（模块文档 `:609-615`）；`:13724` blocking hold test comment `deferred` → `absorbed ✅`（模块文档 `:659-671`）；`:6335` cancel-token test comment `deferred` → `absorbed ✅`（模块文档 `:410-416`）；`:3300` CapacityController "off by default since v0.8.11 — deferred" → "absorbed ✅ slice 33 §E, but off by default since v0.8.11"（模块文档 `:572-586`；"deferred" 误导——是 opt-in 非 unimplemented）。另修 `:1498` approval 字段 doc "Unlike the steer/LSP fields (which use `std::sync::Mutex`)" → "Unlike the LSP field (which uses `std::sync::Mutex`)"（steer 现亦 tokio）。
- **§E Known-gaps 覆盖用指针段而非逐条转录**：模块文档 `:323-698` 自建 9 个 "Known gaps (by design)" 区（LSP flush / system-prompt refresh / thinking-only / transparent-retry / approval / compaction / capacity / early-tool-start / subagent），ARCHITECTURE.md §E narrative 仅 inline 复现 4 个（LSP flush / transparent-retry / approval / compaction），余 5 个无架构级可见性。本切片在 §E narrative 末尾（`:272` callback_bridge tests 句后）加 "Known gaps coverage" 指针段，列出 9 区 + 指明 narrative 详述 4 个、余 5 个见模块文档——避免逐条转录造成新一轮 line-drift（matching slice 51 "Status 段引用源码而非 inline" 策略）。
- **无 status table 行改动**：status table 行（`:278-291`）描述 wired-today 项（✅ done），本切片修的是 narrative + 模块文档对齐 + inline 注释，非 status 变更——matching slice 50 "行 287 authoritative-current 不动" pattern。仅 `:291` 行内 steer/approval mutex 描述随 E5 同步修正（仍属该行已有内容，非新增 status）。
- **无 strikethrough-correct**：slice 52 "下一聚焦工作"（`:2377-2379`）仅列 §E4 两 follow-up 为残项；本切片修的是 gap-audit 新发现的 stale-absorbed 漂移，非推翻 prior forward-looking claim，无需 strikethrough-correct。

**落地步骤：**
1. `ARCHITECTURE.md`：`:167-171` steer narrative §1（std→tokio + LSP/steer 拆分）；`:174` drop "first"；`:207-209` steer narrative §2（std→tokio）；`:213-214` drop "first" + steer note；`:228` "same pattern" 修正；`:220-222` compaction 25a/b/c 三项从 deferred → absorbed ✅；`:249-251` cancel-token deferred → absorbed ✅；`:291` status table steer/approval mutex 描述同步；`:272` 后加 §E Known-gaps coverage 指针段。
2. `crates/agent-runtime/src/engine/host_executor.rs`：`:1498-1500` approval 字段 doc "steer/LSP fields" → "LSP field"；`:3003-3005` `post_compact_cleanup` (25c) 移出 Still deferred 列表；`:2269-2270` ToolCallStarted deferred→absorbed ✅；`:13723-13725` blocking hold test comment deferred→absorbed ✅；`:6335-6336` cancel-token test comment deferred→absorbed ✅；`:3299-3300` CapacityController "deferred" → "absorbed ✅ slice 33 §E, off by default"。
3. `ROADMAP.md`：追加本 slice 53 进度条目（slice 52 entry `:2379` 后、`:2381` `---` separator 前）。

**测试：** `cargo +1.90.0 build --workspace` 全绿（零 `.rs` 行为改动——仅注释 + `.md`；`.md` 不影响 build）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 1149 passed + 2 ignored（baseline slice 52；`:13724` & `:6335` 编辑在 `#[tokio::test]` 注释行内，无行为变化）。

**验证：** grep `std::sync::Mutex<mpsc::Receiver<String>>` 跨 `ARCHITECTURE.md` → 零命中（steer 三处已修）；grep `deferred to wire-in` 跨 `ARCHITECTURE.md` → 仅余 2 合法项（`:221` enhancements + working-set pins）；grep `post_compact_cleanup.*Still deferred|Still deferred.*post_compact_cleanup` 跨 `host_executor.rs` → 零命中；grep `ToolCallStarted.*deferred|deferred.*ToolCallStarted` → 零命中；grep `blocking hold.*deferred|deferred.*blocking hold` → 零命中；grep `Cancel-token short-circuit is deferred` → 零命中；grep `Known gaps (by design)` 跨 `host_executor.rs` → 9 section headers（sanity：指针段 "nine areas" 断言核实）；grep `Known gaps coverage` 跨 `ARCHITECTURE.md` → 1 hit（指针段已加）。

**By-design gaps（out of scope）：**
- **P2 doc drift 推迟 slice 54**：`docs/ARCHITECTURE.md`（pre-refactor 2026-06-21，superseded-pointer 或 full rewrite）、`docs/PROVIDERS.md:241`（删 `client.rs` 引用）、`crates/agent-runtime/src/prompts/modes/coordinator.md:89,95,97,101`（system prompt example 用 `turn_loop.rs`/`handle_turn`——**behavior-adjacent**，单独 scoped slice）、`crates/agent-runtime/assets/skills/v4-best-practices/SKILL.md:46`（example 路径）、`crates/tui/.codesmith/instructions.md:24-25`（auto-regenerate）、`docs/rfcs/claude-code-architecture-parity.md`（borderline RFC）。本切片 scope 仅 P1 stale-absorbed + steer mutex + inline 矛盾 + §E 指针段。
- **§E4 两 follow-up 不变**——仍 deferred（env override augment / flash-kimi-code sinking），按需。

**下一聚焦工作：**
- §E stale-absorbed doc-drift 清零（本切片）。pluggable framework core 迁移的结构 + doc + catalog-status + cosmetic fold（slice 52）+ §E stale-absorbed 对齐（slice 53）五线收尾。
- 残项：P2 doc drift（推迟 slice 54，含 behavior-adjacent `coordinator.md`）+ §E4 两 follow-up（按需）——均 on-demand / 非阻塞。

**进度（2026-07-21 §F slice 1 extension system foundational core——pi-mono Extension 模型 port 的 slice 1：核心 traits + 新 crate codesmith-extensions + agent-runtime adapter + tui host wiring + sample + docs，`feat/pluggable-framework-core`）：**

接 slice 53（§E stale-absorbed doc-debt cleanup）。本切片开新 ROADMAP §F section，落地 pi-mono extension 模型的 found core（phase 1 静态加载）——spec `docs/superpowers/specs/2026-07-21-codesmith-extension-system-design.md` §10.1 scope。镜像 §E 三层模式：traits in codesmith-agent、runtime in codesmith-extensions（新 crate）、adapters in codesmith-agent-runtime、host wiring in codesmith-tui。plan：`docs/superpowers/plans/2026-07-21-codesmith-extension-system-slice-1.md`。

**关键设计决策：**
- **§11 open questions 本切片定**：(1) sample = `scratchpad`（tool + command + handler，验证三个 contribution point）；(2) `ExtensionCommandContext: ExtensionContext` sub-trait，slice 1 零 session-mutation 方法（split 为 type-safety + §F2 growth）；(3) 单个 `Handler` trait（observer-only，`async fn handle(event, ctx) -> Result<(), ExtensionError>`），per-variant subscription + `HandlerOutcome`（cancel/transform/block）defer §F2；(4) §10.3 vs §10.2 tension → §10.2 authoritative（observer-only；catch_unwind 真实隔离 §F2）。
- **`#[async_trait]` 引入 codesmith-agent**：既有 Tool/Callback/AgentExecutor 用 manual `Pin<Box<dyn Future>>`；extension traits 面向 extension author（外部 crate），`#[async_trait]` 显著友好，匹配 spec literal + ToolSpec/HookSink 惯例。代价：codesmith-agent +2 deps（async-trait + tokio-util）。
- **ExtensionToolSpecAdapter 镜像 ToolSpecAdapter**：held `Arc<dyn ToolDefinition>` + `Arc<dyn ExtensionContext>`，`execute` 委托 `ToolDefinition::execute(input, &*ctx)`；`input_schema()` 强制 object-rooted（`build_tool` fail-closed chokepoint 要求）。
- **HostAgentExecutor seam wiring 四点**（`host_executor.rs` 实测行号）：TurnStart `:3736`（user-msg push 后）、ToolCall `:4390`/`:4496`（on_tool_start 旁）、ToolResult `:4478`/`:4591`（on_tool_end 旁）、TurnEnd `:4268`（NoToolCalls）+ `:3784`（Checkpoint A Interrupted）。其余 terminal sites + step-end 是 §F2 hardening。
- **`ExtensionStateStore` 镜像 `SkillStateStore` verbatim**：TOML + atomic write + malformed→default + BTreeSet；加 `installed` field（§F5 provenance forward-compat）。
- **`build_extension_runtime` 四步**：discover_static → reconcile w/ state → load+configure（stub api）→ bind_core（HostExtensionContext）。reload 的 re-discover 是 §F2（slice 1 仅 build-time）。async `Extension::configure` 在 fresh `current_thread` runtime on a plain OS thread 驱动（`std::thread::scope`）——`block_in_place` 仅在 multi-thread runtime 有效（TUI `#[tokio::main]` 满足，但 `#[tokio::test]` 默认 `current_thread` 会 panic），nested runtime drop 也会 panic；OS-thread 方案两场景皆 safe。
- **`/extension install`/`uninstall` stub "phase 2"**：slice 1 静态无法 runtime install（by definition）；install-source 抽象仅 trait（impl §F5）。
- **`App` 字段 wiring**：slice 1 用 `app.extension_runner: Option<Arc<ExtensionRunner>>`（build_engine 构造、EngineHandle 回传、ui.rs copy 回 app）+ `app.extension_state: ExtensionStateStore`（App::new load_default）；live `/extension` state on App 直接 wiring 是 §F2（当 live reload 需要）。

**落地步骤：**
1. `crates/agent/Cargo.toml` + `src/lib.rs` + `src/extension.rs`（NEW）：8 traits + `ExtensionError`/`ExtensionMetadata`/`ExtensionEvent` minimal set + 5 test。
2. `Cargo.toml`（root workspace.members）+ `crates/extensions/{Cargo.toml,src/lib.rs + 6 sub-mod}`：新 crate。
3. `crates/extensions/src/{runner,api,bus,state,discovery,install_source}.rs`：runtime + stub/real api + bus skeleton + host context + discovery + install traits。
4. `crates/agent-runtime/{Cargo.toml,src/tools/mod.rs,src/tools/extension.rs}`（NEW）：adapter + dep + module。
5. `crates/agent-runtime/src/engine/{mod.rs,host_executor.rs}`：`extension: Option<Arc<ExtensionRunner>>` Engine field + `new_runtime` param + per-turn `.with_extension_runner` chain（`mod.rs:1306`）+ `with_extension_runner` builder（`host_executor.rs:1838`）+ 6 seam emits（`host_executor.rs:3736/3784/4268/4390/4478/4496/4591`）。
6. `crates/tui/src/extension_state.rs`（NEW）+ `commands/{mod.rs,extension_commands.rs}`（NEW）+ `tui/app.rs`（fields）：state store + command group + execute() tier + App fields。
7. `crates/tui/src/core/engine.rs`：`build_extension_runtime()` + wire into `build_engine`（Engine 传参 + EngineHandle 回传）+ `crates/tui/src/tui/ui.rs`（copy handle→app）。
8. `crates/extensions/src/sample_scratchpad.rs`（NEW）：sample + `inventory::submit!`。
9. `ROADMAP.md` + `ARCHITECTURE.md` + `docs/EXTENSIONS.md`（NEW）：§F section + §F1 entry + dev guide。

**测试：** `cargo +1.90.0 build --workspace` 绿（142 tui warning = slice 47 baseline 非新增）；`cargo +1.90.0 test -p codesmith-extensions --lib` 绿（8 pass，新 crate：6 runner/api/discovery + 2 sample）；`cargo +1.90.0 test -p codesmith-agent --lib` 绿（extension::tests 5 pass）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 绿（1152 pass + 2 ignored，baseline 1149→1152，+3 adapter/round-trip test）；`cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui` 绿（2853 pass + 2 ignored，baseline 2844→2853，+9 extension_commands tests + smoke test 现 exercise /extension）。

**验证：** grep `pub trait Extension ` 跨 `crates/agent/src/extension.rs` → 1 hit；grep `ExtensionToolSpecAdapter` 跨 `crates/agent-runtime/src/` → def + test refs；grep `with_extension_runner` 跨 `host_executor.rs` → 1 hit；grep `build_extension_runtime` 跨 `crates/tui/src/` → 1 def（engine.rs）+ 1 call（engine.rs build_engine）；grep `discover_static` 跨 `crates/` → 1 def（discovery.rs）+ sample（sample_scratchpad.rs）+ call（engine.rs）；grep `extensions_state.toml` 跨 `crates/tui/src/extension_state.rs` → path hit；`/extension list` 在 tui 运行报 `scratchpad`（sample registered via inventory）。

**By-design gaps（out of scope, §F2–§F8）：**
- **完整事件集**（~25 更多变体 + cancel/transform/block 链）——§F2。slice 1 Handler observer-only。
- **catch_unwind 真实隔离**——§F2（slice 1 emit 直接 await；panic 会传播——documented in runner.rs `emit` doc）。
- **EventBus 完整 impl**——§F3（slice 1 skeleton，subscribe/publish 返回 Unimplemented）。
- **registerProvider**——§F4。
- **Dylib 加载 + install/uninstall 真实现 + install-source impl + trust prompt + extension.toml manifest**——§F5。
- **Renderer / Shortcut / Flag**——§F6/§F7。
- **嵌入 API**——§F8。
- **Hot-load**——永不（spec §2.4）。
- **Host executor 端到端 round-trip test**（mock client + assert seen == [ToolCall, ToolResult, TurnEnd]）——§F2（slice 1 land compile-time seam wiring + isolated round-trip test on the runner itself）。

**下一聚焦工作：**
- §F2：完整事件集（~25 变体）+ cancel/transform/block 链 + per-variant Handler subscription + catch_unwind 真实隔离 + Host executor 端到端 round-trip test + App 字段 live wiring + reload re-discover。
- P2 doc drift（推迟 slice 54，含 behavior-adjacent `coordinator.md`）+ §E4 两 follow-up（按需）——均 on-demand / 非阻塞，承接自 slice 53 残项。
- §F3-F8 按需。

**进度（2026-07-22 §F2a extension system contract + runtime core——§F2 拆为两子切片的 a 半：完整 23-变体 `ExtensionEvent` 集 + `HandlerOutcome` cancel/transform/block 链 + per-variant `on_variant` subscription + `catch_unwind` 隔离 + 7 处 `host_executor` emit 签名机械更新，`feat/pluggable-framework-core`）：**

接 §F slice 1。本切片落地 §F2 的 contract + runtime core 半（§F2a），把 §F1 的 observer-only `Handler`（`Result<(), _>`）升级为 outcome-链模型（`Result<HandlerOutcome, _>`），并补齐 pi-mono spec §10.2 的完整 23-变体事件集。host seam wiring（§F2b）显式 out-of-scope——让 hard-to-reverse contract 先稳定。plan：`docs/superpowers/plans/2026-07-22-codesmith-extension-system-slice-2a.md`。

**关键设计决策：**
- **handler trait shape = Approach A**：单个 dyn-safe `Handler`（`Arc<dyn Handler>` 不变），返回 `Result<HandlerOutcome, ExtensionError>`；per-variant 经 `kind_filter` 而非 per-variant trait——最小 delta、匹配 pi-mono single-handler/union-return 模型、保持 object-safe。
- **`HandlerOutcome` flat enum**：`Continue`/`Cancel { reason }`/`Block { reason }`/`Transform(ExtensionEvent)`；variant-specific 语义由 host 运行时强制（§F2b）而非类型系统；terminal `EmitOutcome.outcome` 永不为 `Transform`（fold 进 `event`）。
- **`emit` owned-in / `EmitOutcome`-out、链式、registration-order**：`Transform` 对下一 handler 可见，`Cancel`/`Block` short-circuit；§F2a 不 `#[must_use]` `EmitOutcome`（7 处 host_executor 机械更新仅 drop `&`），§F2b 可加 `#[must_use]` 强制 seam 检查。
- **per-variant = 单一有序 `Vec<RegisteredHandler{handler, kind_filter}>`**：全局注册顺序保留，dispatch 前 filter `kind_filter.is_none() || == event.kind()`。
- **`catch_unwind` 经 `futures-util`**（匹配 `codesmith-agent-runtime` 版本 `0.3.31`）：panic + handler `Err` 均 `tracing::error!` 记录 + 链继续（§8.3 best-effort）。
- **§F2a/§F2b 拆分**：contract 先 land + 稳定，再 wire host。

**落地步骤：**
1. `crates/agent/src/extension.rs`：T1 reason enums（`TrustReason`/`DiscoverReason`）+ 6 payload structs（`InputEvent`/`AgentStartEvent`/`BeforeProviderRequestEvent`/`AfterProviderResponseEvent`/`ToolExecutionUpdateEvent`）；T2 `ExtensionEvent` 6→23 变体 + `ExtensionEventKind` discriminant + `kind()` exhaustive guard；T3 `HandlerOutcome` enum；T4 `Handler::handle` 返回 `Result<HandlerOutcome>` + 5 in-tree handler → `Continue`；T6 `ExtensionApi::on_variant` trait 方法。
2. `crates/extensions/Cargo.toml`：T5 `futures-util = "0.3.31"` dep。
3. `crates/extensions/src/runner.rs`：T6 `PendingHandler`/`RegisteredHandler` `kind_filter`；T7 `handlers: Vec<RegisteredHandler>` + `bind_core` drain carry `kind_filter`；T8 `EmitOutcome` struct + `emit` 重写（owned-in、per-variant filter、`catch_unwind`、transform 链、cancel/block short-circuit）+ 5 runner tests。
4. `crates/extensions/src/api.rs`：T6 `Stub`/`Real` `on`/`on_variant` impl push `kind_filter` + `f2a_stub_on_variant_queues_with_kind_filter` test。
5. `crates/extensions/src/lib.rs`：T8 re-export `EmitOutcome`。
6. `crates/agent-runtime/src/engine/host_executor.rs`：T8 7 处 emit 站点机械 drop `&`（`.emit(&codesmith_agent::extension::ExtensionEvent` → `.emit(codesmith_agent::extension::ExtensionEvent`；TurnStart `:3736`/TurnEnd-Interrupted `:3784`/TurnEnd-NoToolCalls `:4268`/ToolCall-parallel `:4390`/ToolResult-parallel `:4478`/ToolCall-serial `:4496`/ToolResult-serial `:4591`；§F2a 丢弃 `EmitOutcome`，§F2b 加 seam 检查）。

**测试/验证：** `cargo +1.90.0 build --workspace` 全绿；`codesmith-extensions --lib` 9→14（+5 `f2a_*`：emit chains transform to next handler / cancel short-circuits / block short-circuits / on_variant dispatches only matching kind / catch_unwind isolates panicking handler）；`codesmith-agent --lib` 93→97（+4 `f2a_*` contract：payload structs construct / event_kind round-trip 23 变体 / handler_outcome constructs each variant / handler_handle returns Continue by default）；`codesmith-agent-runtime --lib` 1152 passed + 2 ignored（7-site 签名变更 behavior-preserving，round-trip `extension_runner_bound_emits_lifecycle_events_on_minimal_run` 仍绿，seen == [TurnStart, ToolCall, ToolResult, TurnEnd]）；grep `.emit(&codesmith_agent::extension::ExtensionEvent` 跨 `host_executor.rs` → 0-hit；grep `.emit(codesmith_agent::extension::ExtensionEvent` → 7-hit。

**By-design gaps（§F2b，显式 out-of-scope）：**
- host_executor 7 seam 的 cancel/block/transform *handling*（当前丢弃 `EmitOutcome`）。
- ~17 新事件尚未由 `host_executor` 发射（仅 §F1 的 6 个 live：`SessionStart`/`TurnStart`/`ToolCall`/`ToolResult`/`TurnEnd`/`SessionShutdown`；`ProjectTrust`/`ResourcesDiscover`/`Input`/`BeforeAgentStart`/`AgentStart`/`BeforeProviderHeaders`/`BeforeProviderRequest`/`AfterProviderResponse`/`ToolExecutionStart`/`ToolExecutionUpdate`/`ToolExecutionEnd`/`AgentEnd`/`AgentSettled`/`SessionBeforeSwitch`/`SessionBeforeFork`/`SessionBeforeCompact`/`SessionCompact` 待 §F2b wire）。
- 完整 e2e round-trip（assert complete ordered event sequence）+ App 字段 live wiring + `/extension reload` re-discover（当前仅 `invalidate()`）。

**下一聚焦工作：**
- §F2b：wire 每个新事件到其 `HostAgentExecutor`/app seam + 完整 e2e round-trip test + App 字段 live wiring + `/extension reload` re-discover。
- 残项：P2 doc drift（推迟 slice 54）+ §E4 两 follow-up（按需）——均 on-demand / 非阻塞。

**进度（2026-07-22 §F2b host seam wiring——§F2 拆为两子切片的 b 半：honor `EmitOutcome`（Block/Cancel/Transform）at 7 `host_executor` seams + 发射 22/23 事件（`ToolExecutionUpdate` 延后 §F2c）+ 完整 e2e round-trip + App 字段 live wiring + `/extension reload` re-discover，`feat/pluggable-framework-core`）：**

接 §F2a（contract + runtime core 半）。本切片把 §F2a 的 outcome-链 contract wire 进 host：在每个 seam 检查 `out.outcome` 的 seam-capable 能力（Block=`ToolCall`、Cancel=`SessionBefore*`、Transform=`Input`/`BeforeAgentStart`/`BeforeProviderRequest`/`ToolResult`），out-of-place outcome → `Continue`；并补齐 22 个 not-yet-wired 事件。`ExtensionEvent`/`HandlerOutcome`/`EmitOutcome`/`on_variant`/`catch_unwind` 全部稳定不变（仅 `EmitOutcome` 加 `#[must_use]`，`ExtensionRunner` 加 runtime-lifecycle `clear_handlers`——非 contract 变更）。plan：`docs/superpowers/plans/2026-07-22-codesmith-extension-system-slice-2b.md`。

**关键设计决策：**
- **seam-capable outcome 映射固定**：Block=`ToolCall`（skip dispatch → permission-denied result）、Cancel=`SessionBefore*`（skip compaction/switch/fork）、Transform=`Input`/`BeforeAgentStart`/`BeforeAgentStart`/`BeforeProviderRequest`/`ToolResult`（rewrite actionable field）；out-of-place（如 Block at `TurnEnd`）→ `Continue`，由 host 的 `match _ => original` fallthrough 强制。
- **Transform at ToolResult reorder**：emit 移到 `on_tool_end` **之前**，提取 transformed result → `on_tool_end` + downstream `outcomes[idx].result` 都见 transformed（Phase-4 持久化见 transformed）。
- **22/23 wired，`ToolExecutionUpdate` 延后 §F2c**：`Callback` trait 无 `on_tool_progress` 流式钩子——需 §F2c 扩 trait（spec §F10.2 deferred）。EXTENSIONS.md 记 deferral。
- **`SessionStart`/`SessionShutdown` 补齐 §F1 gap**：§F1 声明但从未发射；§F2b 在 engine/mod.rs observe-only 发射，闭合 6-live-事件宣称与实际的缺口。
- **live reload = re-populate shared `Arc`**：App 持 `extension_runner`(Arc)+`extension_state`+`workspace` 但**不**持 `Engine`/`cancel_token`；engine build 时 clone `self.extension_runner`。换 App 的 Arc 无法更新 Engine 字段 → 必须 re-populate 共享 Arc（所有 holder 自动更新）。`bind_core` append 到 `handlers`(Vec) 不清空 → 加 `clear_handlers`。`reload_extension_runtime` = clear → invalidate → discover_static → reconcile → load → bind_core。
- **reload 用 fresh `CancellationToken`**：当前无 §F2b handler 读 ctx signal；共享 engine 的 token 属 §F2c 增强（避免 App field plumbing + respawn stale-token 复杂度）。
- **4 事件 tui-level 显式延后**：`ProjectTrust`（`build_tool_context_for` 同步、不能 `.await` emit）、`ResourcesDiscover`（MCP 独立进程不共享 App runner Arc）、`SessionBeforeFork`（dead-code `fork_at_user_message`、tui 无 `RuntimeThreadManager` ctor）—— EXTENSIONS.md 记 deferral；`SessionBeforeSwitch` test 因 TaskManager scaffolding 比例失衡，wire 已落地但测试 deferred。`Input` transform wire 到 `run_inner`（非 `submit_user_input`——后者携 `UserInputResponse` 结构而非 text）。

**落地步骤：**
1. `crates/extensions/src/runner.rs`：T1 `EmitOutcome` 加 `#[must_use]`（强制 seam 检查）；T7 `clear_handlers()`（runtime lifecycle，clear `handlers` Vec）。
2. `crates/agent-runtime/src/engine/host_executor.rs`：T1 7 seam honor `EmitOutcome`（observe-only `let _ =`；Block at `ToolCall` parallel+serial skip dispatch → permission-denied；Transform at `ToolResult` parallel+serial reorder emit→extract→`on_tool_end`→propagate）；T2 6 agent-lifecycle/provider 事件（`BeforeAgentStart`[transform inject_message+system_prompt]/`AgentStart`/`BeforeProviderHeaders`/`BeforeProviderRequest`[transform messages]/`AfterProviderResponse`/`AgentEnd`）；T3 4 tool-exec+compaction 事件（`ToolExecutionStart`/`End` bracket `tool.run`；`SessionBeforeCompact`[cancel] gate compaction；`SessionCompact` observe）；T4 完整 e2e round-trip（15-event ordered lifecycle across 2 calls）。
3. `crates/agent-runtime/src/engine/mod.rs`：T5 3 engine-level 事件（`AgentSettled` post-run drain observe；`SessionStart`[Startup] pre-op-loop observe；`SessionShutdown` post-MCP-shutdown observe）。
4. `crates/tui/src/tui/ui.rs`：T6 `SessionBeforeSwitch`[cancel] gate at `switch_workspace` entry；`Input`[transform] wire 到 `run_inner`（host_executor.rs，非 tui）。
5. `crates/tui/src/core/engine.rs`：T7 extract `populate_extension_runtime`（discover→reconcile→load→bind_core body）+ pub `reload_extension_runtime`（clear+invalidate+populate）；`build_extension_runtime` 调 `populate`。
6. `crates/tui/src/commands/extension_commands.rs`：T7 `reload(app)` rewire 调 `reload_extension_runtime`（re-discovers+re-loads+re-binds on shared Arc）。

**测试/验证：** `cargo +1.90.0 build --workspace` 全绿；`codesmith-extensions --lib` 14（`#[must_use]` 无行为变化）；`codesmith-agent --lib` 97（contract 不变）；`codesmith-agent-runtime --lib` 1152+2 → +11 `f2b_*`（Block at ToolCall skips dispatch / Transform at ToolResult rewrites on_tool_end / BeforeAgentStart transform injects / BeforeProviderRequest transform rewrites messages / ToolExecution Start/End bracket / SessionBeforeCompact cancel skips / SessionCompact fires / full lifecycle ordered 15 events / AgentSettled post-run / Session start-settled-shutdown）；`codesmith-tui --bin codesmith-tui` 2853+2 → 2855+2（+2 `f2b_*`：engine emits session start/settled/shutdown + extension reload clears+rebinds live）；grep `.emit(codesmith_agent::extension::ExtensionEvent` 跨 `host_executor.rs` >7-hit（22 wired）；`.emit(&...` → 0-hit。

**By-design gaps（§F2c，显式 out-of-scope）：**
- `ToolExecutionUpdate`（无 `Callback::on_tool_progress` 流式钩子——需 §F2c 扩 trait）。
- `ProjectTrust`/`ResourcesDiscover`/`SessionBeforeFork`（tui-level seam 不可达：sync context / 独立进程 / dead-code fork 路径）。
- `SessionBeforeSwitch` test（wire 已落地，TaskManager scaffolding 比例失衡）。
- reload 共享 engine `cancel_token`（当前 fresh token；无 §F2b handler 读 ctx signal）。

**下一聚焦工作：**
- §F2c（按需）：`ToolExecutionUpdate` 需 `Callback` trait 扩 `on_tool_progress` 流式钩子；reload 共享 engine cancel_token；3 个 tui-level deferred seam。
- 残项：P2 doc drift（推迟 slice 54）+ §E4 两 follow-up（按需）——均 on-demand / 非阻塞。

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

**Status (2026-07-07 §A1 + slice 41/42):** superseded — `DeepSeekClient` was
**retired, not extracted.** The "needs DeepSeek replay bridge" blocker was
found unnecessary: rig's OpenAI/DeepSeek compat layer natively serializes
`AssistantContent::Reasoning` as the `reasoning_content` wire field (verified
against rig-core 0.39.0, see the 2026-07-07 §A1 checkpoint). DeepSeek was
switched onto the rig `DeepSeekFactory` via `default_registry()` and the
tui-local `DeepSeekProviderFactory` + `DeepSeekClient` were deleted. Slice 41
then migrated the residual inspect/warmup cluster (`client.rs`/`chat.rs`) to
`codesmith-agent-runtime` `prompt_inspect` and deleted those tui files; slice
42 deduped the reasoning predicates (→ `codesmith-agent` core) and
`sha256_hex` (→ agent-runtime `utils`). The "extract into
`codesmith-providers`" path above is the abandoned original plan, kept as a
record of the approach considered.

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

**Status (slice 51):** partially superseded — the decoupling goal is
already satisfied, but via `&str`-keying rather than the `ProviderKind` switch
the original plan describes. `crates/providers` has no `codesmith-agent-runtime`
dep edge (see `crates/providers/Cargo.toml` — declares only `codesmith-agent`
+ `codesmith-config`); provider branching keys on `&str` provider names, not
`ApiProvider` (the `reasoning.rs:16-19` module doc documents this as the
deliberate §C6 decoupling path). The `chat.rs:80` `apply_provider_token_limit`
(XiaomiMimo) and `chat.rs:1915` `provider_accepts_reasoning_content` (9-variant
allowlist) references above are **stale post-slice-41** — slice 41 deleted
`crates/tui/src/client.rs` + `crates/tui/src/client/chat.rs`; both functions
migrated to `crates/providers/src/rig_adapter/{shaper.rs:219 (shape_max_tokens),
reasoning.rs:30 (provider_accepts_reasoning_content)}` and now key on `&str`.
`ProviderKind` already folds the alias (`crates/config/src/lib.rs:76-79` serde
aliases + `:138-139` `parse()` collapse the `deepseek-cn` family onto
`Deepseek`; `as_str()` returns `"deepseek"`). What remains of §B3 is purely the
`ApiProvider::DeepseekCN` variant itself
(`crates/agent-runtime/src/config_types.rs:202`) — folding it would align
`ApiProvider` with `ProviderKind`. Deferred as low-priority/mitigated: the
decoupling goal is done, the variant is cosmetic, and the fold has a narrow
read-path regression (hand-edited configs with `[providers.deepseek_cn]`
storage; the TUI save flow already routes to root `api_key`, so
onboarding-flow users are unaffected; a future structural slice could mitigate
via a read-side fallback). The "switch the client + factory to `ProviderKind`"
path above is the abandoned original plan, kept as a record of the approach
considered.

**Status (slice 52):** the cosmetic fold landed. `ApiProvider::DeepseekCN` was
deleted from `crates/agent-runtime/src/config_types.rs:202`; `Deepseek` gained
the serde aliases `deepseek-cn`/`deepseek_china`/`deepseekcn`/`deepseek-china`/`deepseek_cn`
(the last covers the old snake_case rename → `ProviderCapability` JSON
backward-compat), and `parse()` folds the CN family + adds `deepseek_cn` (closing
a latent gap where serde accepted `deepseek_cn` but `parse()` rejected it) —
mirroring `ProviderKind` (`crates/config/src/lib.rs:76-79` + `:138-139`).
`DEFAULT_DEEPSEEKCN_BASE_URL` (== `DEFAULT_DEEPSEEK_BASE_URL`) was deleted; its
2 arms repointed. 35 grouped `Deepseek | DeepseekCN` match arms across
`config_types.rs` + `tui/{config.rs, core/engine.rs, tui/*, main.rs,
commands/balance.rs}` were mechanically folded. The documented `api_key`-storage
read-path regression (a) is mitigated by a surgical read-side fallback:
`providers.deepseek_cn: ProviderConfig` is retained as a deprecated read-only
legacy sink (`crates/tui/src/config.rs:1370`) and `Config::legacy_deepseek_cn_api_key()`
(`:1762`) is consulted by `has_api_key_for` (`:4484`) +
`active_provider_has_config_api_key` (`:4327`) when `providers.deepseek.api_key`
misses for `Deepseek`; the `deepseek_cn` field is never written by any code path
post-fold (env override repointed to `providers.deepseek`, TUI save flow already
bailed for both variants). The `display_name` regression (b) — loss of the
"(legacy alias)" suffix, now just `"DeepSeek"` — is accepted. §B3 closed;
`&str`-keying (not the `ProviderKind` switch) was the §C6 decoupling path.
`codesmith-providers` `"deepseek-cn"` `&str` arms (`reasoning.rs:34/100/133/180`)
were retained as defensive input-accept paths.

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
- **Status (9d47942c + slice 46):** landed as a dedicated `custom_provider`
  selector + `[[providers.custom]]` table (the bare `provider = "<id>"` form is
  **by-design rejected** — it cascades the closed `ProviderKind` enum through
  `ConfigToml`/overrides/env + every `match` arm; see the 9d47942c commit
  message). Slice 46 closed the residual polish: `--custom-provider <id>` CLI
  flag (env-forwarded to the TUI as `CODESMITH_CUSTOM_PROVIDER`, mutually
  exclusive with `--provider`, builtin-id collision rejected at parse) +
  per-entry `config set/get/unset providers.custom.<id>.<field>` (find-or-create
  by id; `id` field rejected as it is the key). See the slice-46 progress entry
  below.

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

**Status (framework-core traits + slices 11–20 §E):** done. The `AgentExecutor`
trait + the host-agnostic `DefaultAgentExecutor` reference impl landed in
`codesmith-agent` (`crates/agent/src/executor/mod.rs`). The host-side
`HostAgentExecutor` (`crates/agent-runtime/src/engine/host_executor.rs`)
absorbed the production guardrails across slices 11–40 and became the live
production path in the slice 20 §E cutover — `Engine::handle_send_message`
constructs a `HostAgentExecutor` and calls `executor.run(...)`; the tangled
`handle_deepseek_turn` (~2.4k lines) is deleted. The "Today this lives tangled
in Engine" framing above is the pre-migration state, kept as a record.

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

**Status (slices 43–45 + slice 51):** landed. The declarative `providers.toml`
schema/loader shipped in `codesmith-config` (slice 43: `FactoryBackend` enum +
`ProviderDescriptor`/`ProvidersManifest` types + `load_providers_manifest_from`
+ the `CODESMITH_PROVIDERS_MANIFEST` env override); `default_registry()` was
wired to the bundled manifest via a process-wide `OnceLock` (slice 44); the
`base_url`/`model` columns were populated and the four rig-backed factories
consume them as a manifest-default fallback via `resolve_with_manifest_default`
(`crates/providers/src/lib.rs:206-217`, slice 45). Two follow-ups remain,
tracked only in ROADMAP (no in-source `TODO`/`FIXME` markers, no
`ARCHITECTURE.md` status-table mention):
- **env override augment** — the resolver chain
  (`ConfigToml::resolve_runtime_options_with_secrets` in
  `crates/config/src/lib.rs:1620-1787`) still falls back to the hardcoded
  `DEFAULT_*_BASE_URL`/`DEFAULT_*_MODEL` constants (`:1650-1672`/`:1992-2032`),
  NOT the manifest; the manifest default fires only inside the factory
  `build()` path for empty-host `ProviderConfig` values. The env path
  (`EnvRuntimeOverrides`, `:2640-2746`) and the manifest path
  (`resolve_with_manifest_default`) are two disjoint fallback chains — env does
  not augment the manifest, and the `DEFAULT_*` constants duplicate the
  manifest's `base_url`/`model` values. Dedup/augment is deferred as
  cross-layer-unreachable (`codesmith-config` cannot depend on
  `codesmith-providers`' bundled manifest — §C6 layering).
- **flash/kimi-code variant sinking** — no `Flash`/`KimiCode` variant exists
  in `FactoryBackend`/`ProviderKind`/`ApiProvider`, and no such entry exists in
  `providers.toml`. The variants live as host-side constants + selection logic
  in `crates/config/src/lib.rs`: `DEFAULT_*_FLASH_MODEL` constants (`:19`,
  `:31`, `:47`, `:50`, `:56`, `:64`) selected via `normalize_model_for_provider`
  flash-alias arms (`:1884-1932`); `DEFAULT_KIMI_CODE_*` (`:53-54`) selected
  via `auth_mode_uses_kimi_oauth` (`:2107-2116`) and
  `moonshot_base_url_uses_kimi_code` (`:2034-2039`) inside the `Moonshot` arms
  of the base_url/model resolvers (`:1662-1668`/`:1722-1730`). "Sinking" these
  into the manifest would require new manifest fields/entries or a
  host-resolver refactor — deferred as a host concern (the manifest carries
  only the primary URL+model).

## §F — Extension system (pi-mono parity)

The provider seam (§A) + framework-core traits (§E) are the foundation. §F
builds the **extension system** on top: a unified `Extension` concept with
imperative registration, lifecycle events, extension-to-extension bus,
runtime provider registration, unified discovery/manifest, stale-context
guard — ported from pi-mono's extension model. Mirrors the §E three-layer
pattern (traits in `codesmith-agent`, runtime in `codesmith-extensions`,
adapters in `codesmith-agent-runtime`, host wiring in `codesmith-tui`).
Delivered in slices; hot-load is permanently out (install + reload only).

### F1 — Slice 1 (foundational core, phase 1 static)

- Core traits in `codesmith-agent::extension` (`Extension` /
  `ExtensionApi` / `ExtensionContext` / `ExtensionCommandContext` /
  `ExtensionEvent` / `Handler` / `ToolDefinition` / `CommandDefinition`)
  with the minimal 6-event set (`#[non_exhaustive]`).
- New crate `codesmith-extensions`: `ExtensionRunner` (event dispatch +
  stale-context guard) + `ExtensionApi` stub→real + `inventory`-based static
  discovery + `EventBus` skeleton + install-source traits (impls §F5).
- `codesmith-agent-runtime`: `ExtensionToolSpecAdapter` (mirrors
  `ToolSpecAdapter`) + `HostAgentExecutor` seam wiring (TurnStart/ToolCall/
  ToolResult/TurnEnd emits).
- `codesmith-tui`: `build_extension_runtime()` + `ExtensionStateStore`
  (mirrors `SkillStateStore`) + `/extension` command group (list/info/enable/
  disable/status/reload working; install/uninstall stub "phase 2").
- In-tree sample `scratchpad` extension (tool + command + handler).
- `docs/EXTENSIONS.md` developer guide + sandbox stance.

**Status (slice 1 §F1):** done. Minimal contract + runtime + adapters + host
wiring + sample + docs landed. Deferred to §F2–§F8: full ~30-event lifecycle,
cancel/transform/block chains, `EventBus` impl, `registerProvider`,
`registerShortcut`/`registerFlag`/renderers, dylib loading (phase 2),
install-source impls, embed API. Hot-load permanently out.

### F2a — Slice 2a (contract + runtime core)

- Full 23-variant `ExtensionEvent` set (§F1's 6 + 17 new) + `ExtensionEventKind`
  discriminant + exhaustive `kind()` guard, all in `codesmith-agent::extension`.
- `HandlerOutcome` (`Continue`/`Cancel`/`Block`/`Transform`) drives the
  cross-handler chain; `Handler::handle` returns `Result<HandlerOutcome, _>`.
- Per-variant subscription via `ExtensionApi::on_variant(kind, handler)` +
  `RegisteredHandler { handler, kind_filter }`; `on` = subscribe-to-all (`None`).
- `ExtensionRunner::emit` rewrite: owned-in / `EmitOutcome`-out, chained
  (registration-order; `Transform` visible to next handler; `Cancel`/`Block`
  short-circuit), per-variant filter, `catch_unwind` isolation per §8.3.
- Mechanical 7-site `host_executor` emit-signature update (drops `&`; §F2a
  discards `EmitOutcome`).

**Status (slice 2a §F2a):** done. Full event set + `HandlerOutcome` chain +
per-variant dispatch + `catch_unwind` landed. Deferred to §F2b: host seam
wiring (honor `Cancel`/`Block`/`Transform` at the 7 seams), emit the ~17 new
events from `host_executor`, full e2e round-trip, App field live wiring,
`/extension reload` re-discover. Remaining §F3–§F8 unchanged.

### F2b — Slice 2b (host seam wiring)

- `#[must_use]` on `EmitOutcome` (forces seam sites to inspect the outcome).
- Honor `EmitOutcome` at the 7 `host_executor` seams: observe-only
  (`let _ =`) for `TurnStart`/`TurnEnd`; `Block` at `ToolCall` (parallel +
  serial) skips dispatch → permission-denied result; `Transform` at
  `ToolResult` (parallel + serial) reorders emit→extract→`on_tool_end`→
  propagate to `outcomes[idx].result`; out-of-place outcomes → `Continue`.
- Wire 22/23 events from host: T2 `BeforeAgentStart`[transform inject+
  system_prompt]/`AgentStart`/`BeforeProviderHeaders`/`BeforeProviderRequest`
  [transform messages]/`AfterProviderResponse`/`AgentEnd`; T3 `ToolExecution
  Start`/`End` bracket `tool.run` + `SessionBeforeCompact`[cancel] gate +
  `SessionCompact` observe; T5 `AgentSettled`/`SessionStart`/`SessionShutdown`
  (engine/mod.rs); T6 `Input`[transform] + `SessionBeforeSwitch`[cancel].
- Full e2e round-trip test asserting the complete ordered host lifecycle
  (15 events across 2 calls).
- Live reload: `ExtensionRunner::clear_handlers` + extracted
  `reload_extension_runtime` (clear→invalidate→discover→reconcile→load→
  bind_core) so `/extension reload` re-populates the **shared runner `Arc`**
  (App.extension_runner + Engine field both live, no Arc swap).

**Status (slice 2b §F2b):** done. 22/23 events wired + `EmitOutcome` honored
at 7 seams + full e2e + live reload. Deferred to §F2c: `ToolExecutionUpdate`
(needs `Callback::on_tool_progress` stream hook), reload sharing the engine's
`cancel_token`, and 3 tui-level seams (`ProjectTrust`/`ResourcesDiscover`/
`SessionBeforeFork`) that are unreachable from the App runner (`sync context`/
`separate MCP process`/`dead-code fork path`). Remaining §F3–§F8 unchanged.
