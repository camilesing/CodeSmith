# 工具面

为什么是这些特定的工具、这样的分组方式，以及在每个场景下如何相对于可用的 shell 等价物来选择它们。本文档是 `crates/agent-runtime/src/prompts/agent.txt` 的配套说明。

## 设计立场

- **只要专用工具能返回结构化输出，就优先于 `exec_shell` 使用专用工具。** Bash 转义容易出错，且平台行为各异（GNU 与 BSD 的 `grep` 差异、`rg` 并非处处安装）。结构化输出还能让模型免去重新解析自由格式文本的负担。
- **其余一切交给 `exec_shell`。** 构建、测试、格式化、lint、临时命令、任何平台特定的操作。我们不去包装那条长长的尾巴。
- **砍掉不比 shell 等价物更好的工具。** 针对同一底层操作的双工具别名是模型陷阱——LLM 会在两者之间来回切换，缓存命中率随之受损。

## 当前工具面

### 文件操作

| 工具 | 定位 |
|---|---|
| `read_file` | 读取 UTF-8 文件。PDF 默认通过内置的纯 Rust `pdf-extract` 读取器自动提取（无需安装 Poppler）；对分栏较多的 PDF，可在 `~/.config/deepseek/settings.toml` 中设置 `prefer_external_pdftotext = true` 改走外部 `pdftotext -layout` 路线。`pages: "1-5"` 可切分大文档。 |
| `list_dir` | 结构化、感知 gitignore 的目录列表。优先于 `exec_shell("ls")`。 |
| `write_file` | 创建或覆写文件。`.rs`/`.toml`/`.json` 写入前先做 parse 检查：把原本能正常 parse 的文件改坏会被拒绝并给出错误位置。 |
| `edit_file` | 在单个文件内进行搜索替换。比完整重写更省。与 `write_file` 同样的写前 parse 门；rustfmt-clean 的 Rust 文件编辑后会重新规范化。 |
| `apply_patch` | 应用 unified diff。多块（multi-hunk）编辑的正确选择。整个批次在落盘前统一过 parse 门（要么全写要么全不写）。 |
| `fim_edit` | 使用 Fill-in-the-Middle 补全编辑文件：`prefix_anchor` 与 `suffix_anchor` 框定要替换的区域，DeepSeek 的 FIM 端点生成新内容，并应用与 `edit_file` 相同的写前 parse 门。 |
| `retrieve_tool_result` | 读取此前溢出到 `~/.codesmith/tool_outputs/` 的大型工具输出的摘要或切片；使用 `summary`、`head`、`tail`、`lines` 或 `query`，而不是重放整个结果。 |
| `handle_read` | 从活跃工具环境持有的 `var_handle` 载荷中读取有界投影。这是 RLM 会话、子代理转录及其他大型符号载荷的基础。 |

### 搜索

| 工具 | 定位 |
|---|---|
| `grep_files` | 用正则在工作区内搜索文件内容；返回结构化匹配结果 + 上下文行。纯 Rust 实现（`regex` crate），不外部调用 `rg`/`grep`。 |
| `file_search` | 对文件名（而非内容）做模糊匹配。当你大致知道名字时使用。 |
| `symbol_search` | 按名称子串搜索持久的工作区符号索引（函数、结构体、trait、类等）。有索引，对"X 定义在哪"这类问题比 `grep_files` 更快更精确。仅在会话启用代码索引时注册。 |
| `find_references` | 借助同一持久代码索引，返回精确符号名的定义位置及全部词法出现处。比在大仓库上反复 `grep_files` 更快。 |
| `lsp_navigate` | 通过会话的 LSP 服务器做类型感知的 definition / references / hover（1-based 行列定位）——对调用点符号（trait 方法、re-export）的解析比按名搜索的 `symbol_search` 更精确。LSP 未启用或无该语言服务器时 fail-closed。 |
| `web_search` | 默认 DuckDuckGo，Bing 兜底；可在配置中选择 Bing、Tavily、Bocha、Metaso、Baidu 以及 `provider_native`（当前 Anthropic 兼容 provider 自带的服务端搜索）。返回排序后的摘要片段 + 用于引用的 `ref_id`。 |
| `fetch_url` | 对已知 URL 直接发起 HTTP GET。当链接已知时比 `web_search` 更快。默认将 HTML 剥离为纯文本。 |
| `web.run` | 浏览网页（search/open/click/find/screenshot/image_query），返回带 `ref_id`（用于引用）的结构化结果。 |
| `finance` | 通过 Yahoo Finance 风格的公共端点获取股票、ETF 或加密货币的实时行情。 |

### Shell

| 工具 | 定位 |
|---|---|
| `exec_shell` | 运行 shell 命令。前台运行可取消，但只应将其用于有界的命令；超时会杀死进程并返回后台重跑提示。 |
| `exec_shell_wait` | 轮询后台任务以获取增量输出。取消当前回合会停止等待，但不会杀死任务。 |
| `exec_shell_interact` | 向运行中的后台任务发送 stdin 并读取增量输出。 |
| `exec_shell_cancel` | 按 id 取消一个运行中的后台 shell 任务，或在被明确要求时取消全部运行中的后台 shell 任务。 |
| `task_shell_start` | 在后台启动一个长时间运行的命令并立即返回。对于可能运行数分钟的诊断、测试、搜索和服务器，优先于前台 shell 使用。 |
| `task_shell_wait` | 轮询后台命令。如果在完成后提供了 `gate`，则在活跃的持久任务上记录结构化的门控（gate）证据。 |
| `exec_wait` / `exec_interact` | `exec_shell_wait` / `exec_shell_interact` 的旧版别名，为兼容旧提示而保留注册。 |

当前台 shell 命令超时时，进程不会被静默延续。工具结果会告知模型用
`task_shell_start` 或 `background = true` 的 `exec_shell` 重跑长任务，然后用
`task_shell_wait` 或 `exec_shell_wait` 轮询。

交互式 shell 任务也可以通过 `/jobs` 查看：它与
`exec_shell`/`task_shell_start` 使用同一个 shell 管理器，以简单列表
显示每个运行中任务的 id、已运行时间和命令。它没有子命令——查看、
stdin 和取消都通过 shell 工具（`exec_shell_wait`、
`exec_shell_interact`、`exec_shell_cancel`）完成。

Shell 权限策略由 `crates/execpolicy` 评估。Deny 前缀先于 trusted 前缀
检查，并且无论处于哪一层都会阻断匹配的命令。Trusted 前缀只在允许信任
快捷方式的模式下才跳过审批。类型化的 ask 记录目前是一个狭窄的基础：
当其在 `AskForApproval::Never` 下匹配时，命令会被拒绝，因为运行时无法
询问用户；其余现有的 allow/deny 行为保持不变。

### MCP 管理器发现

MCP 服务器配置通过 REPL 中的 `/mcp` 和 `/config` 中的 `mcp_config_path`
行呈现。`/mcp` 显示解析后的配置路径、服务器启用/禁用状态、传输方式、
命令或 URL、超时、连接错误，以及发现的工具/资源/提示。它支持一组狭窄的
管理器动作：init、add、enable、disable、remove、validate 和
reload/reconnect。配置编辑会立即写入，但编辑后对模型可见的 MCP 工具池
需要重启才能生效。发现的工具使用向模型展示的运行时名称，例如
`mcp__<server>__<tool>`。

### Git / 诊断 / 测试

| 工具 | 定位 |
|---|---|
| `git_status` | 无需运行 shell 即可查看仓库状态。 |
| `git_diff` | 查看工作树或暂存区 diff。 |
| `git_log` | 运行 `git log`，支持可选的路径与作者/日期过滤。 |
| `git_show` | 对特定修订运行 `git show`，可选补丁与统计。 |
| `git_blame` | 对文件运行 `git blame`，可选修订与行范围控制。 |
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
| `task_stop` | 面向后台任务、工作者、队友、shell 作业和持久任务的统一停止工具——接受统一后台任务 id、代理 id/会话名、队友名、shell 任务 id 和持久任务 id。需要审批。 |
| `checklist_write` | 在活跃线程/任务下的细粒度进度。清单状态从属于持久任务。 |
| `checklist_add` / `checklist_update` / `checklist_list` | 单条清单项操作。 |
| `todo_write` / `todo_add` / `todo_update` / `todo_list` | 清单工具的兼容别名。现有会话可继续工作，但新提示应使用 `checklist_*`。 |
| `note` | 记录一条供以后使用的重要事实。 |

### 会话运行时状态

| 工具 | 定位 |
|---|---|
| `task_create_v2` / `task_update_v2` / `task_get_v2` / `task_list_v2` | 结构化的 Task V2 记录，带主题、描述、依赖和状态（`pending` → `in_progress` → `completed`）；跨回合持久。 |
| `create_goal` / `get_goal` / `update_goal` | 运行时目标生命周期：创建或替换持久目标、查看其状态（目标、状态、token 预算、证据、阻塞项），以及更新目标——`update_goal` 是 LLM-as-judge 完成门控。 |
| `enter_plan_mode` / `exit_plan_mode` / `write_plan_file` | 计划模式生命周期：进入只读的调研/设计模式、写入计划文件（计划模式下唯一可写的文件）、经用户批准后退出。 |
| `enter_worktree` / `exit_worktree` | 创建隔离的 git worktree 并把会话切换进去；退出时恢复原目录（保留或删除 worktree）。需要审批。 |

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

### 代理团队

仅在启用 `agent_teams` 特性开关时注册。

| 工具 | 定位 |
|---|---|
| `team_create` | 创建一个用于协调多个代理的团队：团队配置文件、共享任务目录，当前会话注册为队长。每个队长会话仅一个团队。 |
| `team_delete` | 移除当前团队及其任务目录；删除前校验没有活跃队友残留。 |
| `send_message` | 通过基于文件的邮箱向队友发消息——纯文本、广播（`to: "*"`）或结构化协议消息（关机、计划审批、权限请求）。 |

### 代码审查与召回

| 工具 | 定位 |
|---|---|
| `review` | 对文件、git diff 或 GitHub pull request 运行结构化代码审查。 |
| `recall_archive` | 在先前的上下文周期中搜索当前简报里没有的内容。请节制使用——频繁召回说明简报太稀疏。 |
| `search_sessions` | 按内容(BM25)搜索本工作区的历史会话转录,返回带会话 id 与标题的匹配摘录。懒扫描、无索引;仅限同工作区会话。 |
| `knowledge_recall` | 按关键字搜索 Knowledge On Demand 记忆文件。仅在启用 KoD 时注册。 |

### 文档与媒体

| 工具 | 定位 |
|---|---|
| `pandoc_convert` | 通过本地 `pandoc` 二进制在格式之间转换文档；仅在 `pandoc` 存在时注册。精选的格式白名单、结构化错误——无需 shell-out。 |
| `image_ocr` | 通过本地 OCR（macOS 为 Vision 框架，其他平台为 tesseract）从图像（PNG/JPEG/TIFF）提取文本。仅在存在 OCR 后端时注册。 |
| `image_analyze` | 使用配置的 `[vision_model]` 分析图像（PNG、JPEG、GIF、WebP、BMP）。仅在配置了视觉模型时注册。 |

### 工作区实用工具

| 工具 | 定位 |
|---|---|
| `project_map` | 高层级项目结构图：关键文件加树状视图。 |
| `validate_data` | 校验来自内联输入或工作区文件的 JSON 或 TOML。 |
| `load_skill` | 将技能的 SKILL.md 正文加伴随文件列表载入下一回合上下文——一次调用即可，无需对系统提示 `## Skills` 一节中的路径做 `read_file` + `list_dir`。 |
| `manage_mods` | 列出、编写并激活 CodeSmith 脚本 mod（Rhai）；`list` 另附已解析策略层（种类 → 生效值 → 获胜 mod id）；`session_write` 用内联源码绑定当会话的临时实验 mod（同内容重写免复审）。仅在 mod 层可用时注册；激活与文件写入按动作审批。 |
| `request_user_input` | 向用户提出 1–3 个简短问题并返回其选择。 |
| `revert_turn` | 把工作树回滚到某个较早回合之前的快照（`turn_offset` 从 1 起计，最多 50）；不改动对话历史。需要审批。 |
| `notify` | 通过 `[notifications]` 配置管线发送单条桌面通知（OSC 9 / 终端响铃）。请节制使用。 |

### 残次账本（Slop ledger）

对未解决的架构残留（兼容垫片、未迁移调用方、重复概念、陈旧文档/测试、疑似死代码）的持久跟踪。首次 append 时自动创建账本 JSON 文件。

| 工具 | 定位 |
|---|---|
| `slop_ledger_append` | 向账本追加一条或多条条目。 |
| `slop_ledger_query` | 按桶、严重性、状态或文本搜索过滤条目。 |
| `slop_ledger_update` | 更新条目状态（已解决、已接受、进行中）。 |
| `slop_ledger_export` | 将账本导出为 Markdown 报告，用于交接或 GitHub issue。 |

### 子代理

v0.8.33 开始将大型工具输出迁移到符号句柄：工具返回小型 `var_handle`
对象，`handle_read` 从底层环境中获取有界切片、计数或 JSON 投影。这样既
保持父转录紧凑，又保留了通向完整载荷的恢复路径。

当前面向模型的子代理工具面是持久化的：

| 工具 | 定位 |
|---|---|
| `agent_open` | 打开一个命名子代理会话以进行独立工作。立即返回一个会话投影，父代理可继续协调。 |
| `agent_eval` | 发送后续输入、阻塞等待完成，或获取现有会话的当前投影/转录句柄。 |
| `agent_close` | 按名称或 id 取消或释放一个子代理会话。 |
| `tool_agent` | 打开实验性的快速通道执行代理（Fin）：当前提供方的轻量级、强制关闭思考，用于简单的工具型工作——OCR、文件/搜索查找、抓取、命令探测——让父代理保持规划上下文干净。 |
| `agent_run` | 内联运行一个子代理并等待结果（前台），或在后台派生——前台形式适用于需要立即得到答案的紧凑反馈回路。 |
| `agent_memory_read` / `agent_memory_write` / `agent_memory_edit` | 在子代理的作用域代理记忆目录内读取、创建/覆写和搜索替换文件（通过 `agent_open` 的 `memory` 参数可选启用）。仅对启用了记忆作用域的子代理生效。 |

派生家族（`agent_open`、`agent_run`、`agent_spawn`、`tool_agent`、
`agent_close`）声明自身并发安全，且不再带有自己的审批要求——因此一条助手消息里
的多次派生会同时开启各自的子级，而不是一个子级排在一个弹窗之后。这不是把闸门撤掉：
子级继承父级的审批姿态，非 auto 父级的子级运行在 `permission_mode: "ask"` 下，
所以子级做的任何事都逃不出父级本就要面对的闸门。其他什么能共享同一个并发 batch，
见 [ARCHITECTURE.md](ARCHITECTURE.md)（"工具执行"）里的 batch 规则。

委派协议见 `agent.txt`，角色分类（`general` / `explore` / `plan` /
`review` / `implementer` / `verifier` / `tool_agent` / `team` /
`coordinator_worker` / `custom`）见
[`SUBAGENTS.md`](SUBAGENTS.md)。

`agent_open` 默认开启一个全新的子会话。对于延续式工作或需要继承父级
上下文的多视角评审，传入 `fork_context: true`。在 fork 模式下，运行时
会尽可能逐字节保留父级 prefill/提示前缀，以便复用 DeepSeek 的前缀
缓存，然后再追加子级角色指令和任务。

### Workflow 编排

`workflow` 工具是模型可写的编排：模型写一段 Python 脚本，它跑在一个
一次性沙箱内核上，通过注入（在脚本之前）的辅助函数协调子代理。

| 工具 | 定位 |
|---|---|
| `workflow` | 在一次性内核上运行一段 Python 编排脚本；脚本驱动子代理，其 `finalize(...)` 值作为工具回执返回。 |

脚本 API：`run_agent(objective, *, role=None, model=None, timeout_s=None)`
运行一个子代理至完成并返回 `{"result", "steps", "status"}`；
`agents([{objective, ...}, ...])` 并发扇出一个列表（受 `max_agents` 约
束，按整次运行计总量）；`log(message)` 向运行日志追加一行；
`finalize(value)` 设置运行的返回值。失败的子级以 `status: "failed"`、
错误文本在 `result` 中返回——那是脚本处理的数据，不是异常；超出代理
预算或脚本自身出错则整次运行带 traceback 失败。子级走与 `agent_run`
相同的一次性路径，继承其并发上限、spawn 深度预算、审批天花板与
`SessionBeforeFork` fork 否决。

回执是结构化 JSON：`return_value`（脚本传给 `finalize` 的值）、
`agent_calls`、`elapsed_ms`，以及最近 20 行 `log(...)` 的 `logs_tail`
（`logs_total` 计其余收集到的行）。整次运行有墙钟上限——默认 600s，
`[workflow] timeout_secs` 最高 1800s；到点即杀内核、取消在途子级，
其部分转录不予记录。超过 16 KiB 的回执在字符边界截断并带显式标记。

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

在 `rlm_eval` 内部，被加载的源可通过 `_context` 访问；`content`
也作为兼容别名绑定，因为代理在做 Python 分析时会自然地使用它。
较短的 `context` 和 `ctx` 名称则刻意不绑定，以便用户变量可以
使用它们而不会与引导程序（bootstrap）冲突。

子调用被包装在 RLM 桥的每子调用超时内——默认 120 秒，可按上下文经
`rlm_configure` 的 `sub_query_timeout_secs` 覆盖（钳制到 1..=600）。
辅助函数 `sub_query`、`sub_query_batch`、`sub_query_map` 和
`sub_rlm` 出于对常见代理猜测的兼容仍接受 `timeout_secs` 关键字——
它依旧被忽略（按上下文的设置才是旋钮）。

`finalize(value, confidence=...)` 保留可 JSON 序列化的值。字符串变为
文本句柄；dict、list、数字、布尔和 null 变为 JSON 句柄，`handle_read`
可用 JSONPath 对其进行投影。

### 会话接力

`/relay [focus]` 让当前代理将 `.codesmith/handoff.md` 写成一个紧凑的
`# Session relay` 产物，交给下一个线程。文件名出于与现有提示加载和旧
会话的兼容而保留；可见的心智模型是 relay / 接力。

别名：`/batonpass`。

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

这些一次性的子代理扇出名称不属于当前的
面向模型工具面。
不要在主动指导中使用这些名称：`agent_wait`、
`agent_result`、`agent_send_input`、`agent_assign`、`agent_resume`、
`agent_list`、`spawn_agent`、`delegate_to_agent`、`send_input` 和
`close_agent`。

`agent_spawn` 是唯一的幸存者：它仍注册在工具注册表中
（`AgentSpawnTool`，`crates/cli/src/tools/registry.rs`）作为后台一次性
别名 —— `agent_open` 加 `run_in_background = true` 与之等价 —— 且子代理
通过它递归派生（见 `docs/SUBAGENTS.md`）。新的指导仍应优先使用
`agent_open` / `agent_eval`。

面向模型的 `rlm` 工具面是持久的
`rlm_open` / `rlm_eval` / `rlm_configure` / `rlm_close` 会话形式。

兼容结果可能包含形如下例的 `_deprecation` 块：

```json
{
  "_deprecation": {
    "this_tool": "spawn_agent",
    "use_instead": "agent_spawn",
    "removed_in": "0.8.0",
    "message": "Tool 'spawn_agent' is deprecated; switch to 'agent_spawn' before v0.8.0."
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
```

工具面冒烟测试：

```bash
rg -n '"handle_read"|"rlm_open"|"rlm_eval"|"rlm_configure"|"rlm_close"|"agent_open"|"agent_eval"|"agent_close"' crates/cli/src
rg -n 'handle_read|rlm_open|rlm_eval|rlm_configure|rlm_close|agent_open|agent_eval|agent_close' docs crates/agent-runtime/src/prompts crates/cli/src/tools
```

权威的实时名称为：

- `handle_read`
- `rlm_open`, `rlm_eval`, `rlm_configure`, `rlm_close`
- `agent_open`, `agent_eval`, `agent_close`

主动指导不应宣传一次性名称
`agent_wait`、`agent_result` 或前台 `rlm` 工具（`agent_spawn`
按上文所述保留注册，作为后台/兼容别名）。兼容性代码与
已存储的结果仍可能提及这些名称。

## 为什么我们不提供单一 `bash` 工具

单一 `bash` 代理（Claude Code 的设计）很强大，但会把 shell 脚本的所有
陷阱交给模型：引号、平台分歧、误读 cwd 带来的副作用、`cd` 不在调用间
持久化等。我们的文件工具在转录中的渲染成本也显著更低（结构化的
JSON 形状输出比 `ls -la` 的文字墙坍缩得更好）。

当缺少某样能力时，模型始终可以退回 `exec_shell`。专用工具只是把常见的
80% 从 shell 应急通道上卸下来。
