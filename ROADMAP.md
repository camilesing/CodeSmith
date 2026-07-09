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
