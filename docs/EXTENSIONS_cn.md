# 扩展

CodeSmith 扩展是编译内置（slice 1，§F1）或待加载（phase 2，§F5）的模块，它们向 agent 循环贡献**工具**、**斜杠命令**和**生命周期事件处理器**。它们是移植到 §E framework-core trait 之上的 pi-mono `Extension` 模型。

扩展是一个工厂（`impl Extension`），它在 `configure` 期间向 `ExtensionApi` 注册自己的贡献项。宿主在启动时通过 `inventory` 发现编译内置的扩展，与磁盘上的 `ExtensionStateStore` 对账（跳过已禁用的），针对一个 stub api 逐个加载并配置，然后 `bind_core` 宿主上下文 —— 之后 runner 将生命周期事件分发给已注册的处理器。根据 §F5d（T1+T2），扩展贡献的工具 + 斜杠命令按轮（per-turn）实时接入宿主：工具通过 `EngineHost::build_turn_dispatcher` 中的 `register_extension_tools` 注册到每轮的 `ToolRegistry`，斜杠命令通过 `commands::execute` 中的 `try_dispatch_extension_command` 分发 —— 因此 agent 循环将扩展工具视为普通的 `ToolSpec`（仅限主轮；不会被子代理继承 —— 见"沙箱立场"一节）。

> **Slice 状态。** §F1（编译内置扩展 + 最小 6 事件契约）、§F2a（完整 23 变体 `ExtensionEvent` 集合 + `HandlerOutcome` cancel/block/transform 链 + 按变体订阅 + `catch_unwind` 隔离）以及 §F2b（宿主接缝布线 —— 在 7 个 `host_executor` 接缝处遵守 `EmitOutcome` + 发出 22/23 个事件 + 完整 e2e 往返 + 实时重载）均已完成。Dylib 加载、`extension.toml` 清单、安装/卸载、`registerProvider`、渲染器、快捷方式、标志、`EventBus` 实现推迟到 §F3–§F8。§F2c（重载共享引擎的活跃 `cancel_token`；`on_tool_progress` `Callback` 钩子作为面向 `ToolExecutionUpdate` 的前瞻性 API 面；`ProjectTrust` 每轮接线）已完成。§F5 slice 1（在 onboarding 信任接受点发出 `ProjectTrust { FirstLoad }` —— 用户接受工作区信任提示时扩展处理器观察到的每会话一次的信号）已完成；§F5 dylib 的 LOAD 侧（`libloading` + `extension.toml` 清单 + 三形态发现 + 项目本地信任门控 [Model A —— 消费 `is_workspace_trusted(workspace)`/`FirstLoad`] + 重载接线）落地于 §F5b；INSTALL 侧（Git/LocalPath 源 + `CargoBuilder` + `Placer` + `Installer` 编排器 + `/extension install`/`uninstall` 真实实现 + `installed[]` 来源记录写入）落地于 §F5c。§F5e（已完成）补充了真实的 `crate:`/`prebuilt:` 源实现（此前是 §F5c 的 "§F5c-later" 桩）。§F5d（已完成）将扩展工具 + 斜杠命令按轮实时接入宿主（T1 工具通过 `EngineHost::build_turn_dispatcher` 中的 `register_extension_tools`；T2 命令通过 `commands::execute` 中的 `try_dispatch_extension_command`）并增加了安全卸载：重载时的 `clear_tools`/`clear_commands`（T3）+ 两阶段 `Library` drop（`pending_drop` + UI 线程上的 `drain_libraries_to_pending` + 引擎 op-loop 轮边界处的 `drop_pending`，T4）—— 因此已卸载扩展的活跃绑定会在下一次 `/extension reload` 时清除，dylib 在下一个轮边界安全卸载（无 UB；扩展工具仅限主轮 + 永不被子代理继承，§4b 结构性保证）。（§F5 slice 1 仅发出 `FirstLoad` *事件* —— 没有 dylib 机制。）`ToolExecutionUpdate`（需要流式 `Tool` 契约 —— `Tool::run` 是一次性的）、`ResourcesDiscover` 和 `SessionBeforeFork` 保持推迟，理由已修正（见宿主接缝表）。Hot-load 永久排除（spec §2.4）—— 仅限 install + reload。

## 引导（Bootstrap）

Slice 1 扩展通过 [`inventory::submit!`](https://docs.rs/inventory) 编译进二进制。在 `crates/extensions/src/lib.rs` 中有一个 `pub mod` + 一条 `pub mod sample_scratchpad;` 声明就是发现所需的全部 —— 无需运行时注册调用。宿主的 `build_extension_runtime()`（位于 `crates/tui/src/core/engine.rs`）在引擎构建时调用一次 `codesmith_extensions::discover_static()`。

## TUI 内管理器

`/extension` 命令组（spec §6.3）是面向用户的界面。它通过 `extension_commands::try_dispatch` 分发，接入到 `execute()` 中用户自定义命令与静态 `match` 之间。

| 子命令 | 别名 | 状态（slice 1） | 效果 |
|---|---|---|---|
| `/extension list` | `ls` | ✅ 可用 | 列出编译内置的扩展（id + 版本）。 |
| `/extension info <id>` | | ✅ 可用 | 显示单个扩展的元数据。 |
| `/extension enable <id>` | | ✅ 可用 | 在 `extensions_state.toml` 中将扩展标记为启用；在下一次 `/extension reload` 时生效（§F2 接入实时重新对账）。 |
| `/extension disable <id>` | | ✅ 可用 | 将扩展标记为禁用；同样的重载注意事项。 |
| `/extension status` | | ✅ 可用 | 报告已绑定 runner 的 generation + 已绑定的命令/工具计数。 |
| `/extension reload` | | ✅ 可用（实时重载） | 重新填充**共享的 runner `Arc`**：`clear_handlers` → `clear_tools` → `clear_commands` → `drain_libraries_to_pending`（§F5d T3+T4）→ `invalidate`（递增 generation）→ `discover_static` + `discover_dylib` → 与状态对账 → 逐个 `load` → `bind_core`（全新的 `HostExtensionContext`）。`App.extension_runner` 和 Engine 的字段都会实时更新（没有 `Arc` 交换 —— 它们共享引擎构建的那一个）。被排空的 `Library` 会在引擎 op-loop 的下一次顶部（轮边界，§F5d T4）被 `drop_pending`。重载前绑定的处理器之后不再观察（被清除，而不是重复）；新编译内置的扩展会在下一次重载时被拾取。 |
| `/extension install <source> [--global]` | | ✅ 可用（§F5c） | 拉取（`git:`/`path:`）→ 构建（`cargo build`）→ 放置到 `<root>/<id>/` + 写入 `extension.toml` + 记录 `installed[]` 来源；`--global` 为可选（默认为项目级）。`crate:` 从 crates.io 拉取（sparse-index → 版本 → sha256 校验的 `.crate` → `tar` 解压 → 构建）；`prebuilt:<https-url>` 拉取预构建的 cdylib（仅限 HTTPS，可选 `--checksum <sha256>`）；两者在项目级且未受信任时都会警告；用 `/extension reload` 加载。 |
| `/extension uninstall <id>` | | ✅ 可用（§F5c） | 移除 `<root>/<id>/` + 清除 `installed[]` 来源记录。活跃的工具/命令绑定在下一次 `/extension reload` 时清除；dylib 在下一个轮边界安全卸载（§F5d 两阶段 drop）。 |

## 发现

- **Phase 1（slice 1，静态）：** 编译内置的扩展通过 `inventory::submit!` 注册一个 `ExtensionRegistration { factory, metadata }`。`discover_static()` 收集链接进二进制的每一个 `ExtensionRegistration`。树内 `scratchpad` 示例是参考注册。
- **Phase 2（§F5，已交付）：** 从安装根加载 dylib + `extension.toml` 清单 + 信任提示 + `ExtensionSource` / `ExtensionBuilder` / `ExtensionPlacer` trait 实现（Git / LocalPath / CratesIo / PrebuiltDylib —— §F5c 接线 Git/LocalPath，§F5e 接线 CratesIo/Prebuilt）。宿主工具/命令由 §F5d 按轮实时接线；`/extension install`/`uninstall`/`reload` 为真实实现（两阶段 `Library` drop 在轮边界安全）。

## 最小示例

树内 `scratchpad` 扩展（`crates/extensions/src/sample_scratchpad.rs`）贡献了全部三个 slice-1 贡献点。原文摘录：

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

`/extension list` 会报告 `scratchpad`；`/extension info scratchpad` 显示其元数据。完整的工具/命令/处理器主体见该文件。

## 扩展字段（trait 契约）

所有契约位于 `crates/agent/src/extension.rs`。扩展作者依赖 `codesmith-extensions`（它 re-export `codesmith_agent::extension::*`），这样一个 crate 就同时提供了 trait 和运行时辅助。

- **`Extension`** —— 工厂：`metadata() -> &ExtensionMetadata` + `async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError>`。
- **`ExtensionApi`** —— 注册面（两阶段：加载时为 stub，`bind_core` 时为真实实现）：`register_tool(Box<dyn ToolDefinition>)` / `register_command(Box<dyn CommandDefinition>)` / `on(Arc<dyn Handler>)`（订阅所有事件）/ `on_variant(ExtensionEventKind, Arc<dyn Handler>)`（仅订阅一个变体 —— §F2a）+ 用于过期上下文防护的 `generation() -> u64`。
- **`ExtensionContext`** —— 交给处理器的以读为主的宿主状态：`cwd() / mode() / is_idle() / signal() / generation()`（slice 1 中为真实实现）；`abort() / shutdown() / compact() / get_context_usage()`（桩化 → `Unimplemented`；§F2 会接线它们）。
- **`ExtensionCommandContext: ExtensionContext`** —— 交给命令处理器的严格子 trait；slice 1 未添加任何会话变更方法（这一拆分是为了类型安全 + §F2 的扩展）。
- **`ExtensionEvent`** —— `#[non_exhaustive]`；§F2a 落地了完整的 23 变体集合（§F1 的 6 个 + 17 个新变体：`ProjectTrust`/`ResourcesDiscover`/`Input`/`BeforeAgentStart`/`AgentStart`/`BeforeProviderHeaders`/`BeforeProviderRequest`/`AfterProviderResponse`/`ToolExecutionStart`/`ToolExecutionUpdate`/`ToolExecutionEnd`/`AgentEnd`/`AgentSettled`/`SessionBeforeSwitch`/`SessionBeforeFork`/`SessionBeforeCompact`/`SessionCompact`）。`ExtensionEvent::kind()` 将每个变体映射到一个 `ExtensionEventKind` 判别值，用于按变体分发。
- **`Handler`** —— §F2a 返回结果（§F1 仅观察者；已被取代）：`async fn handle(&self, event: &ExtensionEvent, ctx: &dyn ExtensionContext)
  -> Result<HandlerOutcome, ExtensionError>`。返回 `Continue`（无变化；继续）、`Cancel { reason }`（中止周围的操作 —— 仅对 `SessionBefore*` 变体有意义）、`Block { reason }`（阻止操作 —— 仅对 `ToolCall` 有意义）或 `Transform(ExtensionEvent)`（替换正在运行的事件供后续处理器使用，并在具备变换能力的接缝处应用其可执行字段 —— `Input`/`BeforeAgentStart`/`BeforeProviderRequest`/`ToolResult`）。变体特定语义由宿主在每个接缝处强制执行（§F2b）；不合时宜的结果（例如在 `TurnEnd` 处 `Block`）会被忽略（按 `Continue` 处理）。`emit` 按注册顺序链接处理器，因此 `Transform` 对下一个处理器可见；`Cancel`/`Block` 短路。
- **`ToolDefinition`** —— 扩展侧工具契约：`name / description / input_schema / capabilities / async execute(input, ctx)`。`execute` 接收一个 `ExtensionContext`（而不是宿主的 `ToolContext`）—— 使扩展与 `ToolContext` 的约 30 个宿主耦合字段解耦。
- **`CommandDefinition`** —— 扩展侧斜杠命令契约：`name / description / async run(ctx, args) -> CommandOutput`。由宿主的 `extension_commands::try_dispatch` 分发。
- **`ExtensionError`** —— `StaleContext`（防护信号）+ `Config` / `Tool` / `Command` / `Conflict` / `Install` / `Load` / `Unimplemented`。

## 处理器：结果 + 按变体订阅（§F2a）

§F1 的处理器是观察者（`Result<(), _>`）。§F2a 将它们升级为**结果链**：`Handler::handle` 返回 `HandlerOutcome`，`ExtensionRunner::emit` 按注册顺序链接处理器 —— `Transform` 对下一个处理器可见，`Cancel`/`Block` 短路。每个处理器调用都通过 `catch_unwind`（§8.3）隔离：panic 的处理器会通过 `tracing` 记录日志并被跳过 —— 它不会让 agent 循环崩溃 —— 处理器返回 `Err` 同样会被记录 + 链继续（尽力而为）。

用 `on` 订阅**所有**事件，或用 `on_variant` 订阅**某一个**变体（runner 在分发前按 `event.kind()` 过滤按变体处理器，因此按变体处理器永远不会看到不匹配的事件）：

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

> 宿主在每个接缝处遵守 `Cancel`/`Block`/`Transform`（§F2b —— 见下文的宿主接缝映射）。§F2a 单独落地了契约 + 链；§F2b 在 `EmitOutcome` 上添加了 `#[must_use]`，因此每个 emit 点都必须检查结果（仅观察接缝使用 `let _ =`）。

## 宿主接缝映射（§F2b）

§F2b 将每个 `ExtensionEvent` 变体接到其宿主 emit 位置，并定义宿主在该接缝处遵守哪些 `HandlerOutcome`。不合时宜的结果（例如在 `TurnEnd` 处 `Block`）会被忽略 —— 按 `Continue` 处理 —— 因此为其变体返回错误能力的处理器是无操作（no-op），而不是错误。`EmitOutcome` 是 `#[must_use]`，因此每个 emit 点都会绑定结果（仅观察接缝使用 `let _ =`；能力接缝检查 `out.outcome` / `out.event`）。

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
| `ToolCall(ToolCallEvent)` | `host_executor` 并行 + 串行工具分发 | **Block** | 跳过审批 + `tool.run` → `Err(ToolError::permission_denied(reason))`，`blocked = true` |
| `ToolResult(ToolResultEvent)` | `host_executor` 并行 + 串行，emit 重排到 `on_tool_end` 之前 | **Transform** | 替换结果；`on_tool_end` + 下游 `outcomes[idx].result` 看到的是变换后的结果 |
| `ToolExecutionStart` | `host_executor` 工具闭包（`tool.run` 之前） | observe | — |
| `ToolExecutionEnd` | `host_executor` 工具闭包（`tool.run` 之后） | observe | — |
| `AgentEnd` | `host_executor::run_inner` 每个 `return Ok(...)` | observe | — |
| `AgentSettled` | `engine/mod.rs` 运行后排空（容量应用之后） | observe | — |
| `SessionBeforeCompact` | `host_executor::run_compaction` 在 `should_compact` 门控之后 | **Cancel** | 跳过压缩（`return`） |
| `SessionCompact` | `host_executor::run_compaction` 在摘要应用之后 | observe | — |
| `SessionBeforeSwitch` | `tui/ui.rs` `switch_workspace` 入口 | **Cancel** | 中止工作区切换 |
| `ProjectTrust` | `HostServices::build_turn_dispatcher`（+ `spawn_subagent`）在 `build_tool_context_for` 之后（每轮 `Trusted`/`Untrusted`）；onboarding 信任接受时 `tui/ui.rs` `TrustDirectory` y/Y/1 分支在 `app.trust_mode = true` 之后（`FirstLoad`） | observe | 来自 `session.trust_mode` 的每轮 `Trusted`/`Untrusted`；`FirstLoad` 在每次 onboarding 信任接受时触发一次（`TrustReason::FirstLoad`）—— 不同于运行时 `trust_mode` 开关（`/trust on`）、YOLO 进入和持久化信任启动，后者在每轮表现为 `Trusted`/`Untrusted`，而不是 `FirstLoad` |
| `—`（dylib LOAD，不是事件） | `populate_extension_runtime`（`tui/src/core/engine.rs`）在 `discover_static` 之后 | n/a（加载阶段） | §F5b：`discover_dylib(&global_roots, &project_roots)` → `apply_trust_gate(discovered, !is_workspace_trusted(workspace))` 丢弃项目本地（`global == false`）→ `state.is_enabled` 对账 → 在 OS 线程加载运行时上执行 `ExtensionRunner::load_dylib`；重载通过 `reload_extension_runtime`→`populate` 自动拾取。`ExtensionRunner.libraries` 持有 `Library` 句柄；在 `/extension reload` 时它们 `drain_libraries_to_pending` 到 `pending_drop`（§F5d T4，与 `clear_tools`/`clear_commands` 一并执行）+ 引擎 op-loop 在下一个轮边界对它们执行 `drop_pending`。通过 `codesmith_register_extension`（§8.2）实现 lockstep `*mut dyn Extension`。 |
| `ResourcesDiscover` | —（推迟至 §F2c） | observe | 唯一的进程内宿主位置是 `McpPool` 中 `list_mcp_resources` 伪工具的分发（`agent-runtime/src/mcp.rs:3014`），已被 `ToolCall`/`ToolResult` 包夹 —— 在那里触发 `ResourcesDiscover` 会与工具执行混淆，且 `DiscoverReason` 没有干净的映射；也没有持有 runner `Arc` 的专用 Startup/Manual/Reload 发现接缝。`tui/mcp_server.rs` 的 stdio 位置是一个独立进程。（早先"独立进程"的说法夸大了阻碍。） |
| `SessionBeforeFork` | —（推迟至 §F2c） | **Cancel** | 实际的 TUI 内回退路径（`apply_backtrack`，`tui/ui.rs:6922`）是就地**回退（rewind）**（`truncate_history_to`/`api_messages.truncate`），而不是**分叉（fork）**（创建新线程）—— 若接为 `SessionBeforeFork` 属于误标。真正的 fork 原语已死（`fork_at_user_message`，`#[allow(dead_code)]`，零个非测试调用方）或仅限 HTTP（`fork_thread`，runtime-api，没有 `App.extension_runner`）。TUI **确实**通过 `TaskManager::start`（`ui.rs:507`→`task_manager.rs:465`）构造了 `RuntimeThreadManager` —— 早先"没有构造函数"的说法是错误的。（规范可以将该事件重新定义为涵盖 rewind；已标记给规范负责人，此处未做。） |
| `ToolExecutionUpdate` | —（推迟至 §F2c） | observe | 没有流式 `Tool` 契约 —— `Tool::run` 是一次性的（`agent/src/tools/mod.rs:71`），因此没有可在执行中途挂钩的进度流。`on_tool_progress` `Callback` 钩子已落地（§F2c T1）作为前瞻性 API 面；emit 位置等待流式 `Tool` 变体（§F-later）。（早先"没有 `on_tool_progress` 钩子"的说法是表面症状，不是根因。） |

> `Transform` 载荷的可执行字段在完整处理器链运行之后才于接缝处应用（因此来自处理器 N 的 `Transform` 会作为正在运行的事件对处理器 N+1 可见）。`Cancel`/`Block` 短路该链。最终的 `EmitOutcome.outcome` 永远不会是 `Transform`（已折叠进 `EmitOutcome.event`）；能力接缝检查 `out.outcome` 中的 `Cancel`/`Block`，检查 `out.event` 中的变换后可执行字段。

## 沙箱立场

CodeSmith **不**对扩展做沙箱隔离（spec §8.1）。扩展与 agent 循环运行在同一进程中，拥有完整的宿主访问权 —— **信任其来源**。对于不受信任的扩展，请将整个 CodeSmith 进程容器化。项目本地 dylib 安装（phase 2，§F5）将在首次加载前要求信任提示。`ProjectTrust { FirstLoad }` 事件（§F5 slice 1）现在会在 onboarding 信任接受时触发 —— 它是扩展处理器可以订阅的*仅观察信号*，与*消费*项目本地信任的 phase-2 dylib 加载器是两回事（且不交付该加载器）。Dylib 加载器（`libloading` + 通过 `codesmith_register_extension` 的 lockstep `*mut dyn Extension`）、`extension.toml` 清单以及项目本地发现信任门控（Model A —— 当 `is_workspace_trusted(workspace)` 为 false 时 `apply_trust_gate` 丢弃项目本地（`global == false`）的 dylib；`ProjectTrust { FirstLoad }` 事件在 onboarding 接受时翻转该信任）属于 §F5b（已完成）。§F5c（已完成）添加了 INSTALL 侧：`/extension install` 拉取（`git:`/`path:`）→ `cargo build --release --locked` → `Placer` 写入 `<root>/<id>/<default_dylib_filename(id)>` → `extension.toml` → `installed[]` 来源记录。`cargo build` 会运行源码的 `build.rs` —— **任意代码执行，按 §8.1 接受（信任来源）**；对不受信任的来源请容器化。安装与信任无关（它只*读取*信任以发出警告：项目本地安装在工作区受信任之前不会加载）。已加载的 dylib 在进程内运行并拥有完整的宿主访问权 —— 信任其来源；对不受信任的来源请容器化。`crate:`/`prebuilt:` 源随 §F5e 交付（真实的 `CratesIoSource`/`PrebuiltDylibSource` 实现；此前是 §F5c 的 "§F5c-later" 桩）。§F5d（已完成）将扩展工具 + 斜杠命令实时接入宿主的每轮 `ToolRegistry`（仅限主轮）并增加了安全卸载：

- **扩展工具仅限主轮（§4b 结构性）：** 扩展工具注册到宿主的每轮 `ToolRegistry`（主 agent 轮），不会被子代理继承。这是**结构性的，而非守卫**：`SubAgentRuntime` 没有 `extension_runner` 字段 + `SubAgentToolRegistry::new` 会重建自己全新的内置 `ToolRegistry` —— 因此无论 `inherit_full_registry` 如何，扩展工具都永远无法进入子代理的有效集合。不需要来源标记 / 强制子集 / 运行时子代理检查。
- **两阶段 `Library` drop（§4a）：** UI 线程上的重载将孤儿 `Library` 移动到 `pending_drop`（`drain_libraries_to_pending`）；引擎 op-loop 顶部在主线程 `HostAgentExecutor`（唯一持有在飞 dylib `Arc` 的角色）已在轮间被 drop 的那一刻将其 DROP（`drop_pending`）。这使得 `/extension reload` + 卸载可以与在飞的轮安全并发。
- **Miri 说明：** 两阶段 drop 的安全性由不变量 + 单调用点纪律证明；dylib+Miri 不可靠（libloading 的 `Library::drop` 会运行 `dlclose`/`FreeLibrary`，而 Miri 不对其建模），因此证明来自不变量 —— 而不是一次 Miri 运行。

Slice 1 的编译内置扩展在构造上就是可信的（它们随二进制发布）。

## 故障排查

- **`/extension list` 什么都不显示。** 没有 `inventory::submit!` 到达链接 —— 确认扩展的 crate 是 workspace 成员，且 `crates/extensions/src/lib.rs` 声明了其模块。`cargo test -p codesmith-extensions scratchpad_is_discoverable` 可证明注册已接线。
- **`/extension status` 显示 "not bound"。** 引擎尚未构建（启动前），或 `app.extension_runner` 没有从句柄复制（`crates/tui/src/tui/ui.rs` 中 `spawn_engine` 之后）。
- **处理器返回 `Continue` 但没有任何变化。** §F2a 处理器返回 `HandlerOutcome`；`Continue` 按设计表示"无变化"。要取消/阻止/变换，请返回对应的变体 —— 并注意变体特定语义（非 `ToolCall` 接缝处的 `Block` 会被忽略；见下文的宿主接缝映射）。`emit` 将每个处理器调用隔离在 `catch_unwind`（§8.3）之后：panic 的处理器会通过 `tracing` 记录日志并被跳过 —— 它不会让 agent 循环崩溃 —— 处理器返回 `Err` 同样会被记录 + 链继续。
- **`configure` 捕获的 `Arc<dyn ExtensionApi>` 现在返回 `StaleContext`。** runner 已被 `invalidate()`（通过 `/extension reload` 或未来的 reload/fork/switch）；请捕获新的 api，或在使用前对照活跃 runner 检查 `generation()`。
- **测试在 `tokio runtime blocking/shutdown.rs` 处 panic。** 在运行时 worker 线程内创建并 drop 了一个嵌套的 tokio 运行时。`build_extension_runtime` 正是为了避免这一点而在普通 OS 线程（`std::thread::scope`）上驱动 `configure` —— 如果你看到此现象，说明 thread::scope 守卫被绕过了。
