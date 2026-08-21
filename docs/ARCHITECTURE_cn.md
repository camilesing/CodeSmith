# codesmith 架构

本文档为开发者和贡献者提供 codesmith 架构的概览。

当前边界说明（v0.8.6）：
- `crates/tui` 仍然是 TUI、运行时 API、任务管理器和工具执行循环的活跃终端用户运行时。
- 其他工作区 crate 正在逐步拆分，但它们尚不是唯一的运行时事实来源。
- 启动信任边界的细节记录在 `docs/STARTUP_TRUST_BOUNDARY_AUDIT.md` 中；该审计是信任前与信任后初始化跟进事项的当前参考。
- LSP 子系统（`crates/tui/src/lsp/`）已完全接入引擎的工具执行后路径
  （`core/engine/lsp_hooks.rs`），在每次 edit_file/apply_patch/write_file 之后提供内联诊断。
- swarm agent 系统已在 v0.8.5 中移除。当前活跃的 v0.8.35 编排面是持久子代理会话（`agent_open` / `agent_eval` / `agent_close`）和持久 RLM 会话（`rlm_open` / `rlm_eval` / `rlm_configure` / `rlm_close`）。
  活跃代码库中不再保留任何模型可见的 swarm 工具。

## 高层概览

```
┌─────────────────────────────────────────────────────────────────┐
│                         User Interface                          │
│  ┌─────────────────┐  ┌─────────────────┐  ┌────────────────┐  │
│  │   TUI (ratatui) │  │  One-shot Mode  │  │  Config/CLI    │  │
│  └────────┬────────┘  └────────┬────────┘  └────────┬───────┘  │
└───────────┼─────────────────────┼────────────────────┼──────────┘
            │                     │                    │
            ▼                     ▼                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                        Core Engine                              │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │                    Agent Loop (core/engine.rs)           │   │
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
│  │              LLM Client Abstraction (llm_client.rs)       │  │
│  │  ┌─────────────────┐  ┌─────────────────────────────┐    │  │
│  │  │  DeepSeek Client │  │  Compatible Client (DeepSeek)│    │  │
│  │  │   (client.rs)   │  │       (client.rs)           │    │  │
│  │  └─────────────────┘  └─────────────────────────────┘    │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## 模块组织

### 入口

- **`main.rs`** - CLI 参数解析（clap）、配置加载、入口路由

### 核心组件

- **`core/`** - 主引擎组件
  - `engine.rs` - 引擎状态、操作处理、消息处理
  - `engine/turn_loop.rs` - 流式 turn 循环和工具执行编排
  - `engine/capacity_flow.rs` - 容量护栏检查点和干预
  - `session.rs` - 会话状态管理
  - `turn.rs` - 基于 turn 的会话处理
  - `events.rs` - 用于 UI 更新的事件系统
  - `ops.rs` - 核心操作

### 配置

- **`config.rs`** - 配置加载、profile、环境变量
- **`settings.rs`** - 运行时设置管理

### 工作区 Crate

- **`crates/tools`** - 共享工具调用原语，包括 TUI 运行时使用的工具结果/错误/能力类型。
- **`crates/agent`** - 模型/提供商注册表（ModelRegistry），用于将模型 ID 解析到提供商端点。
- **`crates/app-server`** - 用于无头（headless）agent 工作流的 HTTP/SSE + JSON-RPC 应用服务器传输层。
- **`crates/config`** - 配置加载、profile、环境变量优先级、CLI 运行时覆盖。
- **`crates/core`** - Agent 循环、会话管理、turn 编排、容量流护栏。
- **`crates/execpolicy`** - 用于工具执行决策的审批/沙箱策略引擎。
- **`crates/hooks`** - 用于工具事件前/后钩子的生命周期钩子（stdout、jsonl、webhook）。
- **`crates/mcp`** - 用于 Model Context Protocol 工具服务器的 MCP 客户端 + stdio 服务器。
- **`crates/protocol`** - 请求/响应组帧和协议类型。
- **`crates/secrets`** - 用于 API 密钥存储的操作系统密钥环集成。
- **`crates/state`** - SQLite 线程/会话持久化层。
- **`crates/tui-core`** - 事件驱动的 TUI 状态机脚手架。

### LLM 集成

- **`client.rs`** - 用于 DeepSeek 官方文档所载 OpenAI 兼容 Chat Completions API 的 HTTP 客户端
- **`llm_client.rs`** - 带重试逻辑的抽象 LLM 客户端 trait
- **`models.rs`** - API 请求/响应的数据结构

#### DeepSeek API 端点

DeepSeek 提供 OpenAI 兼容端点。CLI 使用：
- `https://api.deepseek.com/beta/chat/completions` - v0.8.16 起默认的 DeepSeek 模型 turn
- `https://api.deepseek.com/beta/models` - v0.8.16 起默认的实时模型发现和健康检查

`https://api.deepseek.com/v1` 被接受用于 OpenAI SDK 兼容，并且仍可被显式配置，
以退出仅限 beta 的功能，例如 strict tool mode、chat prefix completion 和 FIM completion。公开的
DeepSeek 文档并未为此工作流记录 Responses API 路径；引擎通过
Chat Completions 驱动 turn。

### 工具系统

- **`tools/`** - 内置工具实现
  - `mod.rs` - 工具注册表和通用类型
  - `shell.rs` - Shell 命令执行
  - `file.rs` - 文件读写操作
  - `todo.rs` - 清单工具以及旧版 todo 别名
  - `tasks.rs` - 模型可见的持久任务、gate、后台 shell 和 PR 尝试工具
  - `github.rs` - 只读 GitHub 上下文以及由 `gh` 支撑的受防护评论/关闭工具
  - `automation.rs` - 基于 `AutomationManager` 的模型可见调度工具
  - `plan.rs` - 规划工具
  - `subagent.rs` - 持久子代理会话（替代已移除的 `agent_swarm` 面）
  - `spec.rs` - 工具规范
  - `rlm.rs` - 持久递归语言模型（RLM）会话 —— 带语义辅助调用和 `var_handle` 输出支持的沙箱化 Python REPL

### 扩展系统

- **`mcp.rs`** - 用于外部工具服务器的 Model Context Protocol 客户端
- **`skills.rs`** - 插件/技能加载与执行
- **`hooks.rs`** - 带条件的执行前/后钩子

### 用户界面

- **`tui/`** - 终端 UI 组件（基于 ratatui）
  - `app.rs` - 应用状态和消息处理
  - `ui.rs` - 事件处理、流式状态和渲染逻辑
  - `approval.rs` - 工具审批对话框
  - `clipboard.rs` - 剪贴板处理
  - `streaming.rs` - 流式文本收集器

- **`ui.rs`** - 旧版/简单 UI 工具

### LSP 集成

- **`lsp/`** - 编辑后诊断注入（#136）
  - `mod.rs` - `LspManager` —— 按语言惰性初始化的传输池 + 配置
  - `client.rs` - `StdioLspTransport` —— 基于 stdio 的 JSON-RPC，支持 `didOpen`/`didChange`/`publishDiagnostics`
  - `diagnostics.rs` - 诊断类型、严重级别和 HTML 块渲染器
  - `registry.rs` - 语言检测和默认服务器映射（rust-analyzer、pyright、gopls、clangd、typescript-language-server、jdtls、vue-language-server）
  - 通过 `core/engine/lsp_hooks.rs` 接入引擎 —— 在每次成功编辑后调用

### 代码索引

- **`crates/index`**（`codesmith-index`）—— 按工作区持久化的代码索引（参见 `docs/INDEX.md`）
  - `types.rs` / `backend.rs` —— 值类型 + 三个接缝：`IndexBackendFactory`/`IndexBackend`（提供商注册表模式）、`IndexServiceApi`（LspManagerApi 风格注入）、保留的 `SemanticIndexApi`
  - `registry.rs` - `IndexBackendRegistry`，镜像 `ProviderRegistry`（upsert；构建错误列表会列出已注册的 id）
  - `tree_sitter.rs` - `tree-sitter` cargo feature 之后的内置符号后端（rust/python/js/ts/go；容器作用域、词法引用）
  - `walk.rs` - 基于 `ignore` 的工作区遍历（感知 `.gitignore`），为清单 + 新鲜度差异提供输入
  - `store.rs` - `~/.codesmith/index/<ws-hash>/` 下的按工作区 SQLite（schema 版本不匹配 → 重建）
  - `service.rs` - `IndexService` 编排：惰性增量刷新（mtime+size 差异、预算 + `stale_files` 报告）及后台补全
- 接线：tui 为每个工作区构建一次服务（`tui/index.rs`、`[index]` 配置）并注入 `RuntimeToolServices::index_service` → 每个 turn 的 `ToolContext`；`symbol_search` / `find_references` 位于 `codesmith-tool-impls` 中，注册通过 `EngineConfig::index_enabled` 按会话恒定门控（目录稳定性、KV 前缀缓存）

### 安全

- **`sandbox/`** - 平台沙箱策略准备和拒绝报告
  - `mod.rs` - 沙箱类型定义
  - `policy.rs` - 沙箱策略配置
  - `seatbelt.rs` - macOS Seatbelt profile 生成
  - `landlock.rs` - Linux Landlock 检测和未来的辅助程序契约
  - `windows.rs` - Windows 辅助程序契约；在存在 Job
    Object 进程遏制辅助程序之前不予宣告

### 工具库

- **`utils.rs`** - 通用工具
- **`logging.rs`** - 日志基础设施
- **`compaction.rs`** - 长对话的上下文压缩
- **`purge.rs`** - Agent 驱动的上下文清除（精准的消息移除/改写）
- **`pricing.rs`** - 成本估算
- **`prompts.rs`** - 系统提示词模板
- **`project_doc.rs`** - 项目文档处理
- **`session.rs`** - 会话序列化
- **`runtime_api.rs`** - HTTP/SSE 运行时 API（`codesmith serve --http`）
- **`runtime_threads.rs`** - 持久线程/turn/条目存储 + 可重放的事件时间线
- **`task_manager.rs`** - 持久队列、工作线程池、任务时间线和工件

## 数据流

### 交互式会话

1. 在 TUI 中接收用户输入
2. 输入由 `core/engine.rs` 处理
3. 消息通过 `llm_client.rs` 发送到 LLM
4. 响应流式返回，在 `client.rs` 中解析
5. 提取工具调用并通过 `tools/` 执行
6. 在工具执行前后触发钩子
7. 结果聚合后回传给 LLM
8. 最终响应在 TUI 中渲染

### 崩溃恢复 + 离线队列

1. 发送用户输入之前，TUI 会将检查点快照写入 `~/.codesmith/sessions/checkpoints/latest.json`
2. 启动默认保持全新状态；先前的会话通过 `--resume`/`--continue`（或 TUI 中的 `Ctrl+R`）显式恢复
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

### 后台任务

1. 客户端将任务入队（`/task add ...` 或 `POST /v1/tasks`）
2. `task_manager.rs` 将任务 + 队列条目持久化到 `~/.codesmith/tasks` 下
3. 工作线程领取已排队任务（有界线程池），并转为 `running`
4. 任务创建/使用一个运行时线程并启动一个运行时 turn
5. `runtime_threads.rs` 持久化线程/turn/条目记录 + 单调递增事件序列
6. 时间线/工具摘要/工件引用会增量持久化
7. 清单状态、验证器 gate、PR 尝试和受防护的 GitHub 事件从工具元数据应用到活动任务
8. 最终状态（`completed|failed|canceled`）是持久的，可通过 TUI/API 查询

模型可见的持久任务工具是同一管理器之上的一个表面。它们不会
引入并行的作业系统：`task_create` 入队普通任务，
`checklist_*` 更新任务内的进度，`task_gate_run` 和已完成的
`task_shell_wait` 附上验证证据，自动化运行则入队
普通持久任务。

### 运行时线程/Turn 时间线

1. API/TUI 创建或恢复线程（`/v1/threads*`）
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

在 `~/.codesmith/config.toml` 中配置：

```toml
[[hooks]]
event = "tool_call_before"
command = "echo 'Running tool: $TOOL_NAME'"
```

## 关键设计决策

1. **流式优先**：所有 LLM 响应以流式传输以保证响应速度
2. **工具安全**：非 YOLO 模式要求对破坏性操作进行审批，包括有副作用的 MCP 工具
3. **可扩展性**：MCP、技能和钩子允许在不修改代码的情况下进行定制
4. **跨平台**：核心在 Linux/macOS/Windows 上工作。沙箱保证
   是平台特定的：macOS Seatbelt 是活跃的策略路径；Linux 和
   Windows 需要辅助程序强制执行，然后才能被视为完整的 OS
   沙箱。
5. **最小依赖**：精心选择依赖以保证构建速度
6. **本地优先的运行时 API**：HTTP/SSE 端点面向受信任的 localhost 访问，目前由 `crates/tui` 运行时提供

## 配置文件

- `~/.codesmith/config.toml` - 主配置（`~/.deepseek/config.toml` 仍作为旧版回退被读取）
- `/etc/deepseek/managed_config.toml` - 可选的托管默认值层（Unix）
- `/etc/deepseek/requirements.toml` - 可选的允许策略约束（Unix）
- `~/.codesmith/mcp.json` - MCP 服务器配置
- `~/.codesmith/skills/` - 用户技能目录
- `~/.codesmith/sessions/` - 会话历史
- `~/.codesmith/sessions/checkpoints/` - 崩溃检查点 + 离线队列持久化
- `~/.codesmith/snapshots/` - 用于 `/restore` 和 `revert_turn` 的 turn 前/后 side-git 工作区快照
- `~/.codesmith/tasks/` - 后台任务记录、队列、时间线、工件
- `~/.codesmith/audit.log` - 用于凭据 + 审批/提权操作的只追加审计事件
