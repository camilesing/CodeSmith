# MCP（外部工具服务器）

codesmith 可以通过 MCP（Model Context Protocol，模型上下文协议）加载额外工具。MCP 服务器既可以是由交互 REPL 启动的本地 stdio 子进程，也可以是通过 Streamable HTTP（含旧版 SSE 回落）、SSE 或 WebSocket 访问的远程服务器。

浏览工具说明：
- `web.run` 是内置的标准浏览工具。
- `web_search` 仍作为兼容别名保留，供旧提示词和集成使用。

服务器模式说明：
- `codesmith serve --mcp` 运行 MCP stdio 服务器。
- `codesmith serve --http` 运行运行时 HTTP/SSE API（独立模式）。
- `codesmith mcp-server` 是等价的 stdio 入口。

## 引导 MCP 配置

在解析出的 MCP 路径创建一个入门 MCP 配置：

```bash
codesmith mcp init
```

`codesmith setup --mcp` 在完成 skills 设置的同时执行相同的 MCP 引导。

常用管理命令：

```bash
codesmith mcp list
codesmith mcp tools [server]
codesmith mcp add <name> --command "<cmd>" --arg "<arg>"
codesmith mcp add <name> --url "http://localhost:3000/mcp"
codesmith mcp enable <name>
codesmith mcp disable <name>
codesmith mcp remove <name>
codesmith mcp validate
```

## REPL 内管理器

在交互 REPL 内，`/mcp` 会为解析出的 MCP 配置路径打开一个紧凑的管理器。它显示每个已配置的服务器、其启用或禁用状态、传输方式、命令或 URL、超时值、连接错误，以及发现流程运行后得到的工具/资源/提示词。

支持的 REPL 内操作：

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

`/mcp validate` 和 `/mcp reload` 会重新连接以进行 UI 发现，并刷新管理器快照。在管理器中所做的配置编辑会立即写入，但模型可见的 MCP 工具池不会热重载；在 REPL 重启之前，管理器会将其标记为需要重启。

## 配置文件位置

默认路径：

- `~/.codesmith/mcp.json`

覆盖方式：

- 配置：`mcp_config_path = "/path/to/mcp.json"`
- 环境变量：`CODESMITH_MCP_CONFIG=/path/to/mcp.json`

`codesmith mcp init`（以及 `codesmith setup --mcp`）会写入这个解析后的路径。

交互式 `/config` 编辑器也提供 `mcp_config_path`。在 REPL 中修改它会更新 `/mcp` 使用的路径，但模型可见的 MCP 工具池需要重启后才会重建。

编辑该文件或更改 `mcp_config_path` 之后，请重启 REPL。

## 工具命名

发现的 MCP 工具以如下形式暴露给模型：

- `mcp__<server>__<tool>`

示例：名为 `git` 的服务器上名为 `status` 的工具会变成 `mcp__git__status`。旧的单下划线形式（`mcp_<server>_<tool>`）仍作为调用时别名被接受以保持向后兼容，但模型在工具列表中看到的是双下划线形式。

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
codesmith mcp add-self
```

该命令会解析当前二进制路径，生成一个运行 `codesmith serve --mcp` 的配置条目，并写入你的 MCP 配置文件。默认服务器名为 `codesmith`。

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

`codesmith` 二进制直接支持 `serve --mcp`；`codesmith mcp-server` 是等价的专用 stdio 入口。`mcp add-self` 命令会自动解析当前二进制路径。

### 前置条件

- `command` 引用的二进制必须存在且可执行。
- MCP 服务器作为 stdio 子进程运行——不需要网络端口。
- 每个 MCP 客户端会话都会生成自己的服务器进程。

### 工具命名

来自自托管 CodeSmith 服务器的工具遵循标准命名约定：

- `mcp__<server>__<tool>` —— 使用默认服务器名 `codesmith` 时，形式为 `mcp__codesmith__<tool>`

例如，`shell` 工具会变成 `mcp__codesmith__shell`。

### MCP 服务器 vs HTTP/SSE API vs ACP

| | `codesmith serve --mcp` | `codesmith serve --http` | `codesmith serve --acp` |
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
codesmith mcp validate
codesmith mcp tools codesmith
```

## 服务器字段

逐服务器设置：

- `command`（字符串，stdio 服务器必填）：要启动的可执行文件。远程服务器改用 `url`。
- `args`（字符串数组，可选）
- `env`（对象，可选）
- `url`（字符串，可选）：远程 MCP 服务器的基础 URL。基于 URL 的服务器默认使用 Streamable HTTP，并在服务器拒绝 Streamable HTTP 时回落到旧版 SSE。
- `transport`（字符串，可选）：对 `url` 服务器的显式传输覆盖。支持的值：`http` / `streamable` / `streamable-http`（默认）、`sse`、`sse-ide`、`ws` / `websocket`、`ws-ide` / `websocket-ide`。对必须从端点发现开始的旧版 SSE 端点使用 `sse` 或 `sse-ide`，对 WebSocket MCP 端点使用 `ws` / `ws-ide`。
- `headers`（对象，可选）：随发往该服务器的每个请求附带的额外 HTTP 头（例如 `Authorization: Bearer ...`）。只有 HTTP 传输会使用它；stdio 服务器会忽略它。头键名和值按原样传递（不做环境变量替换），并以明文存储在 `mcp.json` 中——请像对待任何其他含密钥的配置一样谨慎对待该文件。
- `connect_timeout`、`execute_timeout`、`read_timeout`（秒，可选）：各自约束什么见下文[远程传输（HTTP）](#远程传输http)。
- `disabled`（布尔值，可选）
- `enabled`（布尔值，可选，默认 `true`）
- `required`（布尔值，可选）：若该服务器无法初始化，则启动/连接校验失败。
- `enabled_tools`（数组，可选）：该服务器的工具名允许列表。
- `disabled_tools`（数组，可选）：在 `enabled_tools` 之后应用的拒绝列表。
- `oauth`（对象，可选）：URL 服务器的 OAuth 设备流配置；见下文 [OAuth](#oauth) 一节。

## 远程传输（HTTP）

带 `url` 的服务器条目默认通过 MCP Streamable HTTP 访问，除非 `transport`
另有指定。一个完整的远程条目：

```json
{
  "servers": {
    "docs": {
      "url": "https://mcp.example.com/mcp",
      "headers": { "Authorization": "Bearer <token>" },
      "connect_timeout": 10,
      "execute_timeout": 60,
      "read_timeout": 120
    }
  }
}
```

### Streamable HTTP

- 每条 JSON-RPC 消息是一次对该 URL 的 `POST`，携带
  `Accept: application/json, text/event-stream`。服务器以纯 JSON 响应体或
  SSE 事件流应答；对通知的应答是 `202`。
- SSE 响应按**增量**解码：事件随字节到达即可用——被拆分或粘连的 TCP
  分片、一个流里的多个事件都会被正确重组——因此服务器在最终结果之前
  先流式推送进度通知时，调用不会被卡到响应体结束。
- **会话 ID。** 服务器在 `initialize` 响应（或在尽力而为的 `GET` 预检上，
  面向要求每个请求都带会话的服务器）返回的 `Mcp-Session-Id` 会被捕获，
  并作为请求头在之后的每个请求上回带。
- **旧版 SSE 回退。** 若服务器以 404/405/406/415/501 拒绝该 `POST`，
  客户端切换到旧版 SSE 传输（在长连 `GET` 上做端点发现，消息 `POST` 到
  发现的端点）。对只支持旧版 SSE 的服务器设 `transport = "sse"` 跳过协商，
  WebSocket 端点用 `"ws"`。

### 超时

- `connect_timeout` 约束 TCP 连接与握手（`initialize` + 工具/资源/提示词
  发现），以及旧版 SSE 的 GET + 端点发现阶段。
- `execute_timeout` 端到端约束每次工具调用（请求发出到响应收到）。
- `read_timeout` 约束任一 HTTP 流上字节之间的空闲间隔——Streamable HTTP
  响应流与旧版 SSE `GET` 流同样适用。持续送出字节的流可以自由地活过
  `connect_timeout`；沉默的对端会在 `read_timeout` 后被切断。

### 断线与重试

- **会话过期**（HTTP 404、JSON-RPC 错误 `-32001`，或响应体提到
  "expired/invalid session"）：丢弃缓存的会话 ID、重建连接，并把工具调用
  重试**一次**。
- **长连流被切断**（旧版 SSE `GET` 或 WebSocket 关闭）：下次调用时同样做
  一次重连并重试。后台不会主动重连；重连在下次使用时惰性发生。
- **Streamable HTTP 流在响应到达前结束**：调用以可诊断的错误失败，且
  **不**重试——服务器已经收到请求、可能已执行了工具，自动重放可能让副作用
  翻倍。若确认安全，可手动重新发起调用。
- **401**：一次凭据刷新（已存储的 OAuth token，或 Claude.ai 刷新命令）加
  一次重试。持续的认证失败会被缓存 15 分钟，REPL 会显示登录提示而不是
  每回合重试。

### 局限

- 不会打开独立的服务器到客户端 `GET` 流：不支持服务器发起的请求
  （sampling、roots 等）。随响应流到达的通知会被解码后忽略。
- 空闲流在 `read_timeout` 后被切断；若服务器在没有 SSE keep-alive 的情况下
  把响应流挂起超过该时长，下次调用会触发一次重连。
- 在事件中途被截断的响应按宽松方式解析：被切断的尾部表现为 JSON-RPC
  解码错误，而不是专门的截断错误。

## OAuth

需要用户 OAuth 的基于 URL 的 MCP 服务器（例如 GitHub）可以通过设备流授权，而不必把一个长期有效的 token 粘贴进 `headers`。

在 `mcp.json` 的服务器条目中加一个 `oauth` 表：

```jsonc
{
  "mcpServers": {
    "github": {
      "url": "https://api.githubcopilot.com/mcp/",
      "oauth": {
        "provider": "github",
        "client_id": "<your OAuth app client id>",
        "scopes": ["repo"]
      }
    }
  }
}
```

字段：

- `provider`（字符串，必填）：`github` 会填入标准的 GitHub 设备码和 token 端点。任何其他值都被视为自定义 provider，需要显式给出 `device_code_url` 和 `token_url`。
- `client_id`（字符串，必填）。
- `scopes`（字符串数组，可选）：以空格拼接后作为设备流的 `scope` 参数。
- `device_code_url` / `token_url`（字符串，可选）：非 GitHub provider 的自定义端点。

然后授权一次：

```console
$ codesmith mcp auth github
Authorization required for MCP server `github` (github).
  1. Open:  https://github.com/login/device
  2. Code:  ABCD-1234   (expires in 900s)
Waiting for authorization…
Authorization stored for MCP server `github`.
```

行为：

- token 通过 `codesmith-secrets` 存储——系统密钥环可用时存入密钥环，否则回落到 `~/.codesmith/secrets/` 下的权限受控文件。它永远不会落进 `mcp.json`。
- 连接时，存储的 token 会作为 `Authorization: Bearer` 头注入，除非 `headers` 已经带了该头。当服务器应答 401 时，HTTP 传输会通过 refresh-token 授权刷新一次。用户在 `headers.Authorization` 里配置的头永远优先，不会被覆盖。
- stdio 服务器忽略 `oauth`——凭据请通过它们的 `env` 表传递。
- 不在范围内：PKCE/重定向流和 client-credentials 授权。

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

- 运行 `codesmith doctor`，确认它解析出的 MCP 配置路径以及该文件是否存在。
- 在 REPL 中运行 `/mcp validate`，刷新可见的服务器/工具快照。
- 如果 MCP 配置缺失，运行 `codesmith mcp init --force` 重新生成。
- 如果工具没有出现，请验证服务器命令能否在你的 shell 中运行，以及服务器是否支持 MCP `tools/list`。
