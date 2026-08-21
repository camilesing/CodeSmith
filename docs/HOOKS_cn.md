# 钩子（生命周期 Shell 命令）

CodeSmith 可以在代理生命周期的明确定义节点上运行你的 shell 命令：会话
开始与结束、每次工具执行之前/之后、模式切换时、出错时、任务创建前后，
以及上下文压缩（compaction）前后。钩子可用于审计日志、通知、临时凭据
注入和提交前文本转换。

钩子配置在 `~/.codesmith/config.toml` 的 `[hooks]` 表下（旧版
`~/.deepseek/config.toml` 也会被解析）。项目级
`<workspace>/.codesmith/config.toml` 可以携带自己的 `[hooks]` 表——
存在时它会**整体替换**用户级表，而不是合并（与 `instructions` 的规则
相同）。如果两者都要，请在项目表内再次列出全局钩子。

钩子是普通 shell 命令。它们以你的完整用户权限运行——不受沙箱保护，
也不构成安全边界（参见[安全说明](#安全说明)）。

## 快速开始

追加到 `~/.codesmith/config.toml`：

```toml
[[hooks.hooks]]
event = "session_start"
command = "echo 'CodeSmith session started'"

[[hooks.hooks]]
name = "audit-shell"
event = "tool_call_after"
command = "~/.codesmith/hooks/audit-log.sh"
condition = { type = "tool_name", name = "exec_shell" }
```

在 TUI 中运行 `/hooks`（或 `/hooks list`）确认两条条目均已加载，
运行 `/hooks events` 列出所有可作为目标的事件名称。

## 配置模式

`[hooks]` 表持有全局开关；每个 `[[hooks.hooks]]` 条目是一个钩子。

```toml
[hooks]
enabled = true                # 全局总开关；false 会抑制所有钩子
default_timeout_secs = 30     # 表级超时；覆盖各钩子的值

working_dir = "/some/dir"     # 钩子 cwd；默认为工作区

[[hooks.hooks]]
event = "message_submit"      # 下文列出的事件之一（必填）
command = "my-script.sh"      # shell 命令（必填）
name = "inject-context"       # 可选；显示在 /hooks 和日志中
timeout_secs = 2              # 单钩子超时；设置了
                              # default_timeout_secs 时被忽略
background = false            # 即发即忘；stdout 契约失效
continue_on_error = true      # 钩子失败时的处理方式
condition = { type = "mode", mode = "agent" }   # 可选；见条件
```

字段说明：

- `command` 通过平台 shell 运行——Unix 上是 `sh -c`，Windows 上是
  `cmd /C`——因此在 Unix 上 `~` 展开、管道和 `&&` 链都可以工作。
- 超时优先级为表级高于钩子级：一旦设置了 `default_timeout_secs`，它
  就适用于每个钩子，即使那些钩子有自己的 `timeout_secs`。超过超时的
  钩子会被杀死并计为失败（继承管道的子孙进程不会被等待，因此超时
  得以保持）。
- `continue_on_error` 只影响后续钩子，以及（对 `message_submit` 而言）
  失败是否阻断提交；见下文。无论何种情况，失败总会记录一条
  `tracing::warn!`。
- 钩子按配置顺序串行运行。一个 `continue_on_error = false` 的失败钩子
  会停止该事件的其余钩子。
- `background = true` 在一个分离线程上启动命令并立即返回：没有超时、
  没有 stdout 捕获，处处仅为观察者。

## 事件

| 事件 | 别名 | 触发时机 | 可变性 |
|---|---|---|---|
| `session_start` | — | TUI 启动时一次 | observer |
| `session_end` | — | 优雅关闭时一次 | observer |
| `message_submit` | — | 在提交的消息进入历史或到达模型之前 | **可替换或阻断文本** |
| `tool_call_before` | `pre_tool_use` | 每次工具执行之前 | observer |
| `tool_call_after` | `post_tool_use` | 每次工具执行完成之后 | observer |
| `mode_change` | — | Plan / Agent / YOLO 模式切换时 | observer |
| `on_error` | — | 传输 / 容量 / 工具出错时 | observer |
| `shell_env` | — | 每次调用 `exec_shell` 之前立即 | **stdout 成为环境变量** |
| `task_created` | — | 任务管理器创建任务时 | observer |
| `task_completed` | — | 被跟踪的任务完成时 | observer |
| `pre_compact` | — | 上下文压缩之前（手动、自动或紧急） | **stdout 是被保留的上下文** |

各分组详情：

- **生命周期。** `session_start` / `session_end` 在每个 TUI 进程中
  触发一次。适合加载状态、设置日志或发送通知。
- **`message_submit`** 是唯一能够更改代理输入的事件。stdin/stdout
  JSON 契约和退出码 2 的阻断语义见[钩子输出](#钩子输出)。它在文件
  提及展开、技能包装、自动路由和历史追加*之前*运行——转换后的文本
  才是模型看到的。
- **工具调用。** `tool_call_before` / `tool_call_after` 是只读观察者：
  它们不能否决工具调用，也不能修改其参数。门控是审批流程、沙箱和
  execpolicy 规则的职责（参见 [docs/SANDBOX.md](SANDBOX.md)）。
- **`shell_env`** 在每次 `exec_shell` 之前同步运行。其 stdout 被解析
  为 `KEY=VALUE` 行并合并进被派生进程的环境（后面的钩子覆盖前面的）。
  可用于临时凭据或按技能的 `PATH` 调整。失败和超时不贡献任何变量——
  shell 调用本身总是照常进行。
- **任务。** `task_created` / `task_completed` 携带 `task_id`、
  `task_subject` 和 `task_status` 上下文。
- **`pre_compact`** 在上下文被摘要之前触发。每个匹配的非后台钩子的
  stdout 会被拼接（以 `---` 分隔线隔开）并合并进压缩摘要，因此你
  打印的材料会在摘要化后存活。失败不会造成阻断：压缩总是照常进行。

## 条件

每个钩子都接受可选的 `condition`。没有条件（或为
`{ type = "always" }`）时，钩子在其事件触发时无条件运行。

```toml
# 仅针对某一个工具
condition = { type = "tool_name", name = "exec_shell" }

# 仅针对某一类工具
condition = { type = "tool_category", category = "shell" }

# 仅在某个模式下
condition = { type = "mode", mode = "yolo" }

# 仅当工具以特定退出码结束（tool_call_after）
condition = { type = "exit_code", code = 1 }

# 用 AND / OR 组合
condition = { type = "all", conditions = [
  { type = "tool_name", name = "exec_shell" },
  { type = "mode", mode = "agent" },
] }
condition = { type = "any", conditions = [ ... ] }
```

`tool_category` 匹配器把工具名映射到类别：

| 工具 | 类别 |
|---|---|
| `exec_shell` | `shell` |
| `write_file`, `edit_file`, `apply_patch` | `file_write` |
| `read_file`, `list_dir`, `grep_files` | `safe` |
| 其他任何工具 | `other` |

`mode` 匹配不区分大小写；`tool_name` 是精确匹配。

## 钩子输入

### 环境变量

每个钩子都会收到一组描述事件上下文的 `DEEPSEEK_*` 环境变量。
（`DEEPSEEK_` 前缀是历史遗留，适用于所有 CodeSmith 钩子。）未设置
的字段在环境中直接缺席。

| 变量 | 出现于 | 内容 |
|---|---|---|
| `DEEPSEEK_TOOL_NAME` | 工具事件、`shell_env` | 工具名，例如 `exec_shell` |
| `DEEPSEEK_TOOL_ARGS` | 工具事件 | 以 JSON 字符串表示的工具参数 |
| `DEEPSEEK_TOOL_RESULT` | `tool_call_after` | 工具输出，截断至 10 KiB |
| `DEEPSEEK_TOOL_EXIT_CODE` | `tool_call_after` | 适用时的退出码 |
| `DEEPSEEK_TOOL_SUCCESS` | `tool_call_after` | `true` / `false` |
| `DEEPSEEK_MODE` | 多数事件 | 当前模式（`plan` / `agent` / `yolo`） |
| `DEEPSEEK_PREVIOUS_MODE` | `mode_change` | 切换前的模式 |
| `DEEPSEEK_SESSION_ID` | 多数事件 | **临时**遥测 id，见下文 |
| `DEEPSEEK_THREAD_ID` | 多数事件 | 持久线程 id，见下文 |
| `DEEPSEEK_MESSAGE` | `message_submit` | 当前（可能已被转换的）文本，截断至 5 KiB |
| `DEEPSEEK_ERROR` | `on_error` | 错误消息 |
| `DEEPSEEK_WORKSPACE` | 多数事件 | 工作区路径 |
| `DEEPSEEK_MODEL` | 多数事件 | 当前模型名 |
| `DEEPSEEK_TOTAL_TOKENS` | 多数事件 | 目前已使用的总 token 数 |
| `DEEPSEEK_SESSION_COST` | 多数事件 | 以美元计的会话成本 |
| `DEEPSEEK_TASK_ID` / `DEEPSEEK_TASK_SUBJECT` / `DEEPSEEK_TASK_STATUS` | 任务事件 | 任务元数据 |

> **会话与线程标识。** `DEEPSEEK_SESSION_ID` 是每次会话启动时重新生成
> 的临时 id——它**不**跨重启关联。若需要能在恢复后存活的相关性（审计
> 追踪、容量记忆），请使用 `DEEPSEEK_THREAD_ID`，它携带持久的线程 id。

### stdin

只有两个事件会在 stdin 上传递结构化 JSON；所有其他事件在没有 stdin 的
情况下运行。

`message_submit` 收到：

```json
{
  "event": "message_submit",
  "text": "original user text",
  "session_id": "sess_12345678",
  "thread_id": "thread-abc",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "total_tokens": 1234
}
```

`pre_compact` 收到同样的信封，事件键为 `hook_event_name` 且没有
`text` 字段：

```json
{
  "hook_event_name": "pre_compact",
  "session_id": "sess_12345678",
  "thread_id": "thread-abc",
  "workspace": "/path/to/workspace",
  "model": "deepseek-chat",
  "total_tokens": 1234
}
```

## 钩子输出

三个事件会解释钩子的 stdout；对其他所有事件，stdout 都被忽略。

### `message_submit`（转换或阻断）

- 退出 `0` + stdout JSON 带有非空字符串 `text` 字段 → 该值**替换**
  提交的文本：`{"text": "replacement user text"}`
- 退出 `0` + stdout 为空，或 JSON 无 `text`，或 `{"text": ""}` →
  文本不变
- 退出 `0` + stdout JSON 格式非法 → 文本不变，记录一条警告
- 退出 `2` → 提交在回合开始前被**阻断**；`reason` 字段、stderr 或
  stdout 提供显示在 TUI 中的消息
- 任何其他非零退出、超时或启动失败 → 由 `continue_on_error` 决定：
  `true` 保留当前文本并继续后续钩子（附带一条 TUI 状态消息）；
  `false` 阻断提交

多个 `message_submit` 钩子按配置顺序运行，每个钩子收到上一个钩子产出
的文本。此事件上 `background = true` 的钩子仅为观察者——既不能转换
也不能阻断。

### `shell_env`（KEY=VALUE 行）

```text
AWS_ACCESS_KEY_ID=...
AWS_SECRET_ACCESS_KEY=...
```

stdout 上每行一个 `KEY=VALUE`；接受可选的 `export ` 前缀。变量被
合并进 `exec_shell` 环境；后面的钩子覆盖前面的。解析出的 KEY 名称
（绝不包括值）会被写入 `~/.codesmith/audit.log`，以便核对会话而不
泄露秘密材料。

### `pre_compact`（自由文本）

每个匹配的非后台钩子在 stdout 上的全部内容会被拼接（以 `---` 分隔线
隔开）并合并进压缩摘要。打印出你希望在摘要化后存活的事实即可。

## 执行语义

- **Shell：** Unix 上为 `sh -c <command>`，Windows 上为
  `cmd /C <command>`。
- **工作目录：** 若设置了 `[hooks].working_dir` 则使用之，否则为
  当前工作区。
- **顺序：** 按事件串行，顺序与配置一致。
- **超时：** 每钩子 `timeout_secs`（默认 30），除非设置了表级
  `default_timeout_secs`，后者胜出。超时的钩子被杀死并计为失败。
- **失败处理：** 失败在 `hooks` target 下记录一条 `tracing::warn!`。
  当 `continue_on_error = true`（默认）时后续钩子仍会运行；为
  `false` 时该事件的其余钩子被跳过。
- **启用：** `[hooks].enabled = false` 抑制一切；
  `/hooks list` 会显示此状态。

## 查看钩子

- `/hooks` 或 `/hooks list` — 按事件分组的全部已配置钩子，含名称、
  命令预览、超时和条件；并显示全局启用标志是否抑制了它们。
- `/hooks events` — 所有可用于 `event = "..."` 的事件名称，各附一行
  触发时机说明。

## 安全说明

- 钩子命令**以你的完整用户权限**运行，不受沙箱保护。任何能写你的
  `config.toml` 的人（或你能打开的仓库中的项目级
  `.codesmith/config.toml`）都可以通过钩子以你的身份运行任意命令——
  在不受信任的仓库中工作前请先审查项目配置。
- `shell_env` 的值存在于进程环境中，在某些平台上可能出现在子进程
  列表里。审计日志只记录键名，绝不记录值。
- 工具参数和结果（通过 `DEEPSEEK_TOOL_ARGS` / `DEEPSEEK_TOOL_RESULT`
  暴露）可能包含你仓库中的秘密；请据此对待钩子的 stdout/日志。

## 钩子不是什么

- **不是门控机制。** `tool_call_before` 不能否决或改写工具调用。
  审批提示、沙箱和 execpolicy 规则才是强制执行层（参见
  [docs/SANDBOX.md](SANDBOX.md)）。
- **不是 `[hook_sinks]`。** `[hook_sinks]` 配置表服务于一个无关的
  可观测性系统（HTTP API 服务器的 stdout / JSONL / webhook /
  Unix-socket 事件汇）。生命周期钩子只存在于 `[hooks]` 之下。
- **不是扩展系统。** 关于带事件总线的进程内 Rust 扩展，参见
  [docs/EXTENSIONS.md](EXTENSIONS.md)。
- `turn_end`、`subagent_spawn` 和 `subagent_complete` 钩子事件在
  [docs/rfcs/1364-hooks-lifecycle.md](rfcs/1364-hooks-lifecycle.md)
  中有草案，但尚未实现。

## 配方

### 为每条 shell 命令记录审计日志

```toml
[[hooks.hooks]]
name = "shell-audit"
event = "tool_call_after"
command = "printf '%s\\t%s\\t%s\\n' \"$DEEPSEEK_THREAD_ID\" \"$DEEPSEEK_TOOL_NAME\" \"$DEEPSEEK_TOOL_EXIT_CODE\" >> ~/.codesmith/hooks/shell-audit.log"
condition = { type = "tool_name", name = "exec_shell" }
```

### 在每次提交前注入上下文

```toml
[[hooks.hooks]]
name = "inject-todo"
event = "message_submit"
command = "~/.codesmith/hooks/inject-context.sh"
timeout_secs = 2
continue_on_error = true
```

`~/.codesmith/hooks/inject-context.sh`:

```sh
#!/bin/sh
# Read the JSON payload from stdin, prepend current TODOs to the text.
input=$(cat)
text=$(printf '%s' "$input" | jq -r .text)
todos=$(cat ~/.codesmith/TODO.md 2>/dev/null || true)
if [ -n "$todos" ]; then
  jq -n --arg t "$text

<context>
$todos
</context>" '{text: $t}'
fi
# Empty stdout leaves the submission unchanged.
```

### 为每次 shell 调用提供临时凭据

```toml
[[hooks.hooks]]
name = "aws-creds"
event = "shell_env"
command = "aws-vault export my-profile --format=env"
```

### 让事实在压缩后存活

```toml
[[hooks.hooks]]
name = "preserve-decisions"
event = "pre_compact"
command = "cat .codesmith/DECISIONS.md 2>/dev/null"
```

## 故障排查

- **什么都不触发。** 先查看 `/hooks list`：钩子必须出现在那里，且
  页眉会显示 `[hooks].enabled = false` 是否正在抑制一切。项目级
  `[hooks]` 表会静默替换你的用户级表。
- **钩子静默失败。** 失败记录在 `hooks` tracing target 下——以
  `RUST_LOG=hooks=warn`（或 `=debug`）运行以查看退出码、stderr 开头
  和耗时。
- **`message_submit` 的 stdout 被忽略。** 契约很严格：退出 `0` 时
  单个带非空 `text` 字段的 JSON 对象。`{"text": ""}` 和非 JSON 输出
  会被忽略并附警告。后台钩子从不转换。
- **超时杀死长时间运行的钩子。** 要么调大 `timeout_secs`，要么设置
  `background = true`（接受 stdout 因此被忽略）。
- **条件从不匹配。** `tool_name` 是精确匹配；`tool_category` 只认识
  `shell`、`file_write`、`safe` 和 `other`。`/hooks list` 会在每个
  钩子旁渲染其条件。
