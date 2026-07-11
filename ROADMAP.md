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
