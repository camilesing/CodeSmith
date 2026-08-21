# 工具面

为什么是这些特定的工具、这样的分组方式，以及在每个场景下如何相对于可用的 shell 等价物来选择它们。本文档是 `crates/tui/src/prompts/agent.txt` 的配套说明。

## 设计立场

- **只要专用工具能返回结构化输出，就优先于 `exec_shell` 使用专用工具。** Bash 转义容易出错，且平台行为各异（GNU 与 BSD 的 `grep` 差异、`rg` 并非处处安装）。结构化输出还能让模型免去重新解析自由格式文本的负担。
- **其余一切交给 `exec_shell`。** 构建、测试、格式化、lint、临时命令、任何平台特定的操作。我们不去包装那条长长的尾巴。
- **砍掉不比 shell 等价物更好的工具。** 针对同一底层操作的双工具别名是模型陷阱——LLM 会在两者之间来回切换，缓存命中率随之受损。

## 当前工具面（v0.8.35）

### 文件操作

| 工具 | 定位 |
|---|---|
| `read_file` | 读取 UTF-8 文件。可用时通过 `pdftotext`（poppler）自动提取 PDF 内容；`pages: "1-5"` 可切分大文档。 |
| `list_dir` | 结构化、感知 gitignore 的目录列表。优先于 `exec_shell("ls")`。 |
| `write_file` | 创建或覆写文件。 |
| `edit_file` | 在单个文件内进行搜索替换。比完整重写更省。 |
| `apply_patch` | 应用 unified diff。多块（multi-hunk）编辑的正确选择。 |
| `retrieve_tool_result` | 读取此前溢出到 `~/.codesmith/tool_outputs/` 的大型工具输出的摘要或切片；使用 `summary`、`head`、`tail`、`lines` 或 `query`，而不是重放整个结果。 |
| `handle_read` | 从活跃工具环境持有的 `var_handle` 载荷中读取有界投影。这是 RLM 会话、子代理转录及其他大型符号载荷的基础。 |

### 搜索

| 工具 | 定位 |
|---|---|
| `grep_files` | 用正则在工作区内搜索文件内容；返回结构化匹配结果 + 上下文行。纯 Rust 实现（`regex` crate），不外部调用 `rg`/`grep`。 |
| `file_search` | 对文件名（而非内容）做模糊匹配。当你大致知道名字时使用。 |
| `web_search` | 默认 DuckDuckGo，Bing 兜底；可在配置中选择 Bing、Tavily、Bocha、Metaso 和 Baidu。返回排序后的摘要片段 + 用于引用的 `ref_id`。 |
| `fetch_url` | 对已知 URL 直接发起 HTTP GET。当链接已知时比 `web_search` 更快。默认将 HTML 剥离为纯文本。 |

### Shell

| 工具 | 定位 |
|---|---|
| `exec_shell` | 运行 shell 命令。前台运行可取消，但只应将其用于有界的命令；超时会杀死进程并返回后台重跑提示。 |
| `exec_shell_wait` | 轮询后台任务以获取增量输出。取消当前回合会停止等待，但不会杀死任务。 |
| `exec_shell_interact` | 向运行中的后台任务发送 stdin 并读取增量输出。 |
| `exec_shell_cancel` | 按 id 取消一个运行中的后台 shell 任务，或在被明确要求时取消全部运行中的后台 shell 任务。 |
| `task_shell_start` | 在后台启动一个长时间运行的命令并立即返回。对于可能运行数分钟的诊断、测试、搜索和服务器，优先于前台 shell 使用。 |
| `task_shell_wait` | 轮询后台命令。如果在完成后提供了 `gate`，则在活跃的持久任务上记录结构化的门控（gate）证据。 |

当前台 shell 命令超时时，进程不会被静默延续。工具结果会告知模型用
`task_shell_start` 或 `background = true` 的 `exec_shell` 重跑长任务，然后用
`task_shell_wait` 或 `exec_shell_wait` 轮询。

交互式 shell 任务也可以通过 `/jobs` 查看。TUI 任务中心与
`exec_shell`/`task_shell_start` 使用同一个 shell 管理器，并显示命令、cwd、
已运行时间、状态、输出尾部、进程本地 shell id，以及可用时关联的持久任务
id。`/jobs show`、`/jobs poll`、`/jobs wait`、`/jobs stdin` 和
`/jobs cancel` 为活跃任务提供查看、轮询、stdin 和取消控制。任务是进程
本地的；重启后不会重新挂接活跃进程状态，任何被记住的分离条目必须标记为
陈旧，而不是当作活跃进程呈现。

Shell 权限策略由 `crates/execpolicy` 评估。Deny 前缀先于 trusted 前缀
检查，并且无论处于哪一层都会阻断匹配的命令。Trusted 前缀只在允许信任
快捷方式的模式下才跳过审批。类型化的 ask 记录目前是一个狭窄的基础：
当其在 `AskForApproval::Never` 下匹配时，命令会被拒绝，因为运行时无法
询问用户；其余现有的 allow/deny 行为保持不变。

### MCP 管理器与调色板发现

MCP 服务器配置通过 TUI 中的 `/mcp` 和 `/config` 中的 `mcp_config_path`
行呈现。`/mcp` 显示解析后的配置路径、服务器启用/禁用状态、传输方式、
命令或 URL、超时、连接错误，以及发现的工具/资源/提示。它支持一组狭窄的
管理器动作：init、add、enable、disable、remove、validate 和
reload/reconnect。配置编辑会立即写入，但编辑后对模型可见的 MCP 工具池
需要重启才能生效。

命令调色板包含按服务器分组的 MCP 条目。禁用和失败的服务器保持可见，
发现的工具/提示使用向模型展示的运行时名称，例如 `mcp_<server>_<tool>`。

### Git / 诊断 / 测试

| 工具 | 定位 |
|---|---|
| `git_status` | 无需运行 shell 即可查看仓库状态。 |
| `git_diff` | 查看工作树或暂存区 diff。 |
| `diagnostics` | 一次调用获取工作区、git、沙箱和工具链信息。 |
| `run_tests` | 带可选参数的 `cargo test`。 |

### 任务管理与持久工作

| 工具 | 定位 |
|---|---|
| `update_plan` | 面向复杂多阶段工作的可选高层策略元数据；请让 `checklist_write` 保持为主要进度呈现面。 |
| `task_create` | 通过 `TaskManager` 创建/入队一个持久后台任务。这是长时间运行的代理工作真正的可执行工作对象。 |
| `task_list` | 列出持久任务及其状态和关联的运行时 id。 |
| `task_read` | 读取持久任务详情：线程/回合关联、时间线、清单、门控、产物、PR 尝试、GitHub 事件。 |
| `task_cancel` | 取消排队中或运行中的持久任务。需要审批。 |
| `checklist_write` | 在活跃线程/任务下的细粒度进度。清单状态从属于持久任务。 |
| `checklist_add` / `checklist_update` / `checklist_list` | 单条清单项操作。 |
| `todo_write` / `todo_add` / `todo_update` / `todo_list` | 清单工具的兼容别名。现有会话可继续工作，但新提示应使用 `checklist_*`。 |
| `note` | 记录一条供以后使用的重要事实。 |

### 验证门控与产物

| 工具 | 定位 |
|---|---|
| `task_gate_run` | 运行一个已获批准的验证命令，并将结构化证据附加到活跃的持久任务上：命令、cwd、退出码、耗时、分类、摘要和日志产物。 |

大型日志和命令输出应作为产物保存，并在转录中保留紧凑摘要。对于活跃的
持久任务，`task_gate_run` 会自动完成这一工作。

### GitHub 上下文与受保护的写入

| 工具 | 定位 |
|---|---|
| `github_issue_context` | 通过 `gh issue view` 获取只读 issue 上下文；大正文在可能时转为任务产物。 |
| `github_pr_context` | 通过 `gh pr view` 获取只读 PR 上下文；可通过 `gh pr diff --patch` 可选地捕获 diff；大正文/diff 在可能时转为任务产物。 |
| `github_comment` | 需要审批的 issue/PR 评论，附带结构化证据。 |
| `github_close_issue` | 需要审批的 issue 关闭。要求非空的验收标准和证据；除非明确允许，否则拒绝脏工作树。绝不用于 PR。 |
| `github_close_pr` | 需要审批的 PR 关闭。要求与 issue 关闭相同的结构化证据，并在工具输出/审计记录中保留 PR 措辞。 |

### PR 尝试

| 工具 | 定位 |
|---|---|
| `pr_attempt_record` | 将当前 git diff 捕获为尝试元数据，并在持久任务上附加一个补丁产物。 |
| `pr_attempt_list` | 列出任务上记录的尝试。 |
| `pr_attempt_read` | 查看一条已记录的尝试及其产物引用。 |
| `pr_attempt_preflight` | 对尝试补丁运行 `git apply --check`。不改动工作树。 |

### 自动化

| 工具 | 定位 |
|---|---|
| `automation_create` | 创建一个定时自动化。需要审批。 |
| `automation_list` / `automation_read` | 查看持久自动化及最近的运行。 |
| `automation_update` | 更新提示、计划、cwds 或状态。需要审批。 |
| `automation_pause` / `automation_resume` / `automation_delete` | 生命周期控制。需要审批。 |
| `automation_run` | 立即运行一个自动化；该运行会入队为一个普通持久任务。需要审批。 |

### 子代理

v0.8.33 开始将大型工具输出迁移到符号句柄：工具返回小型 `var_handle`
对象，`handle_read` 从底层环境中获取有界切片、计数或 JSON 投影。这样既
保持父转录紧凑，又保留了通向完整载荷的恢复路径。

当前面向模型的子代理工具面是持久化的，并且刻意保持很小：

| 工具 | 定位 |
|---|---|
| `agent_open` | 打开一个命名子代理会话以进行独立工作。立即返回一个会话投影，父代理可继续协调。 |
| `agent_eval` | 发送后续输入、阻塞等待完成，或获取现有会话的当前投影/转录句柄。 |
| `agent_close` | 按名称或 id 取消或释放一个子代理会话。 |

委派协议见 `agent.txt`，角色分类（`general` / `explore` / `plan` /
`review` / `implementer` / `verifier` / `custom`）见
[`SUBAGENTS.md`](SUBAGENTS.md)。

`agent_open` 默认开启一个全新的子会话。对于延续式工作或需要继承父级
上下文的多视角评审，传入 `fork_context: true`。在 fork 模式下，运行时
会尽可能逐字节保留父级 prefill/提示前缀，以便复用 DeepSeek 的前缀
缓存，然后再追加子级角色指令和任务。

### 递归 LM 会话

RLM 现在同样是持久化的：

| 工具 | 定位 |
|---|---|
| `rlm_session_objects` | 为活跃提示、会话元数据、转录、最新用户消息和逐消息引用列出紧凑卡片。 |
| `rlm_open` | 基于文件、内联内容或 URL 打开一个命名 Python REPL。 |
| `rlm_eval` | 在该会话中运行有界 Python，使用确定性代码和 REPL 内语义辅助函数（如 `sub_query_batch`）。 |
| `rlm_configure` | 调整输出反馈、子查询超时/深度和会话共享设置。 |
| `rlm_close` | 关闭 Python 运行时并返回最终会话统计。 |

`rlm_open` 还接受 `session_object`，即 `rlm_session_objects` 返回的
稳定引用，例如 `session://active/system_prompt`、
`session://active/transcript` 或 `session://active/messages/0`。这会将
选定的对象加载到 RLM REPL 中，且只向父转录返回元数据。转录对象将思考
块和大型工具结果保留为紧凑元数据；请通过返回的 `var_handle` 值和
`handle_read` 查看大型载荷，而不是让父转录粘贴原始文本。

大型 RLM 输出应以 `var_handle` 形式返回。使用 `handle_read` 获取有界
文本切片、行范围、计数或 JSONPath 投影，而不是将完整值重放进父转录。

在 `rlm_eval` 内部，被加载的源可通过 `_context` 访问；`_ctx` 和
`content` 也作为兼容别名绑定，因为代理在做 Python 分析时会自然地使用
它们。较短的 `context` 和 `ctx` 名称则刻意不绑定，以便用户变量可以
使用它们而不会与引导程序（bootstrap）冲突。

子调用超时属于会话策略：在运行大规模扇出之前，使用 `rlm_configure`
设置 `sub_query_timeout_secs`。辅助函数 `sub_query`、
`sub_query_batch`、`sub_query_map` 和 `sub_rlm` 出于对常见代理猜测的
兼容而接受 `timeout_secs` 关键字，但有效超时仍在 RLM 会话级别配置。

`finalize(value, confidence=...)` 保留可 JSON 序列化的值。字符串变为
文本句柄；dict、list、数字、布尔和 null 变为 JSON 句柄，`handle_read`
可用 JSONPath 对其进行投影。

### 会话接力

`/relay [focus]` 让当前代理将 `.deepseek/handoff.md` 写成一个紧凑的
`# Session relay` 产物，交给下一个线程。文件名出于与现有提示加载和旧
会话的兼容而保留；可见的心智模型是 relay / 接力。

别名：`/batonpass`、`/接力`。

在长假、压缩（compaction）之前，或将工作转移到新会话之前使用它。接力
应保留目标、当前 Work 清单项、已更改的文件、决策、验证状态，以及一个
具体的下一步动作。

### 并行扇出：成本级别上限

两个工具提供并行扇出，但并发上限不同，反映了截然不同的成本级别：

| 工具 | 每个子任务做什么 | 墙钟时间 | Token 成本 | 上限 |
|---|---|---|---|---|
| `agent_open` | 完整子代理循环（规划、工具调用、多轮流式传输，可开启子级） | 分钟级 | 数千 token | 默认并发 10（`[subagents].max_concurrent`，硬上限 20） |
| `rlm_eval` 辅助函数 `sub_query_batch` | 在活跃 RLM 会话内固定使用 `deepseek-v4-flash` 的一次性非流式 Chat Completions 调用 | 秒级 | 约数百 token | 每次调用 16 |

这些上限出现在各工具的描述和错误消息中，以便模型（和用户）为工作选择
合适的工具。如果一个子代理足够、但你需要对同一已加载上下文做并行语义
查询，优先使用带 `sub_query_batch` 的 `rlm_eval`；如果每个任务都需要
自己的携带工具的代理循环，则使用 `agent_open`，并等待运行中的会话完成，
或用 `agent_close` 取消不再需要的运行中会话。

## 已移除的旧别名与工具面

v0.8.33 从主动提示和工具目录中移除了旧的面向模型的子代理扇出工具面。
不要在新的主动指导中使用这些名称：`agent_spawn`、`agent_wait`、
`agent_result`、`agent_send_input`、`agent_assign`、`agent_resume`、
`agent_list`、`spawn_agent`、`delegate_to_agent`、`send_input` 和
`close_agent`。

旧的一次性 `rlm` 面向模型工具也已被持久的
`rlm_open` / `rlm_eval` / `rlm_configure` / `rlm_close` 会话取代。

历史兼容结果可能包含形如下例的 `_deprecation` 块：

```json
{
  "_deprecation": {
    "this_tool": "spawn_agent",
    "use_instead": "agent_open",
    "removed_in": "0.8.33",
    "message": "Tool 'spawn_agent' is deprecated; switch to 'agent_open'."
  }
}
```

这是旧版/兼容性说明，不是当前推荐的工具面。

## 发布冒烟测试：验证实时的名称

在验证发布时，直接验证模型可见的注册表名称。不要 grep 随机的处理函数
名；在注册表契约保持稳定的同时，允许处理函数名发生漂移。

版本冒烟测试：

```bash
codesmith --version
codesmith-tui --version
```

工具面冒烟测试：

```bash
rg -n '"handle_read"|"rlm_open"|"rlm_eval"|"rlm_configure"|"rlm_close"|"agent_open"|"agent_eval"|"agent_close"' crates/tui/src
rg -n 'handle_read|rlm_open|rlm_eval|rlm_configure|rlm_close|agent_open|agent_eval|agent_close' docs crates/tui/src/prompts crates/tui/src/tools
```

v0.8.35 的权威实时名称为：

- `handle_read`
- `rlm_open`, `rlm_eval`, `rlm_configure`, `rlm_close`
- `agent_open`, `agent_eval`, `agent_close`

注册表不应在旧版/移除说明之外主动宣传旧的一次性名称
`agent_spawn`、`agent_wait`、`agent_result` 或旧的前台 `rlm` 工具。
历史变更日志条目和兼容性代码仍可能提及它们。

## 为什么我们不提供单一 `bash` 工具

单一 `bash` 代理（Claude Code 的设计）很强大，但会把 shell 脚本的所有
陷阱交给模型：引号、平台分歧、误读 cwd 带来的副作用、`cd` 不在调用间
持久化等。我们的文件工具在转录中的渲染成本也显著更低（结构化的
JSON 形状输出比 `ls -la` 的文字墙坍缩得更好）。

当缺少某样能力时，模型始终可以退回 `exec_shell`。专用工具只是把常见的
80% 从 shell 应急通道上卸下来。
