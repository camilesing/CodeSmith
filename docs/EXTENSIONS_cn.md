# 扩展

CodeSmith 扩展是编译内置或从安装根以 dylib 加载的模块，它们向 agent 循环贡献**工具**、**斜杠命令**与**生命周期事件处理器**，并通过同一注册面贡献 **provider**、**提示词分段**、**技能**、**消息投影**与**工具守卫**。扩展是一个工厂（`impl Extension`），在 `configure` 期间向 `ExtensionApi` 注册自己的贡献项。

> **脚本 Mod。** 同一 `Extension` 契约也由 Rhai 脚本 **Mod** 实现（`~/.codesmith/mods/<id>/mod.toml` + `mod.rhai`）—— 无需编译 Rust 即可获得钩子、工具、斜杠命令与持久 KV，首次激活门控并支持热重载。它们运行在本文档描述的同一套 runner / 接缝 / 重载机制之上。作者指南：[MODS_cn.md](MODS_cn.md)。

宿主在引擎构建时（`build_extension_runtime()`，位于
`crates/tui/src/core/engine.rs`）组装扩展运行时，并在每次
`/extension reload` 时重新组装：发现编译内置注册（`inventory`）+
dylib 源（安装根）→ 与磁盘上的 `ExtensionStateStore` 对账（跳过
已禁用的）→ 针对一个 stub api 逐个加载并配置 → `bind_core` 宿主
上下文 —— 之后 runner 将生命周期事件分发给已注册的处理器。

扩展工具 + 斜杠命令按轮实时接入宿主：工具通过
`EngineHost::build_turn_dispatcher` 中的 `register_extension_tools`
注册到每轮的 `ToolRegistry`，斜杠命令通过 `commands::execute` 中的
`try_dispatch_extension_command` 分发 —— 因此 agent 循环将扩展
工具视为普通的 `ToolSpec`（仅限主轮；不会被子代理继承 —— 见
[沙箱立场](#沙箱立场)）。

每次 populate/reload 还会为每个发现的扩展/mod 收集一条结构化审计条目 —— `Loaded` / `Failed { 原始错误 }` / `PendingConsent` / `Disabled` / `TrustGated` —— 以启动被动通知、reload 消息与 `tracing` 汇总（有失败时）三个面呈现。mod.toml 校验为 schema 聚合式（错误带字段路径）。

## 注册面

扩展可注册的一切，以及宿主对它们的处理：

- **工具 + 斜杠命令** —— `register_tool(Box<dyn ToolDefinition>)` /
  `register_command(Box<dyn CommandDefinition>)`；按上文所述按轮实时接线（仅限主轮）。
- **Provider（Rust 形态：`register_provider`）** —— 完整的
  `Arc<dyn ProviderFactory>`，汇入宿主解析客户端所用的
  `codesmith_agent::provider::SharedProviderRegistry`。注册会写日志
  （target `codesmith_extensions`），并在下一次客户端解析时生效 ——
  已构建的客户端从不被热替换。注销经 `ProviderRegistration` drop guard
  对称完成；reload 丢弃该 generation 的全部 guard。
- **Provider 别名（脚本形态：`register_provider_alias`）** —— 声明式
  别名到内置 provider（新自定义 id + 可选 `base_url` /
  `default_model` / `http_headers` 覆盖）；即 Rhai 的
  `register_provider(spec)` 原生函数。Rhai mod 无法实现 `LlmClient`
  （设计上无 async/网络），因此这是它们唯一的 provider 贡献形式。
- **提示词分段（`register_prompt_section`）** —— 追加到基础系统提示词的
  命名、会话内稳定分段（≤16 段，加载时校验）。宿主在每轮
  `BeforeAgentStart` 接缝**之前**追加它们，因此 handler 的整段提示词
  替换仍然优先。reload 清除该 generation 的分段（前缀缓存纪律）。
- **消息投影（`register_message_projection`）** —— 宿主在会话转录上
  维护的折叠（Rhai 形态：`register_message_projection(key, init, fold)`
  注册 + `projection_state(key)` 读取）。追加式增量折叠；整体替换
  （会话重载重建、压缩、`/edit` 回滚）从头重折。状态从不落快照 ——
  它由日志重建。
- **技能（`register_skill`）** —— 会话技能目录中的一个内存技能
  （Rhai 形态：`register_skill(spec)`，字段 `name` / `description` /
  `body` / 可选 `when_to_use`）：系统提示词 `## Skills` 块、`/skills`、
  命令面板与按名的 `load_skill` 均可见，来源归因 `mod: <owner>`。
  名字与文件系统目录冲突时文件系统获胜；名字已被其他 mod 持有则加载
  失败；≤16 个；reload 清除该 generation 的注册。注册技能不带
  `paths`（不参与条件匹配），子代理不渲染技能目录。
- **工具守卫（`GuardHandler`；Rhai 形态 `register_guard(callback)`）** ——
  包在 `ToolCall` 接缝上的 deny-only 闭包：返回字符串即拒绝该调用
  （归因 `guard (mod: <id>)`），返回其他任何值则弃权。没有 allow 或
  transform 词汇，拒绝由构造保证单调（block 短路整条链）。守卫在接缝
  处、审批之前评估，仅覆盖主回合调用；脚本出错则弃权并打 warn。
- **事件处理器（`on` / `on_variant`）** —— 见
  [处理器](#处理器结果--按变体订阅)。

### 工具目录：`ToolsChange` + 能力清单

回合分发器将编译后的模型可见目录与上一主回合基线做差量（来源分类
快照见 `tui/src/core/tool_catalog.rs`），发出仅观察的
`ToolsChange { added, removed }`；`/tools` 按来源分组展示当前基线。
首次构建静默建立基线；子代理工具集走独立路径（`spawn_subagent`），
不会让基线来回翻转。

会话级选择存于能力清单（`~/.codesmith/capabilities.toml`，
`[tools] disabled = [...]`；环境变量覆盖
`CODESMITH_CAPABILITIES_MANIFEST`）。被禁用的工具在组合点
（`EngineConfig.disabled_tools`）移除 —— 在主轮与任何子代理中都既
不可见也不可执行，与 spawn 深度无关。轮内掩码（preset 的
`tools.include`/`exclude`、斜杠命令 frontmatter、每轮的
`allowed_tools`/`blocked_tools`）叠加其上，无法复活被禁用的工具。
该文件每进程读取一次；畸形的清单会让引擎构建大声失败。旧
config.toml 的 `[tools].overrides <name> = { type = "disabled" }` 形态
仍可解析（外部配置契约）并且生效 —— 以弃用警告并入有效集合。

## 引导（Bootstrap）

编译内置的扩展通过 [`inventory::submit!`](https://docs.rs/inventory)
注册。在 `crates/extensions/src/lib.rs` 中声明 `pub mod <name>;` 就是
发现所需的全部 —— 无需运行时注册调用。宿主的
`build_extension_runtime()` 在引擎构建时调用一次
`codesmith_extensions::discover_static()`。

## TUI 内管理器

`/extension` 命令组是面向用户的界面。它通过
`extension_commands::try_dispatch` 分发，接入到 `execute()` 中用户
自定义命令与静态 `match` 之间。

| 子命令 | 别名 | 效果 |
|---|---|---|
| `/extension list` | `ls` | 列出编译内置 + 已安装的扩展（id + 版本）。 |
| `/extension info <id>` | | 显示单个扩展的元数据。 |
| `/extension enable <id>` | | 在 `extensions_state.toml` 中将扩展标记为启用；在下一次 `/extension reload` 时生效。 |
| `/extension disable <id>` | | 将扩展标记为禁用；同样的重载注意事项。 |
| `/extension status` | | 报告已绑定 runner 的 generation + 已绑定的命令/工具计数。 |
| `/extension reload` | | 重新填充**共享的 runner `Arc`**：`clear_handlers` → `clear_tools` → `clear_commands` → `clear_providers`（丢弃注册 guard）→ `clear_prompt_sections` → `clear_skills` → `clear_message_projections` → `drain_libraries_to_pending` → `invalidate`（递增 generation）→ 发现（静态 + dylib）→ 与状态对账 → 逐个 `load` → `bind_core`（全新的 `HostExtensionContext`）。`App.extension_runner` 和 Engine 的字段都会实时更新（没有 `Arc` 交换 —— 它们共享引擎构建的那一个）。被排空的 `Library` 会在引擎 op-loop 的下一次顶部（轮边界）被 `drop_pending`。重载前绑定的处理器之后不再观察（被清除，而不是重复）；新安装的扩展会在下一次重载时被拾取。 |
| `/extension install <source> [--global]` | | 拉取（`git:`/`path:`/`crate:`/`prebuilt:`）→ 构建（`cargo build`）→ 放置到 `<root>/<id>/` + 写入 `extension.toml` + 记录 `installed[]` 来源 + 写入 sha256 边车（`<dylib>.sha256`，加载器会校验 —— 安装后被替换过的 dylib 在加载时会被拒绝）；`--global` 为可选（默认为项目级）。`crate:` 从 crates.io 拉取（sparse-index → 版本 → sha256 校验的 `.crate` → `tar` 解压 → 构建）；`prebuilt:<https-url>` 拉取预构建 cdylib（仅限 HTTPS，重定向不可降级到明文 HTTP，可选 `--checksum <sha256>`）；两者在项目级且未受信任时都会警告；用 `/extension reload` 加载。 |
| `/extension uninstall <id>` | | 移除 `<root>/<id>/` + 清除 `installed[]` 来源记录。活跃的工具/命令绑定在下一次 `/extension reload` 时清除；dylib 在下一个轮边界安全卸载（两阶段 `Library` drop）。 |

## 发现

- **静态（编译内置）：** 扩展通过 `inventory::submit!` 注册一个
  `ExtensionRegistration { factory, metadata }`；`discover_static()`
  收集链接进二进制的每一个注册。树内 `scratchpad` 示例是参考注册。
- **Dylib（安装根）：** `discover_dylib(&global_roots, &project_roots)`
  遍历安装根（全局 `~/.codesmith/extensions`，项目本地
  `.codesmith/extensions`）。每个根可以是扩展子目录的容器、单个清单
  目录（含 `extension.toml`）、或裸 `.dylib`/`.so`/`.dll` 文件；来源按
  规范化 dylib 路径去重。加载经 `libloading` 与 lockstep
  `*mut dyn Extension` 交接（`codesmith_register_extension`），加载器
  校验安装时写入的 sha256 边车。项目本地信任门（`apply_trust_gate`）
  在工作区未受信任时丢弃项目根的 dylib；`ProjectTrust { FirstLoad }`
  事件在 onboarding 接受时翻转该信任。

## 最小示例

树内 `scratchpad` 扩展（`crates/extensions/src/sample_scratchpad.rs`）
贡献了全部三个基础贡献点 —— 一个工具、一个斜杠命令与一个事件
处理器。原文摘录：

```rust
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use codesmith_agent::extension::*;
use codesmith_tools::{ToolCapability, ToolResult};
use serde_json::{json, Value};
use crate::discovery::ExtensionRegistration;
use crate::ExtensionMetadata;

static SCRATCH: Mutex<Option<String>> = Mutex::new(None);

pub struct ScratchpadExtension;

#[async_trait]
impl Extension for ScratchpadExtension {
    fn metadata(&self) -> &ExtensionMetadata {
        static M: ExtensionMetadata = ExtensionMetadata::new("scratchpad");
        &M
    }
    async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
        api.register_tool(Box::new(ScratchTool))?;
        api.register_command(Box::new(ScratchCommand))?;
        api.on(Arc::new(TurnStartLogger))?;
        Ok(())
    }
}

// ScratchTool: impl ToolDefinition (name/description/input_schema/execute)
// ScratchCommand: impl CommandDefinition (name/description/run)
// TurnStartLogger: impl Handler (handle)

inventory::submit! {
    ExtensionRegistration {
        factory: || Box::new(ScratchpadExtension),
        metadata: ExtensionMetadata::new("scratchpad"),
    }
}
```

`/extension list` 会报告 `scratchpad`；`/extension info scratchpad`
显示其元数据。完整的工具/命令/处理器主体见该文件。

## 扩展字段（trait 契约）

所有契约位于 `crates/agent/src/extension.rs`。扩展作者依赖
`codesmith-extensions`（它 re-export `codesmith_agent::extension::*`），
这样一个 crate 就同时提供了 trait 和运行时辅助。

- **`Extension`** —— 工厂：`metadata() -> &ExtensionMetadata` +
  `async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError>`。
- **`ExtensionApi`** —— 注册面（两阶段：加载时为 stub，`bind_core`
  时为真实实现）：`register_tool(Box<dyn ToolDefinition>)` /
  `register_command(Box<dyn CommandDefinition>)` /
  `on(Arc<dyn Handler>)`（订阅所有事件）/
  `on_variant(ExtensionEventKind, Arc<dyn Handler>)`（仅订阅一个变体
  —— runner 在分发前按 `event.kind()` 过滤按变体处理器）+ 用于过期
  上下文防护的 `generation() -> u64`，以及上文的路线 A/B 注册项。
- **`ExtensionContext`** —— 交给处理器的以读为主的宿主状态：
  `cwd() / mode() / is_idle() / signal() / generation()` 为真实实现；
  `abort() / shutdown() / compact() / get_context_usage()` 返回
  `ExtensionError::Unimplemented`（见[已知限制](#已知限制)）。
- **`ExtensionCommandContext: ExtensionContext`** —— 交给命令处理器
  的严格子 trait；它不携带任何会话变更方法（这一拆分是为了类型
  安全）。
- **`ExtensionEvent`** —— `#[non_exhaustive]`，25 个变体：
  `SessionStart` / `TurnStart` / `ToolCall` / `ToolResult` / `TurnEnd` /
  `SessionShutdown` / `ProjectTrust` / `ResourcesDiscover` / `Input` /
  `BeforeAgentStart` / `AgentStart` / `BeforeProviderHeaders` /
  `BeforeProviderRequest` / `AfterProviderResponse` /
  `AssistantStream` / `ToolExecutionStart` / `ToolExecutionUpdate` /
  `ToolExecutionEnd` / `AgentEnd` / `AgentSettled` /
  `SessionBeforeSwitch` / `SessionBeforeFork` / `SessionBeforeCompact` /
  `SessionCompact` / `ToolsChange`。`ExtensionEvent::kind()` 将每个
  变体映射到一个 `ExtensionEventKind` 判别值，用于按变体分发。
- **`Handler`** —— 返回结果：
  `async fn handle(&self, event: &ExtensionEvent, ctx: &dyn ExtensionContext)
  -> Result<HandlerOutcome, ExtensionError>`。返回 `Continue`（无变化；
  继续）、`Cancel { reason }`（中止周围的操作 —— 仅对 `SessionBefore*`
  变体有意义）、`Block { reason }`（阻止操作 —— 仅对 `ToolCall` 有
  意义）或 `Transform(ExtensionEvent)`（替换正在运行的事件供后续
  处理器使用，并在具备变换能力的接缝处应用其可执行字段 ——
  `Input`/`BeforeAgentStart`/`BeforeProviderRequest`/`ToolCall`/
  `ToolResult`）。变体特定语义由宿主在每个接缝处强制执行；不合时宜
  的结果（例如在 `TurnEnd` 处 `Block`）会被忽略（按 `Continue`
  处理）。`emit` 按注册顺序链接处理器，因此 `Transform` 对下一个
  处理器可见；`Cancel`/`Block` 短路。
- **`ToolDefinition`** —— 扩展侧工具契约：`name / description /
  input_schema / capabilities / async execute(input, ctx)`。`execute`
  接收一个 `ExtensionContext`（而不是宿主的 `ToolContext`）—— 使
  扩展与 `ToolContext` 的约 30 个宿主耦合字段解耦。
- **`CommandDefinition`** —— 扩展侧斜杠命令契约：
  `name / description / async run(ctx, args) -> CommandOutput`。由宿主
  的 `extension_commands::try_dispatch` 分发。
- **`ExtensionError`** —— `StaleContext`（防护信号）+ `Config` /
  `Tool` / `Command` / `Conflict` / `Install` / `Load` / `Unimplemented`。

## 处理器：结果 + 按变体订阅

`Handler::handle` 返回 `HandlerOutcome`，`ExtensionRunner::emit` 按
注册顺序链接处理器 —— `Transform` 对下一个处理器可见，
`Cancel`/`Block` 短路。每个处理器调用都通过 `catch_unwind` 隔离：
panic 的处理器会通过 `tracing` 记录日志并被跳过 —— 它不会让 agent
循环崩溃 —— 处理器返回 `Err` 同样会被记录 + 链继续（尽力而为）。

用 `on` 订阅**所有**事件，或用 `on_variant` 订阅**某一个**变体
（runner 在分发前按 `event.kind()` 过滤按变体处理器，因此按变体
处理器永远不会看到不匹配的事件）：

```rust
use codesmith_agent::extension::*;
use async_trait::async_trait;

struct AbortCompaction;
#[async_trait]
impl Handler for AbortCompaction {
    async fn handle(
        &self,
        event: &ExtensionEvent,
        _ctx: &dyn ExtensionContext,
    ) -> Result<HandlerOutcome, ExtensionError> {
        // Fires ONLY for SessionBeforeCompact (per-variant subscription).
        match event {
            ExtensionEvent::SessionBeforeCompact =>
                Ok(HandlerOutcome::Cancel { reason: "user aborted".into() }),
            _ => Ok(HandlerOutcome::Continue),
        }
    }
}

async fn configure(api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
    api.on_variant(ExtensionEventKind::SessionBeforeCompact, Arc::new(AbortCompaction))?;
    Ok(())
}
```

## 分发契约 + 宿主接缝映射

每个 `ExtensionEventKind` 声明自己的分发模式（`dispatch_mode()`）：
**observe**（结果仅具参考性，被忽略）、**transform-chain**
（`Input`/`BeforeAgentStart`/`BeforeProviderRequest`/`ToolResult`）、
**cancel-veto**（`SessionBefore*` 接缝）或 **transform-and-deny**
（`ToolCall`：handler 可改写调用 `input` —— 改写后的输入才是审批、
执行与入档的输入 —— 或直接拒绝；拒绝是单调的）。该匹配与 `kind()`
同为穷尽匹配，并由 `event_dispatch_contract_table` 测试锁定，声明的
契约不会无声漂移。能力图谱（`docs/CAPABILITY_GRAPH.md`，由
`scripts/capability-graph.py` 生成、CI 校验）列出每个接缝的
定义/提供者/消费者三件套。

`EmitOutcome` 是 `#[must_use]`，因此每个 emit 点都会绑定结果
（仅观察接缝使用 `let _ =`；能力接缝检查 `out.outcome` /
`out.event`）。不合时宜的结果（例如在 `TurnEnd` 处 `Block`）会被
忽略 —— 按 `Continue` 处理 —— 因此为其变体返回错误能力的处理器是
无操作（no-op），而不是错误。

| 变体 | Emit 位置 | 遵守的结果 | 效果 |
|---|---|---|---|
| `SessionStart { reason }` | `engine/mod.rs` op-loop 之前 | observe | — |
| `SessionShutdown` | `engine/mod.rs` MCP 关闭之后 | observe | — |
| `TurnStart` | `host_executor` 轮入口 | observe | — |
| `TurnEnd` | `host_executor` 轮退出（被中断 + 无工具调用） | observe | — |
| `Input(InputEvent)` | `host_executor::run_inner`（用户轮种子） | **Transform** | 重写已提交的 `text` |
| `BeforeAgentStart(AgentStartEvent)` | `host_executor::run_inner` 顶部 | **Transform** | 注入 `inject_message`（history push）+ 若设置了 `system_prompt` 则覆盖 |
| `AgentStart` | `host_executor::run_inner`（观察） | observe | — |
| `BeforeProviderHeaders` | `host_executor` 在构建 `request` 之前 | observe | — |
| `BeforeProviderRequest(BeforeProviderRequestEvent)` | `host_executor` 在 `request` 构建后、流式开始前 | **Transform** | 重写 `request.messages` |
| `AfterProviderResponse(AfterProviderResponseEvent)` | `host_executor` `Content` 分支在 `accumulate_usage` 之后 | observe | — |
| `AssistantStream(AssistantStreamEvent)` | 回调桥流式增量路径（`agent-runtime/src/callback_bridge.rs`） | observe | 每个线上增量一个 assistant **文本**块；分发在离线程执行，受每增量 250 ms + 每轮累计 1 s 预算约束 —— 超预算的处理器被脱离（运行至完成、结果被丢弃、从不在执行中途被取消），该轮后续增量跳过分发；无订阅者时完全不分发；thinking 增量保持仅 UI |
| `ToolCall(ToolCallEvent)` | `host_executor` 并行 + 串行工具分发 | **Transform + Block** | handler 可改写调用 `input` 或拒绝它。`Block` 跳过审批 + `tool.run` → `Err(ToolError::permission_denied(reason))`，`blocked = true`。在并行批中，改写后的输入必须重新分类为自动批准，否则调用在该处被阻断 —— 改写无法把未批准的输入偷运过免审批批；模型可重新发起调用以走串行审批门。`on_tool_start` 与审计记录看到的是改写后的输入 |
| `ToolResult(ToolResultEvent)` | `host_executor` 并行 + 串行，emit 重排到 `on_tool_end` 之前 | **Transform** | 替换结果；`on_tool_end` + 下游 `outcomes[idx].result` 看到的是变换后的结果。`ToolResult` 携带 canonical/rendered 分离：`content` 是模型可见渲染，`canonical` 是结构化机器值（仅存活于当前进程，不随转录持久化）—— 改写 `content` 不得改写 `canonical` |
| `ToolExecutionStart` | `host_executor` 工具闭包（`tool.run` 之前） | observe | — |
| `ToolExecutionEnd` | `host_executor` 工具闭包（`tool.run` 之后） | observe | — |
| `AgentEnd` | `host_executor::run_inner` 每个 `return Ok(...)` | observe | — |
| `AgentSettled` | `engine/mod.rs` 运行后排空（容量应用之后） | observe | — |
| `SessionBeforeCompact` | `host_executor::run_compaction` 在 `should_compact` 门控之后 | **Cancel** | 跳过压缩（`return`） |
| `SessionCompact` | `host_executor::run_compaction` 在摘要应用之后 | observe | — |
| `SessionBeforeSwitch` | `tui/ui.rs` `switch_workspace` 入口 | **Cancel** | 中止工作区切换 |
| `ProjectTrust` | `HostServices::build_turn_dispatcher`（+ `spawn_subagent`）在 `build_tool_context_for` 之后（每轮 `Trusted`/`Untrusted`）；onboarding 信任接受时 `tui/ui.rs` `TrustDirectory` y/Y/1 分支在 `app.trust_mode = true` 之后（`FirstLoad`） | observe | 来自 `session.trust_mode` 的每轮 `Trusted`/`Untrusted`；`FirstLoad` 在每次 onboarding 信任接受时触发一次（`TrustReason::FirstLoad`）—— 不同于运行时 `trust_mode` 开关（`/trust on`）、YOLO 进入和持久化信任启动，后者在每轮表现为 `Trusted`/`Untrusted`，而不是 `FirstLoad` |
| `ToolsChange { added, removed }` | `HostServices::build_turn_dispatcher` 在所有选择源应用之后 | observe | 最终模型可见目录与上一主回合基线的差量；首次构建静默建立基线；子代理工具集不经过此接缝 |
| `—`（dylib LOAD，不是事件） | `populate_extension_runtime`（`tui/src/core/engine.rs`）在 `discover_static` 之后 | n/a（加载阶段） | `discover_dylib(&global_roots, &project_roots)` → `apply_trust_gate(discovered, !is_workspace_trusted(workspace))` 丢弃项目本地（`global == false`）→ `state.is_enabled` 对账 → 在 OS 线程加载运行时上执行 `ExtensionRunner::load_dylib`；重载经 `reload_extension_runtime`→`populate` 拾取。`ExtensionRunner.libraries` 持有 `Library` 句柄；在 `/extension reload` 时它们 `drain_libraries_to_pending` 到 `pending_drop`（与各 clear 一并执行）+ 引擎 op-loop 在下一个轮边界对它们执行 `drop_pending`。经 `codesmith_register_extension` 实现 lockstep `*mut dyn Extension`。 |
| `ResourcesDiscover` | —（无 emit 位点） | observe | 已定义但从不发出：唯一的进程内候选位点（`McpPool` 中 `list_mcp_resources` 伪工具的分发，`agent-runtime/src/mcp.rs`）已被 `ToolCall`/`ToolResult` 包夹 —— 在那里触发会与工具执行混淆，且 `DiscoverReason` 在那里没有干净的映射；没有持有 runner `Arc` 的专用 Startup/Manual/Reload 发现接缝（`tui/mcp_server.rs` 的 stdio 位点是独立进程） |
| `SessionBeforeFork` | —（无 emit 位点） | **Cancel** | 已定义但从不发出：TUI 内的回退路径（`apply_backtrack`，`tui/ui.rs`）是就地**回退（rewind）**（`truncate_history_to`/`api_messages.truncate`），而不是**分叉（fork）**（创建新线程）—— 接为 `SessionBeforeFork` 属于误标；真正的 fork 原语已死（`fork_at_user_message`，无非测试调用方）或仅限 HTTP（`fork_thread`，runtime-api，无 runner 访问权） |
| `ToolExecutionUpdate` | —（无 emit 位点） | observe | 已定义但从不发出：`Tool::run` 是一次性的（`agent/src/tools/mod.rs`），因此没有可在执行中途挂钩的进度流。`on_tool_progress` `Callback` 钩子作为前瞻性 API 面存在；emit 位点等待流式 `Tool` 契约 |

> `Transform` 载荷的可执行字段在完整处理器链运行之后才于接缝处应用
> （因此来自处理器 N 的 `Transform` 会作为正在运行的事件对处理器
> N+1 可见）。`Cancel`/`Block` 短路该链。最终的
> `EmitOutcome.outcome` 永远不会是 `Transform`（已折叠进
> `EmitOutcome.event`）；能力接缝检查 `out.outcome` 中的
> `Cancel`/`Block`，检查 `out.event` 中的变换后可执行字段。

## 沙箱立场

CodeSmith **不**对扩展做沙箱隔离。扩展与 agent 循环运行在同一进程中，
拥有完整的宿主访问权 —— **信任其来源**。对于不受信任的扩展，请将
整个 CodeSmith 进程容器化。项目本地 dylib 安装在首次加载前要求工作区
受信任（见上文信任门）；`ProjectTrust { FirstLoad }` 事件是用户接受
工作区信任提示时扩展处理器看到的每会话一次的仅观察信号。

`/extension install` 期间的 `cargo build` 会运行源码的 `build.rs` ——
**任意代码执行，已被接受（信任来源）**；对不受信任的来源请容器化。
安装本身与信任无关（它只*读取*信任以发出警告：项目本地安装在工作区
受信任之前不会加载）。已加载的 dylib 在进程内运行并拥有完整的宿主
访问权 —— 信任其来源；对不受信任的来源请容器化。编译内置的扩展在
构造上就是可信的（它们随二进制发布）。

扩展工具**仅限主轮，且是结构性的**：它们注册到宿主的每轮
`ToolRegistry`（主 agent 轮），不会被子代理继承。这是结构性的，
而非守卫：`SubAgentRuntime` 没有 `extension_runner` 字段 +
`SubAgentToolRegistry::new` 会重建自己全新的内置 `ToolRegistry` ——
因此无论 `inherit_full_registry` 如何，扩展工具都永远无法进入子代理
的有效集合。不需要来源标记 / 强制子集 / 运行时子代理检查。

安全卸载依赖两阶段 `Library` drop：UI 线程上的重载将孤儿 `Library`
移动到 `pending_drop`（`drain_libraries_to_pending`）；引擎 op-loop
顶部在主线程 `HostAgentExecutor`（唯一持有在飞 dylib `Arc` 的角色）
已在轮间被 drop 的那一刻将其 DROP（`drop_pending`）。这使得
`/extension reload` + 卸载可以与在飞的轮安全并发。其安全性由该
不变量 + 单调用点纪律证明；dylib+Miri 不可靠（libloading 的
`Library::drop` 会运行 `dlclose`/`FreeLibrary`，而 Miri 不对其建模），
因此证明来自不变量 —— 而不是一次 Miri 运行。

## 已知限制

- `ExtensionContext::abort() / shutdown() / compact() /
  get_context_usage()` 返回 `ExtensionError::Unimplemented`。
- `ResourcesDiscover`、`SessionBeforeFork` 与 `ToolExecutionUpdate`
  在契约中有定义，但没有宿主 emit 位点（理由见上文接缝表）。
- `EventBus`（`codesmith_extensions::EventBus`）是骨架 ——
  `subscribe`/`publish` 返回 `Unimplemented`；没有扩展到扩展的
  发布/订阅。
- `ExtensionApi` 没有渲染器、快捷方式或标志注册面。
- `AssistantStream` 只携带文本增量 —— thinking 增量保持仅 UI。
- 能力清单每进程读取一次；修改需要重启。
- 热加载永久排除 —— 仅限安装 + `/extension reload`。

## 故障排查

- **`/extension list` 什么都不显示。** 没有 `inventory::submit!` 到达
  链接 —— 确认扩展的 crate 是 workspace 成员，且
  `crates/extensions/src/lib.rs` 声明了其模块。`cargo test -p
  codesmith-extensions scratchpad_is_discoverable` 可证明注册已接线。
- **`/extension status` 显示 "not bound"。** 引擎尚未构建（启动前），
  或 `app.extension_runner` 没有从句柄复制（`crates/tui/src/tui/ui.rs`
  中 `spawn_engine` 之后）。
- **处理器返回 `Continue` 但没有任何变化。** `Continue` 按设计表示
  "无变化"。要取消/阻止/变换，请返回对应的变体 —— 并注意变体特定
  语义（非 `ToolCall` 接缝处的 `Block` 会被忽略；见上文的宿主接缝
  映射）。`emit` 将每个处理器调用隔离在 `catch_unwind` 之后：panic
  的处理器会通过 `tracing` 记录日志并被跳过 —— 它不会让 agent 循环
  崩溃 —— 处理器返回 `Err` 同样会被记录 + 链继续。
- **`configure` 捕获的 `Arc<dyn ExtensionApi>` 现在返回
  `StaleContext`。** runner 已被 `invalidate()`（通过
  `/extension reload` 或未来的 reload/fork/switch）；请捕获新的 api，
  或在使用前对照活跃 runner 检查 `generation()`。
- **测试在 `tokio runtime blocking/shutdown.rs` 处 panic。** 在运行时
  worker 线程内创建并 drop 了一个嵌套的 tokio 运行时。
  `build_extension_runtime` 正是为了避免这一点而在普通 OS 线程
  （`std::thread::scope`）上驱动 `configure` —— 如果你看到此现象，
  说明 thread::scope 守卫被绕过了。
