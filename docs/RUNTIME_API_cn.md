# 运行时 API 与集成契约

codesmith 通过 `codesmith serve --http` 暴露本地运行时 API，并通过
`codesmith doctor --json` 提供机器可读的健康状态。它还提供
`codesmith serve --acp`，供通过 stdio 使用 Agent Client Protocol 的
编辑器客户端使用。本文档是原生 macOS 工作台应用（以及其他本地监督者）
在不抓取终端屏幕输出的情况下嵌入 CodeSmith
引擎的稳定集成契约。

## 架构

```
macOS workbench (or any local supervisor)
        │
        ├─ codesmith doctor --json   → machine-readable health & capability
        ├─ codesmith serve --http    → HTTP/SSE runtime API
        ├─ codesmith serve --acp     → ACP stdio agent for editors such as Zed
        ├─ codesmith serve --mcp     → MCP stdio server
        └─ codesmith [args]          → interactive TUI session
```

引擎作为仅本地进程运行。所有 API 默认绑定到 `localhost`。
没有托管中继，不保管提供商令牌，不泄露机密。

## ACP stdio 适配器：`codesmith serve --acp`

`codesmith serve --acp` 通过换行分隔的 stdio 使用 JSON-RPC 2.0，
服务于 ACP 兼容的编辑器客户端。初始适配器实现了 ACP 基线：

- `initialize`
- `session/new`
- `session/prompt`
- `session/cancel`

提示请求通过已配置的 LLM 客户端和当前默认模型进行路由。
响应以 `session/update` agent 消息分块的形式发出，
随后是带有 `stopReason: "end_turn"` 的 `session/prompt` 响应。

该适配器刻意保持保守：它尚未通过 ACP 暴露 shell 工具、
文件写入工具、检查点重放或会话加载。需要完整的本地运行时 API 时
请使用 `codesmith serve --http`，当其他客户端需要将 CodeSmith 的工具
作为 MCP 工具使用时请使用 `codesmith serve --mcp`。

## 能力端点：`codesmith doctor --json`

返回一个描述当前安装就绪状态的 JSON 对象。
适合 macOS 工作台的健康检查轮询。

```bash
codesmith doctor --json
```

### 响应 schema（关键字段）

| 字段 | 类型 | 描述 |
|---|---|---|
| `version` | string | 已安装版本（例如 `"0.8.9"`） |
| `config_path` | string | 已解析的配置文件路径 |
| `config_present` | bool | 配置文件是否存在 |
| `workspace` | string | 默认工作区目录 |
| `api_key.source` | string | `env`、`config` 或 `missing` |
| `base_url` | string | API base URL |
| `default_text_model` | string | 默认模型 |
| `memory.enabled` | bool | 记忆功能是否开启 |
| `memory.path` | string | 记忆文件路径 |
| `memory.file_present` | bool | 记忆文件是否存在 |
| `mcp.config_path` | string | MCP 配置文件路径 |
| `mcp.present` | bool | MCP 配置是否存在 |
| `mcp.servers` | array | 每个服务器的健康状态：`{name, enabled, status, detail}` |
| `skills.selected` | string | 已解析的技能目录 |
| `skills.global.path` / `.present` / `.count` | — | CodeSmith 全局技能目录（`~/.codesmith/skills`，支持旧版 `~/.codesmith/skills`） |
| `skills.agents.path` / `.present` / `.count` | — | 工作区 `.agents/skills/` 目录 |
| `skills.agents_global.path` / `.present` / `.count` | — | agentskills.io 全局技能目录（`~/.agents/skills`） |
| `skills.local.path` / `.present` / `.count` | — | `skills/` 目录 |
| `skills.opencode.path` / `.present` / `.count` | — | `.opencode/skills/` 目录 |
| `skills.claude.path` / `.present` / `.count` | — | `.claude/skills/` 目录 |
| `tools.path` / `.present` / `.count` | — | 全局工具目录 |
| `plugins.path` / `.present` / `.count` | — | 全局插件目录 |
| `sandbox.available` | bool | 该操作系统是否支持沙箱 |
| `sandbox.kind` | string 或 null | 沙箱类型（例如 `"macos_seatbelt"`） |
| `storage.spillover.path` / `.present` / `.count` | — | 工具输出溢出目录 |
| `storage.stash.path` / `.present` / `.count` | — | Composer 暂存 |

### 示例

```json
{
  "version": "0.8.9",
  "config_path": "/Users/you/.codesmith/config.toml",
  "config_present": true,
  "workspace": "/Users/you/projects/codesmith-tui",
  "api_key": {
    "source": "env"
  },
  "base_url": "https://api.deepseek.com/beta",
  "default_text_model": "deepseek-v4-pro",
  "memory": {
    "enabled": false,
    "path": "/Users/you/.codesmith/memory.md",
    "file_present": true
  },
  "mcp": {
    "config_path": "/Users/you/.codesmith/mcp.json",
    "present": true,
    "servers": [
      {"name": "filesystem", "enabled": true, "status": "ok", "detail": "ready"}
    ]
  },
  "sandbox": {
    "available": true,
    "kind": "macos_seatbelt"
  }
}
```

## HTTP/SSE 运行时 API：`codesmith serve --http`

```bash
codesmith serve --http [--host 127.0.0.1] [--port 7878] [--workers 2] [--auth-token TOKEN]
codesmith serve --mobile [--host 0.0.0.0] [--port 7878] [--auth-token TOKEN]
```

默认值：主机 `127.0.0.1`，端口 `7878`，2 个工作线程（钳制在 1–8）。

服务器默认绑定到 `localhost`。配置通过 CLI 标志完成 ——
没有 `[app_server]` 配置节。

除非显式设置 `--insecure`，`/v1/*` 路由需要 bearer 令牌。
传入 `--auth-token TOKEN`，或在启动服务器之前设置
`CODESMITH_RUNTIME_TOKEN=TOKEN`。如果两者都未设置，进程会生成
一次性令牌并在启动时打印。`/health` 和 `/v1/runtime/info` 保持公开，
用于本地监督和引导。当移动模式被禁用时 `/mobile` 返回 404；
当移动模式启用且认证启用时，若请求未提供运行时令牌，
`/mobile` 返回 401。

已认证的客户端可以通过 `Authorization: Bearer TOKEN`、
`X-CodeSmith-Runtime-Token: TOKEN` 或 `?token=TOKEN` 提供令牌，
最后一种方式适用于无法设置自定义标头的 EventSource 风格
客户端。

### 移动控制页

`codesmith serve --mobile` 启动相同的 HTTP/SSE 运行时 API，并在
`/mobile` 提供适合手机的控制页。当绑定主机保持
默认值时，移动模式绑定到 `0.0.0.0`，打印一条警告，并打印本地/局域网
URL。传入 `--host 127.0.0.1` 可将移动页限制为仅环回访问。如果
生成或提供了运行时令牌，打印的移动 URL 会将其作为
查询参数包含在内；页面会在本地存储该令牌并将其从地址栏中
移除。静态 HTML 页面不含任何机密，但在启用认证时
它仍受令牌门控保护，因此未认证的局域网客户端无法对移动界面
进行指纹识别。

移动页可以列出/创建线程、发送提示词、跟随实时 SSE 事件、
对活动 turn 进行转向或中断，以及通过
`POST /v1/approvals/{approval_id}` 处理常规工具审批。它仍然是一个
本地/局域网的便利界面：
在没有 TLS 和受信任前端层的情况下，不要将其直接
暴露到公共互联网。

### 端点

**健康**
- `GET /health`

**会话**（旧版会话管理器）
- `GET /v1/sessions?limit=50&search=<substring>`
- `GET /v1/sessions/{id}`
- `DELETE /v1/sessions/{id}`
- `POST /v1/sessions/{id}/resume-thread`

**线程**（持久运行时数据模型）
- `GET /v1/threads?limit=50&include_archived=false&archived_only=false`
- `GET /v1/threads/summary?limit=50&search=<optional>&include_archived=false&archived_only=false`
- `POST /v1/threads`
- `GET /v1/threads/{id}`
- `PATCH /v1/threads/{id}`（请求体形状见下文）
- `POST /v1/threads/{id}/resume`
- `POST /v1/threads/{id}/fork`

线程分叉是同级的运行时线程，而不是原地树投影。
`thread.forked` 事件包含 `source_thread_id`；内部感知回溯的
分叉还可能包含 `backtrack_depth_from_tail` 和 `dropped_turn_id`。
在 v0.8.40 中，线程列表和摘要响应仍保持扁平结构，因此需要
图结构的客户端应从事件重建它，而不是假设列表顺序是
一棵完整的树。

`archived_only=true` 仅返回已归档线程（与
`include_archived` 互斥覆盖）。默认行为不变：`include_archived=false`
且 `archived_only=false` 返回活动线程。在 v0.8.10（#563）中添加。

`PATCH /v1/threads/{id}` 请求体 —— 每个字段都是可选的，缺失表示
"无变更"。至少必须存在一个字段。`title` 和 `system_prompt`
接受空字符串以清除先前设置的值。在 v0.8.10（#562）中添加：

```json
{
  "archived": true,
  "allow_shell": false,
  "trust_mode": false,
  "auto_approve": false,
  "model": "deepseek-v4-pro",
  "mode": "agent",
  "title": "User-set thread title",
  "system_prompt": "You are a useful assistant."
}
```

**Turn**（线程内）
- `POST /v1/threads/{id}/turns`
- `POST /v1/threads/{id}/turns/{turn_id}/steer`
- `POST /v1/threads/{id}/turns/{turn_id}/interrupt`
- `POST /v1/threads/{id}/compact`（手动压缩）

**审批**
- `POST /v1/approvals/{approval_id}`，请求体为
  `{ "decision": "allow" | "deny", "remember": false }`

**事件**（SSE 重放 + 实时流）
- `GET /v1/threads/{id}/events?since_seq=<u64>`

**兼容流**（一次性、向后兼容）
- `POST /v1/stream`

**任务**（持久后台作业）
- `GET /v1/tasks`
- `POST /v1/tasks`
- `GET /v1/tasks/{id}`
- `POST /v1/tasks/{id}/cancel`

**自动化**（定时周期作业）
- `GET /v1/automations`
- `POST /v1/automations`
- `GET /v1/automations/{id}`
- `PATCH /v1/automations/{id}`
- `DELETE /v1/automations/{id}`
- `POST /v1/automations/{id}/run`
- `POST /v1/automations/{id}/pause`
- `POST /v1/automations/{id}/resume`
- `GET /v1/automations/{id}/runs?limit=20`

**内省**
- `GET /v1/workspace/status`
- `GET /v1/skills`
- `GET /v1/apps/mcp/servers`
- `GET /v1/apps/mcp/tools?server=<optional>`

**用量**（跨线程的令牌/成本聚合）
- `GET /v1/usage?since=<rfc3339>&until=<rfc3339>&group_by=<day|model|provider|thread>`

`since` / `until` 为闭区间 RFC 3339 时间戳，可以省略（无
边界）。`group_by` 默认为 `day`。桶按键升序排序。
空时间范围产生空的 `buckets`（绝不返回 404）。成本通过
模型→定价映射计算；模型没有定价条目的 turn 贡献
令牌但成本为 `0.0`。在 v0.8.10（#564）中添加。

```json
{
  "since": "2026-04-01T00:00:00Z",
  "until": "2026-04-30T23:59:59Z",
  "group_by": "day",
  "totals": {
    "input_tokens": 12345,
    "output_tokens": 6789,
    "cached_tokens": 0,
    "reasoning_tokens": 0,
    "cost_usd": 0.012,
    "turns": 42
  },
  "buckets": [
    {
      "key": "2026-04-30",
      "input_tokens": 1234,
      "output_tokens": 678,
      "cached_tokens": 0,
      "reasoning_tokens": 0,
      "cost_usd": 0.001,
      "turns": 3
    }
  ]
}
```

## 运行时数据模型

运行时使用持久的 Thread/Turn/Item 生命周期。

- **ThreadRecord** —— `id`、`created_at`、`updated_at`、`model`、`workspace`、
  `mode`、`task_id`、`coherence_state`、`system_prompt`、`latest_turn_id`、
  `latest_response_bookmark`、`archived`
- **TurnRecord** —— `id`、`thread_id`、`status`（`queued|in_progress|completed|
  failed|interrupted|canceled`）、时间戳、时长、用量、错误摘要
- **TurnItemRecord** —— `id`、`turn_id`、`kind`（`user_message|agent_message|
  tool_call|file_change|command_execution|context_compaction|status|error`）、
  生命周期 `status`、`metadata`

事件为只追加，带有全局单调 `seq` 用于重放/恢复。

### 重启语义

- 如果进程在某个 turn 或条目处于 `queued` 或 `in_progress` 时重启，
  恢复的记录会被标记为 `interrupted`，并带有 `"Interrupted by
  process restart"` 错误。
- 任务执行在同一持久化线程/turn 存储之上执行自己的恢复。

### 审批模型

- `auto_approve` 标志应用于运行时审批桥和引擎
  工具上下文。当为线程/turn/任务启用时，需要审批的工具
  在非交互式运行时路径中被自动批准，shell 安全检查
  以自动批准模式运行，并且派生的子代理继承该设置。
- 省略时，`auto_approve` 默认为 `false`。

### SSE 事件流

`/v1/threads/{id}/events` 的 SSE 事件载荷形状：

```json
{
  "schema_version": 1,
  "seq": 42,
  "event": "item.delta",
  "kind": "item.delta",
  "thread_id": "thr_1234abcd",
  "turn_id": "turn_5678efgh",
  "item_id": "item_90ab12cd",
  "timestamp": "2026-02-11T20:18:49.123Z",
  "created_at": "2026-02-11T20:18:49.123Z",
  "payload": {
    "delta": "partial output",
    "kind": "agent_message"
  }
}
```

兼容性说明：

- `schema_version` 是 HTTP/SSE 信封 schema 版本。它独立于
  用于持久化线程/turn/事件记录的运行时存储 schema。
- `event` 在既有客户端中仍是 SSE 事件名；它被原样保留。
- `kind` 在稳定信封中镜像 `event`，供类型化客户端使用。
- `thread.started`、`turn.started` 和 `turn.completed` 作为 SSE 事件
  名完全和以前一样发出。
- `timestamp` 仍是 schema 版本 1 的规范事件时间。`created_at`
  是为在其他地方使用 `created_at` 命名的客户端提供的等价别名；不要
  要求两个字段同时存在。

常见事件名：`thread.started`、`thread.forked`、`turn.started`、
`turn.lifecycle`、`turn.steered`、`turn.interrupt_requested`、
`turn.completed`、`item.started`、`item.delta`、`item.completed`、
`item.failed`、`item.interrupted`、`approval.required`、`approval.decided`、
`approval.timeout`、`sandbox.denied`、`coherence.state`。

## 安全边界

- **默认 localhost**。服务器默认绑定到 `127.0.0.1`。
  在未提供主机时 `--mobile` 绑定到 `0.0.0.0`，以便同一
  局域网上的手机可以访问它，并且 CLI 会为该重新绑定打印一条警告。
  传入 `--host 127.0.0.1` 可获得仅环回的移动页。仅当你信任
  网络路径或拥有经过认证的反向代理 / VPN 时才设置非环回主机。
  运行时不提供用户隔离或 TLS。
- **可选的令牌守卫**。`--auth-token` 或 `CODESMITH_RUNTIME_TOKEN`
  要求 `/v1/*` 路由提供匹配的 bearer 令牌。这是一个本地
  便利性守卫，不能替代公共网络上的 TLS、VPN 或受信任的
  反向代理。
- **不保管提供商令牌**。服务器永远不会返回 API 密钥。
  `api_key.source` 能力字段报告 `env`、`config` 或 `missing` ——
  绝不报告密钥本身。
- **没有托管中继**。app-server 是处于用户控制之下的本地
  进程。没有任何云组件。
- **能力响应**永远不会泄露机密、文件内容或会话
  消息体。它们报告的是*元数据*：存在性、计数、状态标志。

### CORS 允许列表

运行时 API 附带内置的开发来源允许列表：
`http://localhost:3000`、`http://127.0.0.1:3000`、`http://localhost:1420`、
`http://127.0.0.1:1420`、`tauri://localhost`。要添加额外的来源（例如
在 Vite 默认的 `:5173` 上开发 UI 时），可使用以下任一方式：

- CLI 标志（可重复）：`codesmith serve --http --cors-origin http://localhost:5173`
- 环境变量（逗号分隔）：`CODESMITH_CORS_ORIGINS="http://localhost:5173,http://localhost:8080"`
- 配置（`~/.codesmith/config.toml`）：
  ```toml
  [runtime_api]
  cors_origins = ["http://localhost:5173"]
  ```

用户提供的来源**叠加在**内置默认值之上；它们不会
替换内置默认值。不支持通配符来源 —— 显式允许列表
模型被保留。在 v0.8.10（#561）中添加。

## 会话生命周期（原生 UI 监督）

| 操作 | 端点 |
|---|---|
| 列出会话 | `GET /v1/sessions` |
| 获取会话 | `GET /v1/sessions/{id}` |
| 删除会话 | `DELETE /v1/sessions/{id}` |
| 恢复为线程 | `POST /v1/sessions/{id}/resume-thread` |
| 创建线程 | `POST /v1/threads` |
| 列出线程 | `GET /v1/threads` |
| 附着到事件 | `GET /v1/threads/{id}/events?since_seq=0` |
| 发送消息 | `POST /v1/threads/{id}/turns` |
| 转向 | `POST /v1/threads/{id}/turns/{turn_id}/steer` |
| 中断 | `POST /v1/threads/{id}/turns/{turn_id}/interrupt` |
| 压缩 | `POST /v1/threads/{id}/compact` |

## 兼容性测试

契约快照位于 `crates/protocol/tests/`。运行：

```bash
cargo test -p codesmith-protocol --test parity_protocol --locked
```

这会验证 app-server 的事件 schema 尚未偏离
已文档化的契约。CI 在每次推送到 `main` 以及发布标签时运行它。
