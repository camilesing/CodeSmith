# 子代理（Sub-Agents）

子代理是代理循环的持久化后台实例。父代理通过一个聚焦的任务打开子代理，
立即获得一个 `agent_id` 和会话名称，并在子代理运行至完成期间继续自己的
工作。子代理默认继承父代理的工具面。`agent_open` 以分离式后台任务的
方式启动它们：取消父代理回合会停止父代理的等待/求值路径，但不会杀死
已经打开的子会话。使用 `agent_close` 可以显式取消一个正在运行的子代理。

本文档介绍角色分类体系。当前的活动编排接口是
`agent_open`、`agent_eval` 和 `agent_close`；注册表还在它们旁边
注册了 `tool_agent` 和 `agent_run`。参见 `prompts/base.md`
的"Sub-Agent Strategy"以及内联的工具描述。

这三个会话工具保持常驻：它们没有声明延迟，所以 `agent_open` → `agent_eval`
→ `agent_close` 始终随请求的工具清单下发——即使 `[tools].progressive_disclosure`
把声明延迟的长尾收起，照提示词行事的模型也不会为它们多花一次发现往返。
延迟是**逐工具**决定的（`ToolSpec::defer_loading`，声明在工具自己的定义旁边），
不再由某份中心常驻清单反推。

## 角色分类体系

`agent_open` 的 `type` 字段为子代理选择一种系统提示词姿态
（`agent_type` 被接受为兼容性别名）。每个角色对工作而言都是一种独立的
姿态——而不仅仅是一个不同的标签。

| 角色          | 姿态                                   | 可写?   | Shell 姿态    | 典型用途                                       |
|---------------|----------------------------------------|---------|---------------|----------------------------------------------|
| `general`     | 灵活；执行父代理的一切指示             | 是      | 是            | 默认角色；多步骤任务                          |
| `explore`     | 只读；快速梳理相关代码                 | 否      | 只读          | "找出 `Foo` 的所有调用点"                     |
| `plan`        | 分析并产出策略                         | 极少    | 极少          | "设计迁移方案；不要执行"                      |
| `review`      | 阅读并评分，附带严重性打分             | 否      | 只读          | "审计这个 PR 的 bug"                          |
| `implementer` | 以最小改动落地一个具体变更             | 是      | 是            | "重写 `bar.rs::Foo::bar` 以实现 X"            |
| `verifier`    | 运行测试 / 验证，报告结果              | 否      | 以测试为主    | "运行 cargo test --workspace 并报告"          |
| `tool_agent`  | 在轻量级模型上快速执行，关闭思考        | 是      | 是            | 简单的工具型工作：OCR、文件/搜索查找、抓取、命令探测 |
| `team`        | 共享任务清单；通过基于文件的邮箱通信   | 是      | 是            | agent-teams 队友                              |
| `coordinator_worker` | 独立执行被分配的任务；完整工具减去团队管理 | 是 | 是        | 由协调者派发的工作者                          |
| `custom`      | 显式的窄工具白名单                     | 视情况  | 视情况        | 使用精选工具的受控派发                        |

每个角色的完整系统提示词位于
`crates/agent-runtime/src/subagent.rs`（`SubAgentType::system_prompt()`；
每个子代理的合成提示词由 `crates/cli/src/tools/subagent/mod.rs` 中的
`build_subagent_system_prompt` 构建）。提示词前缀在子代理启动时自动加载；父代理的
任务分配提示词会成为第一轮的用户消息。

## 只读角色与 Plan 模式

`explore` 是**强制**只读，而不只是提示词那样写：子代理的工具列表会与其自身注册表中
声明 `ToolCapability::ReadOnly` 的工具名求交，因此无论以何种方式打开，该类型都无法写文件、
打补丁或执行 shell。在 `allowed_tools` 里要求写工具会让 spawn 直接失败并列出被拒的工具名，
而不是悄悄收窄后继续运行。求交同时会去掉 spawn 家族（`agent_open` 等），所以 explore
子代理是叶子节点——请并行开 2–3 个，而不是嵌套。

Plan 模式注册同一套子代理工具（该模式的提示词本身就要求做只读调查），并把**每个**子代理
都标记为只读，不论类型：该模式既不能写也不能执行，它派出的调查者同理。Plan 模式的子代理
同样因此成为叶子节点。

## Coordinator 模式（Router/Worker）

Coordinator 模式（`/mode coordinator`；`coordinator_mode` feature flag——`experiment`
预设默认开启——会把它加进 Shift+Tab 环）把会话变成 Router：只负责派发与综合，
不持有任何直接执行工具；它打开的每个后台 worker 都会自动获得一张 **ticket**——
这块被委派工作的持久身份，以其 worker 的 agent id 为键。设计沿用 DSH super-agent
原型的拆分：路由是结构约束，不是提示词建议。

- **默认 worker 类型**：coordinator 模式下不指定类型的 spawn 会成为
  `coordinator_worker`（排除团队/消息工具，worker 提示词强调 mandate）。显式指定的
  角色保持不变。
- **worker 沙箱**：worker 使用 Agent 模式的沙箱策略（工作区可写 + 网络）——
  coordinator 自身的只读姿态只约束 Router，绝不传导给 worker。
- **结构化汇报**：coordinator worker 的工具面带 `ticket_report`
  （`progress | waiting | ready | blocked | failed | dropped`，可选 `question`）。
  `blocked` 或带提问会立即唤醒 coordinator（工作继续）；其余状态随 worker 自然
  结束的 completion 卡交付。
- **看板**：coordinator 读 `ticket_list`（人优先排序：awaiting-answer →
  awaiting-review → blocked → failed → working → idle）与 `ticket_read`
  （完整 scope、汇报、attention、inputs）；每个 turn 的首个请求还会以
  `ticket_board` runtime event 附带一行 open-ticket 摘要。stage 由汇报派生——
  从不从自由文本猜。
- **attention**：worker 的提问原样转述；用户的回答经 `agent_eval` 的 `message`
  参数回流，同时记为 ticket 的 input 并解除 attention。
- **mandate 守卫**：`ticket_close` 关单后，worker 的后续工具调用全部被拒
  （"Ticket ended; new work needs a new ticket"）。失败运行自动关单；
  `agent_close`/取消按 stopped 关单；`dropped` 汇报静默关单。`ready` 汇报
  等待审查——coordinator 综合答复用户后再关单。
- **Trigger**（实验性，默认关）：`[coordinator.trigger]` 按间隔轮询 GitHub
  通知，代码层过滤（reason 白名单、可选 repo 白名单、去重），命中的候选以
  `[github trigger]` 卡片送入 coordinator 会话。coordinator 自身即 slow path：
  attach、开新工作，或忽略。

已知限制：一个 worker 一张 ticket（不按用户请求分组）；worker 不可跨重启续跑，
上一会话的 ticket 显示 `idle`，需手动关闭或重新派发；ticket 的 scope 由提示词
约束而非机器校验（代码只强制关单后的工具拒绝）；trigger 轮询仅在交互式会话中
运行。

### 跨仓库协作

`agent_open` 的 `cwd` 可以位于父代理工作区内，**或者**位于你用 `/trust add <path>` 授权过
的目录下。子代理的工作区根即该目录，它自己的路径校验也以此为准。要同时勘探两个仓库，
先授权第二个仓库，然后在一条消息里各开一个 explore 子代理：

```text
/trust add ../other-repo
agent_open {name: "repo-a", cwd: "/abs/repo-a",     agent_type: "Explore", prompt: "…"}
agent_open {name: "repo-b", cwd: "/abs/other-repo", agent_type: "Explore", prompt: "…"}
```

工作区之外且未授权的 cwd 会被拒绝，并在错误里给出需要执行的 `/trust add <path>` 命令，
因此写错的路径不会变成对无关目录的静默读取。两次 spawn 依次经过审批；第一次答 `[a]`
即覆盖本次运行里之后每一次 `agent_open`，第二次不再询问。每次都立即返回，
所以两个子代理随后并发运行。

子代理自己的文件工具同样受此约束，而它够不到的路径会变成一个**问题**而不是死路：
子代理把路径访问提示发到派发它的那个会话里，你回答一次，授权即写入父子共读的信任列表。
`[a]` 会为该工作区记住这条路径（与 `/trust add <path>` 写的是同一份记录），这是让双仓库
勘探持久可用的方式。若 REPL 正停在提示符上、没有回合在运行，子代理的提问会在五分钟后
超时，路径保持未授权——而不是把子代理挂死。

## 上下文分叉

`agent_open` 默认全新启动：子代理获得其角色提示词加上你传入的
任务。当子代理应当从父代理当前的请求前缀继续时，使用
`fork_context: true`。在分叉模式下，运行时会在可用的情况下保持
父代理的 prefill/提示词前缀逐字节一致，然后追加一份结构化的
状态快照，最后在末尾加上子代理的角色指令和任务。这样既保留了
DeepSeek 前缀缓存复用的能力，又给子代理提供了继续、审查、总结或
压缩工作所需的上下文。

独立探索请使用全新会话。当任务依赖于父代理转录中已有的决策、文件、
todo 或计划状态时，使用分叉会话。

### 如何选择角色

- **`general`** —— 当任务是"完成这整件事"，而不是"去看看"、"设计"
  或"验证"时。这是正确的默认选择；只有当姿态确实重要时才选择更
  具体的角色。
- **`explore`** —— 当父代理在决定下一步之前需要证据时。探索者
  便宜且快速；可以在互不重叠的区域并行打开 2–3 个。
  它们应先定位方向：确认项目根目录，在陌生的代码树中阅读相关的
  `AGENTS.md`/`README.md` 指引，只搜索可能相关的范围，并返回
  `path:line-range` 证据而非叙述性导览。可使用的角色名为 `explore`
  或 `explorer`。
- **`plan`** —— 当父代理有目标但还没有可执行的分解时。计划者会
  写入工件（`update_plan` 行、`checklist_write` 条目）但不会执行
  它们。
- **`review`** —— 当已经有变更且父代理希望对其评分时。审查者不做
  修补——它们在发现项中描述修复方案，这样当结论是"需要修复"时，
  父代理可以派发一个 Implementer。
- **`implementer`** —— 当变更已经明确、只需落地时。实现者保持严格
  的范围约束：最小改动、不做顺手重构、交回前运行快速验证。
- **`verifier`** —— 当父代理需要测试套件或其他验证的权威通过/失败
  结论时。验证者不修复失败；它们记录失败的断言 + 堆栈，并把修复
  候选方案放在 RISKS 之下。
- **`custom`** —— 仅当父代理需要显式约束工具集时使用。通过
  `agent_open` 的 `allowed_tools` 字段传入白名单。

### 别名

模型可以用多种方式拼写每个角色：

| 规范名        | 别名                                                             |
|---------------|------------------------------------------------------------------|
| `general`     | `worker`, `default`, `general-purpose`                           |
| `explore`     | `explorer`, `exploration`                                        |
| `plan`        | `planning`, `awaiter`                                            |
| `review`      | `reviewer`, `code-review`                                        |
| `implementer` | `implement`, `implementation`, `builder`                         |
| `verifier`    | `verify`, `verification`, `validator`, `tester`                  |
| `tool_agent`  | `tool-agent`, `toolagent`, `executor`, `execution`, `fin`        |
| `team`        | `teammate`, `swarm`                                              |
| `coordinator_worker` | `coordinator-worker`, `coord_worker`                      |
| `custom`      | （无；需要显式的 `allowed_tools` 数组）                          |

所有匹配均不区分大小写。未知值会产生一个列出可接受集合的
类型化错误，因此模型可以在下一轮自行纠正。

## 工具继承与权限收窄

子代理的工具面是**其父代理有效工具的子集**——子代理永远无法调用
父代理未暴露的工具（这是 CodeSmith 对 Claude Code 的
`restrictToSubset(parentContext.toolPermissionContext)` 的对应实现）。
这一点在任意派生深度都成立：

- 顶层父代理（引擎）暴露完整的代理工具面，因此其直接子代理也继承
  完整的工具面——包括 `agent_spawn`，所以递归派生得以保留。
- 一个**被收窄的**子代理（带显式 `allowed_tools` 列表的 `custom`
  角色）只暴露该列表。它的孙代理继承这个窄集合而不会重新扩展回
  完整工具面，因此受控派发无法通过派生一个工具更广泛的子代理来
  提权。
- 子代理显式请求的 `allowed_tools` 会先与父代理的有效集合求交集
  再生效——请求父代理缺少的工具会被静默丢弃，而不会被授予。

注册表执行守卫仍然会阻止需要审批的工具（`write_file`、
`exec_shell` 等），除非父运行时已被自动批准或该角色被显式赋予
写权限（`implementer`、`custom`），因此收窄是在审批门控之上
分层的——而不是替代它。

### `[subagents].inherit_full_registry` 逃生舱口

在 `~/.codesmith/config.toml` 中设置
`[subagents] inherit_full_registry = true`
（默认 `false`）可以让每个子代理都继承完整的
代理工具面，无论父代理的有效集合是什么。仅对确实需要
无限制工具面的流程使用此选项；默认姿态是子集。

## 并发上限

调度器默认将并发子代理数量限制为 10
（可通过 `~/.codesmith/config.toml` 中的 `[subagents].max_concurrent`
配置，硬上限为 20）。当父代理达到上限时，`agent_open` 会返回
一个包含上限值的错误；父代理应先使用 `agent_eval` 等待一个
正在运行的代理完成，或使用 `agent_close` 取消一个正在运行的代理，
然后再重试。

该上限只统计**正在运行**的代理——已完成 / 失败 /
已取消的记录会保留以供检查，但不占用槽位。丢失了 `task_handle`
的代理（例如跨进程重启后）也不计入上限。

## 流式 idle 看门狗

子代理与主 turn 跑同一个 turn 循环（所配置的 `turn_executor`）：它的
模型调用是流式的，一个在看门狗窗口内没有任何事件的流会被重试、然后
让子代理失败，这样卡住的连接不会无限期占住父代理的完成唤醒通道。
持续输出的长回答永远不会触发它——只有静默才会。窗口默认 `120` 秒；
对静默间隙较长的 provider，可在 `~/.codesmith/config.toml` 中放宽：

```toml
[subagents]
api_timeout_secs = 900  # 15 minutes; clamped to 1..=1800
```

值会被钳制到 `1..=1800`；`0` 和 `unset` 保持 `120` 秒默认值。每次重试
按会话的 `stream_idle_retry_increment_secs` 放宽窗口，与主 turn 一致。

子代理 turn 的已知限制（刻意如此）：没有压缩与容量门、没有审批提示（其
委托工具面直接拒绝，或经父级 UI 询问路径），也不向它自己的子代理投递
完成项。执行限额与主 turn 一致：委托工具调用没有统一上限（`exec_shell`
经 `timeout_ms` 自管，默认 120 秒、上限 600 秒；取消子代理会终止长调用），
每次回复的 `max_tokens` 按主 turn 相同的方式由 provider 与模型推导
（上限 65 536；`CODESMITH_MAX_OUTPUT_TOKENS` 覆盖同样生效）。
`agent_send_input` 的消息在子代理下一步边界按序投递（`interrupt` 为兼容
保留，不再丢弃此前排队的输入）。

## 生命周期

每个打开的会话都会产生一条记录，其状态为以下之一：

```
Running → (Completed | Failed(reason) | Cancelled | Interrupted(reason))
```

当管理器检测到一个 `Running` 代理的任务句柄已丢失时，会触发
`Interrupted`——通常发生在进程重启并从
`.codesmith/state/subagents.v1.json` 加载工作区持久化状态之后。
父代理可以用同样的任务打开一个替代会话，或将其视为终态。

### 会话边界

每个 `SubAgentManager` 实例在构造时为自己分配一个新的
`session_boot_id`。每个新会话都会给代理打上该 id；工作区
状态文件会记录它以用于重启恢复。

`agent_eval` 和侧栏/状态投影默认聚焦于当前会话的代理。不再
运行的前一会话代理被视为归档记录，这样模型就不会把陈旧的
工作误认为正在进行的工作。

从 #405 之前的持久化状态文件（没有 `session_boot_id` 字段）
加载的记录会被归类为前一会话，因为管理器无法将它们与当前
启动匹配。

## 输出契约

每个子代理的最终助手消息都以五个部分收尾，顺序固定，并使用
精确的章节标题作为 Markdown 三级标题（H3）：

```
### SUMMARY    一段话；你做了什么以及核心结论
### EVIDENCE   path:line-range 引用、命令 + 退出码、关键发现；每条一行
### CHANGES    写入或编辑的文件，各附一行说明；只读时写 "None."
### RISKS      可能出问题 / 父代理应复核的地方
### BLOCKERS   阻止你完成的因素；干净收尾时写 "None."
```

确切的格式位于 `crates/agent-runtime/src/prompts/subagent_output_format.md`。
父代理会把 `EVIDENCE` 当作下一轮的工作集，因此探索者和审查者
应在这一部分保持精确。

## 同步结果与报告恢复

前台 `agent_run` 返回子代理的最终助手文本。子代理也可能以工具调用
收尾，此时没有最终文本；`agent_run` 会从已存储的 transcript 中恢复
子代理最后一段助手文本（没有则恢复最后一条工具结果），并说明文本
来源。若 transcript 里两者都没有，工具会返回状态行、`agent_id`，以及
明确的 `agent_eval` / `handle_read` 指引，而不是只回一行"completed"。

结果 metadata 携带 `report_available`（子代理是否产出了最终报告）、
`report_source`（`child_final_text`、`recovered_last_assistant_text`、
`recovered_last_tool_result` 或 `none`）和 `transcript_handle`。超过
50,000 个字符的报告会被截断，截断文案保留 `agent_id`。成功的完成哨兵
携带 `report_available`；当子代理在进程内运行且 transcript 已存储时，
还携带 `transcript_handle`，因此被有界完成尾部截短的摘要行仍留有
恢复路径。前台子代理会在本进程内登记到管理器，`agent_eval` 可按同一
id 解析；该记录仅存在于内存中（transcript handle 随进程结束失效，
因此不持久化）。后台子代理保留原有的持久化生命周期。transcript 载荷
记录子代理自身消息的起始位置，恢复时不会把分叉的父前缀误当成
子代理输出。

## 作用域代理记忆

子代理不继承父代理的记忆文件，也没有 `remember` 工具——该工具
只注册在父代理的注册表上。取而代之，子代理获得可选启用的作用域
代理记忆（scoped Agent Memory）：在 `agent_open` 上传入 `memory`
参数（`user`、`project` 或 `local` 作用域，或
`{"scope": ..., "snapshot": "none" | "initialize" | "prompt-update"}`）。
之后子代理可以通过 `agent_memory_read` / `agent_memory_write` /
`agent_memory_edit` 工具维护自己的 `MEMORY.md` 索引和主题文件，
这些工具被限制在该子代理的记忆目录内——没有通用的工作区写入
权限，也不经过标准的写入审批流程。

## 实现说明

- 源码：`crates/cli/src/tools/subagent/mod.rs`。
- 持久化状态：`<workspace>/.codesmith/state/subagents.v1.json`。Schema
  版本为 `1`（前向兼容——新的可选字段使用
  `#[serde(default)]`）。
- `SubAgentRuntime::background_runtime()` 从 `child_runtime()`
  出发，但把回合范围的子令牌替换为一个新的取消令牌，因此父代理
  回合取消不会停止分离式的后台会话。
- `is_running` 检查会忽略 `task_handle` 为 `None` 的代理；这避免
  了把已持久化但已分离的记录计入并发上限。
- `SharedSubAgentManager` 是 `Arc<RwLock<...>>` —— 读路径使用
  读锁，因此在多代理扇出期间 `/agents` 和侧栏投影不会阻塞
  主循环。
