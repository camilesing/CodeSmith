# codesmith 架构

本文档为开发者和贡献者提供 codesmith 架构的概览。
深度设计叙述（provider 接缝、框架核心执行器、guardrail 吸收、扩展系统）见
[DESIGN_INTERNALS_cn.md](DESIGN_INTERNALS_cn.md)。

当前边界说明（v0.11.0）：
- **框架层 / 实现层拆分。** 工作区分为框架群 —— `protocol`、`tools`、`agent`、
  `config`、`secrets`、`extensions`、`agent-runtime`（内核：经
  `TurnExecutorRegistry` 组合、位于 `AgentExecutor` 接缝之后的 turn 循环、
  引擎、压缩）—— 与实现群：
  其余全部（`providers`、`tool-impls`、`cli`、
  `app-server`、`mcp`、`state`、`execpolicy`、`hooks`、`index`、`core`、
  `release`、测试 fixture）。框架群 crate 在构建图中不得依赖框架群
  之外的任何 workspace crate；实现群可以自由依赖框架群。该规则由
  `scripts/check-framework-deps.py`（扫描 cargo metadata，已接入 CI）物理强制；
  dev-dependency 豁免，因为它不会进入框架产物。新能力遵循接缝三件套：定义落在
  框架 crate、提供者落在实现 crate、消费者不限。现成范例：
  `IndexServiceApi` 及其值类型定义于 `codesmith-tools::index_api`，由
  `codesmith-index` 实现并原路径 re-export，内核工具上下文
  （`agent-runtime/src/tools/spec.rs`）消费 —— 与 `LlmClient`（定义于
  `codesmith-agent`、实现于 `codesmith-providers`）以及统一 Web 服务
  （`WebService` 定义于 `agent-runtime/src/web_service.rs`、
  `HttpWebService` 实现于 `codesmith-tool-impls::web_service`，
  由 `web_search` / `fetch_url` / `web.run` 工具经 `ToolContext` 消费）
  同构。
  生成式能力图谱（`docs/CAPABILITY_GRAPH.md`，由
  `scripts/capability-graph.py` 生成、CI 校验新鲜度）列出每个接缝的
  三件套——新能力须在同一变更中落齐三个角色；每个
  `ExtensionEventKind` 声明分发模式（`dispatch_mode()`，由契约测试
  锁定）。
- **会话持久化接缝。** 持久会话记录（`SavedSession`）、格式治理解码
  管线（schema 门禁、相邻内存迁移、未知块容忍、中断回合修复）与
  `SessionStore` 接缝位于 `agent-runtime/src/session_store.rs`；文件
  提供者（`FileSessionStore`，每会话一个 JSON 文件 + 崩溃检查点）同
  模块——按 `FileKeyringStore` 先例允许提供者留在框架 crate。消费者：
  CLI REPL 恢复流程与 runtime API 的会话端点（dyn 分发）。已知限制：
  每会话单写者；app-server 线程运行时保留自己的存储
  （`codesmith-state`）作为查询索引，不持久化 REPL 会话——在那里扩展
  恢复能力应消费本接缝，而不是再造第三种格式（曾考虑
  `ThreadResumeParams.session_id` 扩展并已放弃：CLI runtime API 已提供
  从保存会话恢复的能力，且 core/app-server 目前没有对 agent-runtime 的
  依赖边）。
- **插件分层（已声明）。** 可插拔性统一走 `Extension` 契约
  （`crates/agent/src/extension.rs`）+ `ExtensionRunner`——共四层：用户
  Rhai mod（`~/.codesmith/mods/`，consent 门控）、内建 Rhai 层（内嵌脚本
  默认值，`crates/extensions/src/script/builtin.rs`）、编译内置 Rust 扩展
  （`inventory`）、安装的 Rust dylib（`/extension install` + reload）。
  新的可插拔行为落在其中一层上——逻辑/数据用 Rhai，异步/IO 用 Rust
  扩展——而不是新开一个编译期接线点。注册面是作用域感知的
  （`Scope::Global | Scope::AgentType(id)`，DSH 对齐）：scoped 工具在该
  agent 类型的目录视图里遮蔽同名 global 工具
  （`ExtensionRunner::bound_tools_scoped`），deny-only 的
  `restrict_tools` 名单按视图剪掉扩展层的 global 工具，主轮目录
  （`register_extension_tools`）解析 Global 视图，子代理注册表按来源分层
  解析自己的视图（5b：script/compiled-in 工具可挂载进子代理，dylib 工具
  保持仅主轮——detached 后台子代理可能活得比一次 reload 的两阶段 dylib
  drop 更久）。唯一的 `BASE_PROMPT` 与子代理工具注册表的内建表面是
  契约声明的核心，不做插件化；主 turn 循环是可组合的内核服务（DSH 内核
  组合模型）——引擎从进程共享的 `TurnExecutorRegistry`
  （`engine/executor_composition.rs`）解析按配置选择的 factory
  （`turn_executor`，默认 `builtin`），替代循环以 crate 内编译的注册条目
  落地，并以 `AroundTurnHandler` 链（`register_around_turn`，DSH
  `next()`，Rust 扩展，仅主 turn）包裹 run（逐表面地图见 MODS_cn.md
  "该用哪一层"）。
- `crates/cli`（包 `codesmith-cli`，二进制 `codesmith`）是活跃的终端用户运行时：既承载无头（headless）子命令，也承载基于行的交互式 REPL 前端（`crates/cli/src/interactive/` —— ratatui TUI 已在 v0.10.0 移除），并负责运行时 API、任务管理器和工具注册表接线。Agent 执行引擎本身（turn 循环、压缩、沙箱辅助程序、提示词）现在位于 `crates/agent-runtime`；`crates/cli/src/core/` 是一个薄的重导出 + 构造桥接层（`engine.rs` 定义 `EngineHost`/`build_engine`）。
- 其他工作区 crate 正在逐步拆分，但它们尚不是唯一的运行时事实来源。
- 启动信任边界的细节记录在 `docs/STARTUP_TRUST_BOUNDARY_AUDIT.md` 中；该审计是信任前与信任后初始化跟进事项的当前参考。
- LSP 子系统（`crates/cli/src/lsp/`）已完全接入引擎的工具执行后路径
  （`crates/agent-runtime/src/engine/lsp_hooks.rs` + `engine/turn/postprocess.rs`），在每次 edit_file/apply_patch/write_file 之后提供内联诊断。
- 编排面是持久子代理会话（`agent_open` / `agent_eval` / `agent_close`）和持久 RLM 会话（`rlm_open` / `rlm_eval` / `rlm_configure` / `rlm_close`），再加上模型可写编排的 `workflow` 工具（2026-10-26 创始人决策重新开启）：一段 Python 脚本跑在一次性的 RLM 内核上，经注入的辅助函数（`run_agent` / `agents` / `log`）同步驱动子代理；接缝三元组——定义在 `agent-runtime/src/workflow.rs`（`WorkflowEngine` + `SubAgentSpawner`），provider 与消费者在 `crates/cli/src/tools/workflow/`。已知限制：没有回注运行中回合的异步结算/唤醒（脚本在自己的工具调用内等待子级）；墙钟上限（默认 600s、上限 1800s）到点即杀内核并取消在途子级；子代理的并发上限、spawn 深度预算、审批天花板与 `SessionBeforeFork` fork 否决均继承自其复用的子代理运行路径。
  不再保留 swarm 式对等网状工具。
- 子代理默认在进程内运行；**外部子代理接缝**让子代理在进程外运行而不改变模型可见面。定义：`crates/agent-runtime/src/subagent.rs` 中的 `ExternalSubagentProvider`（及请求/结果类型）；两个 provider，均在 `crates/cli/src/tools/subagent/`：自身 headless provider（`headless.rs` —— 每个非 team 子代理变成一个 `codesmith exec --json` 子进程，spawn 深度预算经 `CODESMITH_SUBAGENT_DEPTH` / `CODESMITH_MAX_SPAWN_DEPTH` 继承）和 ACP provider（`acp.rs`，v0.11.0 —— 把每次子代理运行委托给 `[subagents.acp]` 下配置的任意 Agent-Client-Protocol stdio agent，走与本仓库自身 `--acp` 服务端适配器相同的 JSON-RPC 基线；只返回一个终态结果、中途进度不回流、外来 agent 不继承 spawn 深度预算、`session/request_permission` 按父方 `auto_approve` 姿态自动应答 —— 完整清单见该模块）；消费者：CLI 的 spawn 路径（`run_subagent_task`），由 `[subagents] external_provider = "headless" | "acp"` 选用（`headless.rs` 的 `provider_from_config`；未知名称、或 `"acp"` 缺少 `[subagents.acp] command`，都在配置加载时即报错）。fork 上下文、`agent_send_input` 以及 team/coordinator 子代理按设计留在进程内（接缝定义处有文档）。Codex / Claude Code provider 仍是该接缝未建的槽位。

## 高层概览

```
┌─────────────────────────────────────────────────────────────────┐
│                         User Interface                          │
│  ┌─────────────────┐  ┌─────────────────┐  ┌────────────────┐  │
│  │ REPL (line mode)│  │  One-shot Mode  │  │  Config/CLI    │  │
│  └────────┬────────┘  └────────┬────────┘  └────────┬───────┘  │
└───────────┼─────────────────────┼────────────────────┼──────────┘
            │                     │                    │
            ▼                     ▼                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                        Core Engine                              │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │     Agent Loop (crates/agent-runtime engine)            │   │
│  │  ┌─────────┐  ┌─────────────┐  ┌──────────────────────┐ │   │
│  │  │ Session │  │ Turn Mgmt   │  │ Tool Orchestration   │ │   │
│  │  └─────────┘  └─────────────┘  └──────────────────────┘ │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
            │                     │                    │
            ▼                     ▼                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Tool & Extension Layer                      │
│  ┌──────────┐  ┌──────────┐  ┌─────────┐  ┌────────────────┐   │
│  │  Tools   │  │  Skills  │  │  Hooks  │  │  MCP Servers   │   │
│  │ (shell,  │  │ (plugins)│  │ (pre/   │  │  (external)    │   │
│  │  file)   │  │          │  │  post)  │  │                │   │
│  └──────────┘  └──────────┘  └─────────┘  └────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
            │                     │                    │
            ▼                     ▼                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                  Runtime API + Task Management                  │
│  ┌─────────────────────────────┐  ┌──────────────────────────┐  │
│  │ HTTP/SSE Runtime API        │  │ Persistent Task Manager  │  │
│  │ (runtime_api.rs)            │  │ (task_manager.rs)        │  │
│  └─────────────────────────────┘  └──────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
            │                     │
            ▼                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                        LLM Layer                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │        LLM Client Abstraction (codesmith-agent llm_client)│  │
│  │  ┌─────────────────┐  ┌─────────────────────────────┐    │  │
│  │  │ Provider registry│  │  Provider impls            │    │  │
│  │  │ (codesmith-agent)│  │  (codesmith-providers)     │    │  │
│  │  └─────────────────┘  └─────────────────────────────┘    │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## 模块组织

### 入口

- **`main.rs`** - CLI 参数解析（clap）、配置加载、入口路由

### 核心组件

- **`crates/agent-runtime/src/engine/`** - Agent 执行引擎（已从终端用户前端迁出）
  - `mod.rs` - `Engine` 结构体 + 事件循环（`Engine::run`、`handle_send_message`）、系统提示词刷新、图片块组装
  - `host_executor.rs` - `HostAgentExecutor`，生产环境的 LLM↔工具 turn 循环，吸收了全部护栏（loop-guard、LSP flush、透明重试、steer、审批、压缩、容量、early-tool-start、子代理、cycle）
  - `executor_composition.rs` - turn 执行器组合（内核组合点）：每 turn 的 `TurnDeps` 快照、从进程共享 `TurnExecutorRegistry` 按配置解析的 `TurnExecutorFactory`（默认 `builtin` = `host_executor.rs` 的装配），以及 dyn-safe 的 `TurnHarvest` 收割（take_*）面
  - `turn/` - 从 `host_executor.rs` 按阶段拆出的每步机制模块：`stream.rs`（流式归约 + 透明重试）、`batches.rs`（工具批次规划/执行）、`approval.rs`（逐工具审批门控）、`seams.rs`（取消检查点 + steer 排空）、`postprocess.rs`（子代理收割门控、thinking-only 状态、LSP 收集/刷新）、`truncation.rs`（输出截断/工具输出限界）
  - `capacity_flow.rs` - 容量护栏检查点和干预
  - `loop_guard.rs` / `dispatch.rs` / `tool_catalog.rs` / `tool_execution.rs` / `lsp_hooks.rs` - loop-guard 状态、工具输入解析 + 批次规划、延迟工具目录策略、MCP 分发扇出、编辑后 LSP 钩子
- **`crates/cli/src/core/`** - 指向引擎 crate 的薄重导出 + 构造桥接层
  - `engine.rs` - `EngineHost`（具体的 `ShellManager`/`LspManager`/`SubAgentManager` 宿主服务）、`EngineHandle`（UI 侧邮箱）、`build_engine` 组装
  - `engine/{handle,runtime_traits,tool_setup,tests}.rs` - handle 管道、宿主 trait、工具注册表设置、测试
  - `session.rs` / `turn.rs` / `events.rs` / `ops.rs` / `tool_catalog.rs` / `capacity.rs` / `coherence.rs` - agent-runtime 类型的重导出 shim

### 配置

- **`config.rs`** - 配置加载、profile、环境变量
- **`settings.rs`** - 运行时设置管理

### 工作区 Crate

- **`crates/agent`** - 框架 AI 核心：`LlmClient` trait + 重试、提供商注册表（ModelRegistry，将模型 ID 解析到提供商端点）、wire 模型。
- **`crates/agent-runtime`** - 统一 agent 执行核心：引擎、turn 循环、压缩、沙箱辅助程序、提示词、子代理。
- **`crates/app-server`** - 用于无头（headless）agent 工作流的 HTTP/SSE + JSON-RPC 应用服务器传输层。
- **`crates/config`** - 配置加载、profile、环境变量优先级、CLI 运行时覆盖。
- **`crates/core`** - 核心运行时边界。
- **`crates/execpolicy`** - 用于工具执行决策的审批/沙箱策略引擎。
- **`crates/extensions`** + **`crates/extensions-fixture-dylib`** - 扩展运行时（发现、加载、事件分发）及其测试夹具 dylib。同时承载 Rhai script-mod 层（`src/script/` —— 同一 `Extension` 契约的脚本载体；见 [MODS_cn.md](MODS_cn.md)）。
- **`crates/hooks`** - 用于工具事件前/后钩子的生命周期钩子（stdout、jsonl、webhook）。
- **`crates/index`** - 按工作区持久化的代码索引（见下方"代码索引"一节）。
- **`crates/mcp`** - 用于 Model Context Protocol 工具服务器的 MCP 客户端 + stdio 服务器。
- **`crates/protocol`** - 请求/响应组帧和协议类型。
- **`crates/providers`** - 位于 Cargo feature 之后的可插拔 LLM 客户端实现（openai-compat、anthropic、mock；rig 适配器）。
- **`crates/release`** - 发布发现 / 版本比较。
- **`crates/secrets`** - 用于 API 密钥存储的操作系统密钥环集成。
- **`crates/state`** - SQLite 线程/会话持久化层。
- **`crates/tool-impls`** - 具体的模型可见工具实现。
- **`crates/tools`** - 共享工具调用原语，包括 CLI 运行时使用的工具结果/错误/能力类型。

### LLM 集成

- **`crates/agent/src/llm_client/`** - 带重试逻辑的抽象 `LlmClient` trait（`LlmClientHandle`、`with_retry`）
- **`crates/agent/src/models.rs`** - API 请求/响应的数据结构（含 `ContentBlock` / `ImageSource` wire 类型）
- **`crates/providers/`** - 具体提供商客户端（openai-compat、anthropic、mock）及 `rig_adapter` 请求塑形器；CLI 通过提供商注册表（`default_registry`）解析活跃客户端

#### DeepSeek API 端点

DeepSeek 提供 OpenAI 兼容端点。CLI 使用：
- `https://api.deepseek.com/beta/chat/completions` - v0.8.16 起默认的 DeepSeek 模型 turn
- `https://api.deepseek.com/beta/models` - v0.8.16 起默认的实时模型发现和健康检查

`https://api.deepseek.com/v1` 被接受用于 OpenAI SDK 兼容，并且仍可被显式配置，
以退出仅限 beta 的功能，例如 strict tool mode、chat prefix completion 和 FIM completion。公开的
DeepSeek 文档并未为此工作流记录 Responses API 路径；引擎通过
Chat Completions 驱动 turn。

### 工具系统

工具实现分为两部分：CLI crate（宿主耦合工具）和 `crates/tool-impls`
（模型可见工具；例如
`grep_files`/`file_search` 位于后者）。

- **`crates/cli/src/tools/`** - 宿主耦合的内置工具实现和注册表
  - `mod.rs` / `registry.rs` - 工具注册表、组装（`with_subagent_tools` 等）和通用类型
  - `shell.rs` - Shell 命令执行（`shell_output.rs` 与其同目录；`command_safety.rs` 在 crate 根部）
  - `file.rs` - 文件读写操作
  - `tasks.rs` / `task_v2.rs` - 模型可见的持久任务、gate、后台 shell 和 PR 尝试工具
  - `github.rs` - 只读 GitHub 上下文以及由 `gh` 支撑的受防护评论/关闭工具
  - `automation.rs` - 基于 `AutomationManager` 的模型可见调度工具
  - `subagent/` - 持久子代理会话（`agent_open` / `agent_eval` / `agent_close`）和持久 RLM 会话（`rlm_open` / `rlm_eval` / `rlm_configure` / `rlm_close` —— 带语义辅助调用和 `var_handle` 输出支持的沙箱化 Python REPL；运行时位于 `crates/cli/src/rlm/`）
  - `skill.rs` / `plugin.rs` / `web_search.rs` / `goal.rs` / `js_execution.rs` / `large_output_router.rs` 等 - 其余工具表面

### 扩展系统

- **`mcp.rs`** - 用于外部工具服务器的 Model Context Protocol 客户端（生命周期在 `crates/mcp`）
- **`skills/`** - 插件/技能加载与执行（发现在 `crates/agent-runtime/src/skills/`，状态在 `skill_state.rs`）
- **`hooks.rs`** - 带条件的执行前/后钩子（分发在 `crates/hooks`）

### 用户界面

- **`interactive/`** - 基于行的交互式 REPL 前端（Claude Code / Codex 风格的循环；取代已移除的 ratatui TUI）
  - `mod.rs` - REPL 循环：读入一行，运行一个引擎 turn，输出流式打印到终端
  - `state.rs` - REPL 会话状态（含已知限制说明）
  - `render.rs` - 行输入处理、Ctrl+C/`Ctrl+D` 语义与流式输出渲染
  - `status.rs` - 提示符上方的状态行（运行状态圆点、preset 档位、上下文窗口占用、模式、子代理数、模型、思考档位、版本）
  - `completion.rs` - Tab 补全与行内提示（rustyline helper：注册表命令名与用户命令；仅补全封闭参数词表）
  - `commands/` - 斜杠命令分发（`/resume`、`/config`、`/skills`、`/queue` 等）；`commands/registry.rs` 是分发、`/help` 与 Tab 补全共读的命令名/别名/帮助表

### LSP 集成

- **`crates/cli/src/lsp/`** - 编辑后诊断注入
  - `mod.rs` - `LspManager` —— 按语言惰性初始化的传输池 + 配置
  - `client.rs` - `StdioLspTransport` —— 基于 stdio 的 JSON-RPC，支持 `didOpen`/`didChange`/`publishDiagnostics`
  - `diagnostics.rs` - 诊断类型、严重级别和 HTML 块渲染器
  - `registry.rs` - 语言检测和默认服务器映射（rust-analyzer、pyright、gopls、clangd、typescript-language-server、jdtls、vue-language-server）
  - 通过 `crates/agent-runtime/src/engine/lsp_hooks.rs`（`run_post_edit_lsp_hook`）接入引擎，收集/刷新对位于 `engine/turn/postprocess.rs` —— 在每次成功编辑后调用

### 代码索引

- **`crates/index`**（`codesmith-index`）—— 按工作区持久化的代码索引（参见 `docs/INDEX.md`）
  - 服务接缝 `IndexServiceApi` / 保留的 `SemanticIndexApi` 及值类型词汇定义于框架层（`codesmith-tools::index_api`）并由本 crate 原路径 re-export；`backend.rs` 保留后端 SPI：`IndexBackendFactory`/`IndexBackend`（提供商注册表模式）
  - `registry.rs` - `IndexBackendRegistry`，镜像 `ProviderRegistry`（upsert；构建错误列表会列出已注册的 id）
  - `tree_sitter.rs` - `tree-sitter` cargo feature 之后的内置符号后端（rust/python/js/ts/go；容器作用域、词法引用）
  - `walk.rs` - 基于 `ignore` 的工作区遍历（感知 `.gitignore`），为清单 + 新鲜度差异提供输入
  - `store.rs` - `~/.codesmith/index/<ws-hash>/` 下的按工作区 SQLite（schema 版本不匹配 → 重建）
  - `service.rs` - `IndexService` 编排：惰性增量刷新（mtime+size 差异、预算 + `stale_files` 报告）及后台补全
- 接线：CLI 为每个工作区构建一次服务（`crates/cli/src/index.rs`、`[index]` 配置）并注入 `RuntimeToolServices::index_service` → 每个 turn 的 `ToolContext`；`symbol_search` / `find_references` 位于 `codesmith-tool-impls` 中，注册通过 `EngineConfig::index_enabled` 按会话恒定门控（目录稳定性、KV 前缀缓存）

### 安全

- **`crates/agent-runtime/src/sandbox/`** - 平台沙箱强制执行辅助程序（经 `crates/cli/src/sandbox/` 重导出）
  - `seatbelt.rs` - macOS Seatbelt profile 生成（已启用）
  - `landlock.rs` - Linux Landlock 规则集应用（已强制执行：规则集由 `prepare_landlock` 在父进程构建，经 `install_sandbox_preexec` 在 fork 与 exec 之间应用；仅文件系统——见其已知局限）
  - `windows.rs` - Windows 沙箱辅助程序契约桩（不宣传任何进程内 Windows 沙箱；要求沙箱的策略以 `SANDBOX_UNAVAILABLE` 失败关闭）
  - `bwrap.rs` - 可选 bubblewrap 直通（`[sandbox] prefer_bwrap = true`，见其模块文档）
- **`crates/cli/src/sandbox/`** - 宿主侧沙箱策略准备和拒绝报告（`mod.rs`、`policy.rs`、`runtime.rs`、外部 OpenSandbox 的 `backend.rs`、`opensandbox.rs`）

### 工具库

- **`utils.rs`** - 通用工具
- **`logging.rs`** - 日志基础设施
- **`compaction/`** - 长对话的上下文压缩（引擎侧流程在 `crates/agent-runtime`）
- **`purge.rs`** - Agent 驱动的上下文清除（精准的消息移除/改写）
- **`pricing.rs`** - 成本估算
- **`prompts.rs`** - 提示词加载 shim（组装后的系统提示词位于 `crates/agent-runtime/src/prompts.rs` + `prompts/` 资产：基础宪法、模式增量、性格叠加、审批策略）
- **`project_context.rs`** - 项目文档处理
- **`session_manager.rs`** - 会话序列化
- **`runtime_api.rs`** - HTTP/SSE 运行时 API（`codesmith serve --http`）
- **`runtime_threads.rs`** - 持久线程/turn/条目存储 + 可重放的事件时间线
- **`task_manager.rs`** - 持久队列、工作线程池、任务时间线和工件

## 数据流

### 交互式会话

1. 在 REPL 中接收用户输入
2. 输入由引擎（`crates/agent-runtime/src/engine/mod.rs`）处理
3. 消息通过 `LlmClient` trait（`crates/agent/src/llm_client/`，来自 `crates/providers` 的提供商客户端）发送到 LLM
4. 响应经流式归约器（`engine/turn/stream.rs`）流式返回
5. 提取工具调用并通过 `tools/` 执行
6. 在工具执行前后触发钩子
7. 结果聚合后回传给 LLM
8. 最终响应在 REPL 中渲染

### 崩溃恢复 + 离线队列

1. 发送用户输入之前，REPL 会将检查点快照写入 `~/.codesmith/sessions/checkpoints/latest.json`
2. 启动默认保持全新状态；先前的会话通过 `--resume`/`--continue`（或 REPL 中的 `/resume` 命令）显式恢复
3. 在降级/离线状态下，新的提示词会在内存中排队并镜像到 `~/.codesmith/sessions/checkpoints/offline_queue.json`
4. 队列编辑（`/queue ...`）会持续持久化，因此草稿和已排队的提示词可在重启后保留
5. turn 成功完成后会清除活动检查点并写入持久会话快照
6. Agent/Yolo turn 还会在 `~/.codesmith/snapshots/<project_hash>/<worktree_hash>/.git` 下创建 turn 前/后的 side-git 工作区快照；`/restore N` 和 `revert_turn` 恢复文件状态，而不更改对话历史或用户的 `.git`

### 工具执行

1. LLM 通过 `tool_use` 内容块请求工具
2. 工具注册表查找处理器
3. 运行执行前钩子
4. 如有需要则请求审批（非 yolo 模式）
5. 执行工具（在 macOS 上可能处于沙箱中）
6. 运行执行后钩子
7. 结果元数据保留在运行时条目记录上
8. **LSP 编辑后钩子**（v0.8.6）：如果工具是 `edit_file`/`apply_patch`/`write_file` 且 LSP 已启用，引擎运行 `run_post_edit_lsp_hook()` 以收集诊断
9. **诊断刷新**（v0.8.6）：在下一次 API 请求之前，`flush_pending_lsp_diagnostics()` 将已收集的错误作为合成用户消息注入
10. 结果返回给 agent 循环

以上是**单次调用**的流程，而一条助手消息可以携带多次调用。引擎把它们分组为
**batch**（`engine::dispatch::plan_tool_execution_batches`）：一个 batch 内部并发执行，
且每个 batch 都在下一个 batch 开始前跑完——所以分组本身就是顺序保证，声明顺序在
任何切分下都不变。一次调用可以进入并发 batch 的条件是：它是只读的，或声明了
`supports_parallel`（sub-agent 派生家族），或它写入的文件路径与该 batch 已累计的路径
不相交。它会独占一个串行 batch 的情况是：它是交互式的、破坏性的，或它写入却没有
声明目标（`exec_shell`——它可能写任何地方）。两次调用命名同一个文件永不共享 batch；
一个路径未知的读工具与写工具也永不共享 batch：对整棵树跑的 `grep_files` 可能正好在
看某个被改写中的文件。单个 batch 最多容纳 `MAX_PARALLEL_BATCH`（10）次调用。
**审批不再把调用挤出并发 batch**：每个需要审批的成员都会在任何 future 派生之前，
按声明顺序在一个串行的预派发环节里被询问——一次拒绝只终止那一个调用，兄弟调用照常
运行。已知缺口：获批的沙箱**提权**无法注入并发 batch，因此这类调用会记录日志并以固定
上下文运行。

### 后台任务

1. 客户端将任务入队（`/task add ...` 或 `POST /v1/tasks`）
2. `task_manager.rs` 将任务 + 队列条目持久化到 `~/.codesmith/tasks` 下
3. 工作线程领取已排队任务（有界线程池），并转为 `running`
4. 任务创建/使用一个运行时线程并启动一个运行时 turn
5. `runtime_threads.rs` 持久化线程/turn/条目记录 + 单调递增事件序列
6. 时间线/工具摘要/工件引用会增量持久化
7. 清单状态、验证器 gate、PR 尝试和受防护的 GitHub 事件从工具元数据应用到活动任务
8. 最终状态（`completed|failed|canceled`）是持久的，可通过 REPL/运行时 API 查询

模型可见的持久任务工具是同一管理器之上的一个表面。它们不会
引入并行的作业系统：`task_create` 入队普通任务，
`checklist_*` 更新任务内的进度，`task_gate_run` 和已完成的
`task_shell_wait` 附上验证证据，自动化运行则入队
普通持久任务。

### 运行时线程/Turn 时间线

1. API/REPL 创建或恢复线程（`/v1/threads*`）
2. 在线程上启动 turn（`/v1/threads/{id}/turns`）
3. 引擎事件被映射为条目生命周期事件（`item.started|item.delta|item.completed`）
4. 中断/转向操作仅作用于活动 turn
5. 压缩（自动/手动）以 `context_compaction` 条目生命周期发出
6. 清除（agent 驱动）以 `context_purge` 条目生命周期发出
7. 客户端通过 `/v1/threads/{id}/events?since_seq=<n>` 重放历史并恢复

### 持久 Schema 门控

- `session_manager.rs`、`runtime_threads.rs` 和 `task_manager.rs` 在持久化记录中嵌入 `schema_version`。
- 加载时，较新的 schema 版本会被显式错误拒绝，而不是静默截断/覆盖数据。
- 这允许安全的前向迁移，并在二进制文件与已存储状态不同步时防止损坏。

## 扩展点

所有可插拔能力都经 `Extension` 契约落在四层之一（用户 Rhai mod、内建
Rhai 层、编译内置 Rust 扩展、安装的 Rust dylib）——哪个表面适配哪一层、
哪些按契约留在核心，逐表面地图见 [MODS_cn.md](MODS_cn.md)（"该用哪
一层"）。Rust 侧的编写指南是 [EXTENSIONS_cn.md](EXTENSIONS_cn.md)。

### 添加新工具

1. 在 `tools/` 中创建处理器
2. 在 `tools/registry.rs` 中注册
3. 添加工具规范（名称、描述、输入 schema）

### 添加 MCP 服务器

1. 在 `~/.codesmith/mcp.json` 中配置
2. 服务器在启动时自动发现
3. 工具自动暴露给 LLM

### 创建技能

1. 创建包含 `SKILL.md` 的技能目录
2. 定义技能提示词和可选脚本
3. 放入 `~/.codesmith/skills/`

### 添加钩子

在 `~/.codesmith/config.toml` 中配置（完整 schema 见 `docs/HOOKS.md`）：

```toml
[hooks]

[[hooks.hooks]]
event = "tool_call_before"
command = "echo 'Running tool: $TOOL_NAME'"
```

## 关键设计决策

1. **流式优先**：所有 LLM 响应以流式传输以保证响应速度
2. **工具安全**：非 YOLO 模式要求对破坏性操作进行审批，包括有副作用的 MCP 工具
3. **可扩展性**：Rhai mod、Rust 扩展（编译内置或 dylib）、MCP、技能和钩子允许在不修改代码的情况下进行定制——插件分层地图见 [MODS_cn.md](MODS_cn.md)
4. **跨平台**：核心在 Linux/macOS/Windows 上工作。操作系统级沙箱在
   macOS（Seatbelt）强制执行，在 Linux 上经 bubblewrap（已安装时）或
   Landlock（仅文件系统）强制执行；不存在 seccomp 过滤器，Windows 不
   提供沙箱——无法强制执行的要求沙箱策略在默认失败关闭姿态下被拒绝
   （`SANDBOX_UNAVAILABLE`）（平台矩阵见 `docs/SANDBOX.md`）。
5. **最小依赖**：精心选择依赖以保证构建速度
6. **本地优先的运行时 API**：HTTP/SSE 端点面向受信任的 localhost 访问，目前由 `crates/cli` 运行时提供
7. **三区 prompt 契约**：每个请求划分为 `PinnedPrefix`（系统提示词 + 工具目录，每步由 `prompt_zones::PinnedPrefix::freeze` 指纹化、由 `prefix_cache::PrefixStabilityManager` 漂移检查）、`AppendLog`（`Session` 转录存储——`push` 是该类型唯一日常可表达的变更；整体替换必须通过 `RebuildReason` 自我声明并落入日志的审计记录，以 `Event::TranscriptRebuilt` 浮出）与 `TurnScratch`（每轮的组合暂存区，在请求循环前提交进日志、在每个轮次边界清空）。每步的 `MessageRequest` 经 `engine/host_executor.rs` 中的 `ThreeZoneRequest` 组装——`messages` 只能是日志切片加暂存尾部，因此在别处用任意 `Vec<Message>` 手工拼装请求正是该契约要杜绝的模式（与旧式直接构造逐字节一致；见 `three_zone_assembly_sends_verbatim_log_snapshot` 回归测试）

## 配置文件

- `~/.codesmith/config.toml` - 主配置（`~/.codesmith/config.toml` 仍作为旧版回退被读取）
- `/etc/codesmith/managed_config.toml` - 可选的托管默认值层（Unix）
- `/etc/codesmith/requirements.toml` - 可选的允许策略约束（Unix）
- `~/.codesmith/mcp.json` - MCP 服务器配置
- `~/.codesmith/skills/` - 用户技能目录
- `~/.codesmith/sessions/` - 会话历史
- `~/.codesmith/sessions/checkpoints/` - 崩溃检查点 + 离线队列持久化
- `~/.codesmith/snapshots/` - 用于 `/restore` 和 `revert_turn` 的 turn 前/后 side-git 工作区快照
- `~/.codesmith/tasks/` - 后台任务记录、队列、时间线、工件
- `~/.codesmith/audit.log` - 用于凭据 + 审批/提权操作的只追加审计事件
