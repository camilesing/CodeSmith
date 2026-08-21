# MCP（外部工具服务器）

codesmith 可以通过 MCP（Model Context Protocol，模型上下文协议）加载额外工具。MCP 服务器既可以是由 TUI 启动的本地 stdio 子进程，也可以是通过 Streamable HTTP（含旧版 SSE 回落）、SSE 或 WebSocket 访问的远程服务器。

浏览工具说明：
- `web.run` 是内置的标准浏览工具。
- `web_search` 仍作为兼容别名保留，供旧提示词和集成使用。

服务器模式说明：
- `codesmith-tui serve --mcp` 运行 MCP stdio 服务器。
- `codesmith-tui serve --http` 运行运行时 HTTP/SSE API（独立模式）。
- `codesmith` 调度器提供 `codesmith mcp-server`，作为拆分式 CLI 使用的等价
  stdio 入口。

## 引导 MCP 配置

在解析出的 MCP 路径创建一个入门 MCP 配置：

```bash
codesmith-tui mcp init
```

`codesmith-tui setup --mcp` 在完成 skills 设置的同时执行相同的 MCP 引导。

常用管理命令：

```bash
codesmith-tui mcp list
codesmith-tui mcp tools [server]
codesmith-tui mcp add <name> --command "<cmd>" --arg "<arg>"
codesmith-tui mcp add <name> --url "http://localhost:3000/mcp"
codesmith-tui mcp enable <name>
codesmith-tui mcp disable <name>
codesmith-tui mcp remove <name>
codesmith-tui mcp validate
```

## TUI 内管理器

在交互式 TUI 内，`/mcp` 会为解析出的 MCP 配置路径打开一个紧凑的管理器。它显示每个已配置的服务器、其启用或禁用状态、传输方式、命令或 URL、超时值、连接错误，以及发现流程运行后得到的工具/资源/提示词。

支持的 TUI 内操作：

```text
/mcp init
/mcp init --force
/mcp add stdio <name> <command> [args...]
/mcp add http <name> <url>
/mcp enable <name>
/mcp disable <name>
/mcp remove <name>
/mcp validate
/mcp reload
```

`/mcp validate` 和 `/mcp reload` 会重新连接以进行 UI 发现，并刷新管理器快照。在 TUI 中所做的配置编辑会立即写入，但模型可见的 MCP 工具池不会热重载；在 TUI 重启之前，管理器会将其标记为需要重启。

## 配置文件位置

默认路径：

- `~/.codesmith/mcp.json`（当 CodeSmith 文件不存在时，仍会读取 `~/.deepseek/mcp.json`）

覆盖方式：

- 配置：`mcp_config_path = "/path/to/mcp.json"`
- 环境变量：`CODESMITH_MCP_CONFIG=/path/to/mcp.json`（仍接受旧别名 `CODEWHALE_MCP_CONFIG` 和 `DEEPSEEK_MCP_CONFIG`）

`codesmith-tui mcp init`（以及 `codesmith-tui setup --mcp`）会写入这个解析后的路径。

交互式 `/config` 编辑器也提供 `mcp_config_path`。在 TUI 中修改它会更新 `/mcp` 使用的路径，但模型可见的 MCP 工具池需要重启后才会重建。

编辑该文件或更改 `mcp_config_path` 之后，请重启 TUI。

## 工具命名

发现的 MCP 工具以如下形式暴露给模型：

- `mcp__<server>__<tool>`

示例：名为 `git` 的服务器上名为 `status` 的工具会变成 `mcp__git__status`。旧的单下划线形式（`mcp_<server>_<tool>`）仍作为调用时别名被接受以保持向后兼容，但模型在工具列表中看到的是双下划线形式。

命令面板包含按服务器分组的 MCP 条目。它会显示已禁用和失败的服务器而不是隐藏它们，并使用与模型所见相同的运行时工具名。

## 资源与提示词辅助工具

当 MCP 启用时，CLI 还会暴露以下辅助工具：

- `list_mcp_resources`（可选 `server` 过滤器）
- `list_mcp_resource_templates`（可选 `server` 过滤器）
- `mcp_read_resource` / `read_mcp_resource`（别名）
- `mcp_get_prompt`

## 最小示例

```json
{
  "timeouts": {
    "connect_timeout": 10,
    "execute_timeout": 60,
    "read_timeout": 120
  },
  "servers": {
    "example": {
      "command": "node",
      "args": ["./path/to/your-mcp-server.js"],
      "env": {},
      "disabled": false
    }
  }
}
```

为了与其他客户端兼容，你也可以用 `mcpServers` 代替 `servers`。

## 将 CodeSmith 作为 MCP 服务器运行

你可以把本地的 CodeSmith 二进制注册为 MCP 服务器，让其他 CodeSmith 会话（或任何 MCP 客户端）调用它的工具。

### 快速设置

```bash
codesmith-tui mcp add-self
```

该命令会解析当前二进制路径，生成一个运行 `codesmith-tui serve --mcp` 的配置条目，并写入你的 MCP 配置文件。默认服务器名为 `codesmith`。

选项：

- `--name <NAME>` —— 自定义服务器名（默认：`codesmith`）
- `--workspace <PATH>` —— 服务器的工作区目录

### 手动配置

在 `~/.codesmith/mcp.json` 中等价的手动条目：

```json
{
  "servers": {
    "codesmith": {
      "command": "/path/to/codesmith",
      "args": ["serve", "--mcp"],
      "env": {}
    }
  }
}
```

`codesmith-tui` 二进制直接支持 `serve --mcp`。`codesmith` 调度器提供等价的 `codesmith mcp-server` stdio 入口。使用位于你 `PATH` 中的那一个（运行 `which codesmith` 或 `which codesmith-tui` 查看完整路径）。`mcp add-self` 命令会自动解析正确的二进制。

### 前置条件

- `command` 引用的二进制必须存在且可执行。
- MCP 服务器作为 stdio 子进程运行——不需要网络端口。
- 每个 MCP 客户端会话都会生成自己的服务器进程。

### 工具命名

来自自托管 CodeSmith 服务器的工具遵循标准命名约定：

- `mcp__<server>__<tool>` —— 使用默认服务器名 `codesmith` 时，形式为 `mcp__codesmith__<tool>`

例如，`shell` 工具会变成 `mcp__codesmith__shell`。

### MCP 服务器 vs HTTP/SSE API vs ACP

| | `codesmith-tui serve --mcp` | `codesmith-tui serve --http` | `codesmith-tui serve --acp` |
|---|---|---|---|
| **协议** | MCP stdio | HTTP/SSE JSON-RPC | ACP stdio |
| **用例** | 面向 MCP 客户端的工具服务器 | 面向应用的运行时 API | 面向 Zed/自定义 ACP 客户端的编辑器智能体 |
| **配置** | `~/.codesmith/mcp.json` 条目 | 直接 URL 连接 | 编辑器 `agent_servers` 自定义命令 |
| **生命周期** | 每个客户端会话各生成一个 | 长期运行的守护进程 | 每个编辑器智能体会话各生成一个 |

想让 CodeSmith 工具对其他 MCP 客户端可用时，使用 `mcp add-self`。
构建直接调用该 API 的应用时，使用 `serve --http`。
编辑器希望以 ACP 智能体方式与 CodeSmith 通信时，使用 `serve --acp`。

### 验证

添加之后，测试连接：

```bash
codesmith-tui mcp validate
codesmith-tui mcp tools codesmith
```

## 服务器字段

逐服务器设置：

- `command`（字符串，stdio 服务器必填）：要启动的可执行文件。远程服务器改用 `url`。
- `args`（字符串数组，可选）
- `env`（对象，可选）
- `url`（字符串，可选）：远程 MCP 服务器的基础 URL。基于 URL 的服务器默认使用 Streamable HTTP，并在服务器拒绝 Streamable HTTP 时回落到旧版 SSE。
- `transport`（字符串，可选）：对 `url` 服务器的显式传输覆盖。支持的值：`http` / `streamable` / `streamable-http`（默认）、`sse`、`sse-ide`、`ws` / `websocket`、`ws-ide` / `websocket-ide`。对必须从端点发现开始的旧版 SSE 端点使用 `sse` 或 `sse-ide`，对 WebSocket MCP 端点使用 `ws` / `ws-ide`。
- `headers`（对象，可选）：随发往该服务器的每个请求附带的额外 HTTP 头（例如 `Authorization: Bearer ...`）。只有 HTTP 传输会使用它；stdio 服务器会忽略它。头键名和值按原样传递（不做环境变量替换），并以明文存储在 `mcp.json` 中——请像对待任何其他含密钥的配置一样谨慎对待该文件。
- `connect_timeout`、`execute_timeout`、`read_timeout`（秒，可选）
- `disabled`（布尔值，可选）
- `enabled`（布尔值，可选，默认 `true`）
- `required`（布尔值，可选）：若该服务器无法初始化，则启动/连接校验失败。
- `enabled_tools`（数组，可选）：该服务器的工具名允许列表。
- `disabled_tools`（数组，可选）：在 `enabled_tools` 之后应用的拒绝列表。

## 特性开关

MCP 支持由 `mcp` 特性开关控制，默认启用（实验性）。要完全关闭 MCP，请在 `config.toml` 中设置：

```toml
[features]
mcp = false
```

## 安全说明

MCP 工具现在与内置工具走同一套工具审批框架。只读 MCP 助手（资源/提示词的列出与读取）在建议型审批模式下可以免提示运行，而带副作用的 MCP 工具需要审批。

你仍然应当只配置自己信任的 MCP 服务器，并把 MCP 服务器配置等同于在你机器上运行代码来对待。

## 故障排查

- 运行 `codesmith-tui doctor`，确认它解析出的 MCP 配置路径以及该文件是否存在。
- 在 TUI 中运行 `/mcp validate`，刷新可见的服务器/工具快照。
- 如果 MCP 配置缺失，运行 `codesmith-tui mcp init --force` 重新生成。
- 如果工具没有出现，请验证服务器命令能否在你的 shell 中运行，以及服务器是否支持 MCP `tools/list`。
