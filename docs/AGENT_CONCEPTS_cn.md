# 通用 Agent 概念图谱 —— 落到代码的事件流词汇

一篇流传较广的通用 Agent harness 文章（讲的是 DeepSeekHarness，即 DSH）把
harness 描述为围绕同一条事件流做的层次切分，各概念各司其职，而不是一条龙
流水线：

> Source 把外部世界翻译成事件；Thread 把事件按线索归拢；Eval 做一次接单
> 判断，Policy 给它尺子；Ticket 让工作有身份，Mandate 给它边界；
> Subscription 让上下文自动流动；Assignment 与 Executor 负责真正执行；
> Channel 把结果送回人群。

本图谱把每个概念钉到本仓库中已经拥有它的代码。它是设计讨论与评审的共享
词汇 —— **不是改名计划**。十个词里有五个在本仓库已指别的东西
（协议/runtime API 里的 `Thread*`、五个模块里的 `Policy*`、四种含义的
`Eval`、引擎里遍布的 `*Executor*`、协议里的 `Channel`），引入文章命名会
给一个代码库留下两套词汇。概念没有着落的地方，下面会点名空地；目前唯一
补上的是 Source 的帧目录
（`crates/agent-runtime/src/engine/runtime_event.rs`）。

约束这些 seam 未来工作的边界规则见
[ARCHITECTURE_cn.md](ARCHITECTURE_cn.md)（边界说明）与 `AGENTS.md`：单一
循环、单一收件箱、seam 三元组、prefix-cache 纪律。

## 映射总表

| 概念 | 本仓库的归属 |
|---|---|
| Source | 各入口机制 + runtime-event 帧目录 |
| Thread | runtime-API 线程 + 会话日志 |
| Eval | 接单判断，刻意分布在三处 |
| Policy | 这些判断所依据的尺子 |
| Ticket | 命名子会话 + 团队任务项 |
| Mandate | 骑在子代理工具面上的委托信任包络 |
| Subscription | 部分：子→父上下文补丁；跨会话 recall 是空地 |
| Assignment | 子代理运行时与团队任务流 |
| Executor | turn-executor 注册表（重叠最强） |
| Channel | 事件通道、mailbox、runtime API、桌面桥 |

### Source —— 外部世界变成事件

没有一个统一的 Source 抽象——这是累积的结果而非决定。每条入口路径都已
自名：

- 宿主操作：`Op`（`crates/agent-runtime/src/ops.rs`）—— 引擎的入口词
  汇；REPL 输入、runtime-API 提示、合成回合。
- 完成事件：`SubAgentCompletionSender`
  （`crates/agent-runtime/src/subagent.rs`）—— 子代理与后台 shell 任务
  完成，附带 idle-wake 的旁路 `Notify`。
- Steer：`rx_steer`（`crates/agent-runtime/src/engine/turn/seams.rs`）。
- 团队收件箱：基于文件的 poller
  （`crates/cli/src/tools/team/inbox_poller.rs`），经
  `Op::TeamInboxDispatch` 分发
  （`crates/agent-runtime/src/engine/team_inbox.rs`）。
- MCP：客户端池（`crates/mcp-client`）以工具而非事件的形式出现。

缺的 —— 现已补上 —— 是"注入的*内部*输入长什么样"的目录：每条引擎注入
的哨兵 user 消息都骑在 `<codesmith:runtime_event>` 信封上，封闭的 kind 集
合与唯一的帧构建器在
`crates/agent-runtime/src/engine/runtime_event.rs`（`subagent_completion`、
`shell_job_completion`、`result_verification`、`plan_mode`、
`plan_mode_exit`、`deliverables_check`、`idle_wake`）。
`<teammate-message>` 与 `<turn_meta>` 刻意保持独立信封（见该模块的已知限
制）。

### Thread —— 事件按线索归拢

Runtime-API 线程（`crates/protocol/src/lib.rs` 的 `Thread*`、
`crates/cli/src/runtime_threads.rs`）：每个会话一个引擎加一条 per-thread
JSONL 事件流 —— Codex 式生命周期语义，持久化的权威记录。注意撞名：它们是
*会话*，不是通用 Agent 那种按线索归拢的事件线程；本仓库没有任何按线索分组事件
的机制，也没有规划。每条线程之下的持久会话记录是会话日志
（`crates/session/src/journal.rs`）。

### Eval —— 接单判断

接单判断在三个门上，各有各的问题：

- 回合接单：收件箱投递规则
  （`crates/agent-runtime/src/engine/inbox.rs` —— Target × Delivery）决定
  停留的输入*何时*到达循环；由执行器的 pre-request drain 投递。
- 子代理的工具级接单：`DelegationPolicy::admit`
  （`crates/cli/src/tools/subagent/mod.rs`）—— 白名单、审批层级、terminal
  接管拒绝、越出工作区的路径问题路由给父代理。
- 宿主的动作级接单：审批门
  （`crates/agent-runtime/src/engine/turn/approval.rs`）与沙箱执行策略
  （`crates/sandbox/src/execpolicy/`）。

模型面工具 `agent_eval` 是该词的另一种含义：取回或等待子会话的结果，不是
接单判断。

### Policy —— 尺子

上述 Eval 所依据的尺子：`DelegationPolicy`（信任包络：`allowed_tools`、
`auto_approve`、路径询问路由）、`PolicyStore`
（`crates/agent/src/policy.rs` —— 可脚本化的宿主行为旋钮）、沙箱
execpolicy 编译器、以及沙箱模式策略
（`crates/agent-runtime/src/engine/sandbox_mode_policy.rs`）。

### Ticket —— 工作有身份

经 `agent_open` 打开的命名子会话
（`crates/cli/src/tools/subagent/`）、`SubAgentAssignment { objective, role }`
记录（`crates/agent-runtime/src/subagent.rs`）、团队任务项
（`TaskAssignmentEntry`，`crates/cli/src/tools/team/inbox_poller.rs`）。

### Mandate —— 工作有边界

边界骑在子代理的工具面上，从不在循环里：`DelegationPolicy` 收窄包络
（`allowed_tools`、审批上限、路径询问），`ChildTurnSpec`
（`crates/agent-runtime/src/engine/child_turn.rs`）以纯数据携带它 ——
messages 是任务，tools 是收窄后的面。"委托收窄包络，从不改循环。"

### Subscription —— 上下文自动流动

部分存在。现状是 push-at-assembly：子→父的
`SubAgentCompletion.context_patch`（只收紧）、事实账本
（`crates/session/src/fact_ledger.rs`）、agent 记忆
（`crates/context/src/agent_memory/`）—— 都在无需模型调用的情况下把上下文
送进下一次组装。**空地：** 通用 Agent 的 session-reference（跨会话 recall 自动注
入）在这里没有对应物。未来的切片必须守住 prefix-cache 纪律：易变事实以
user-role 消息进入，绝不进冻结前缀。

### Assignment —— 工作有主

子代理运行时：`SubAgentManager` / `run_subagent_task`
（`crates/cli/src/tools/subagent/mod.rs`）构建 `ChildTurnSpec` 并运行
`run_child_turn`；外加团队任务分配流（`crates/cli/src/tools/team/`）。

### Executor —— 工作被执行

重叠最强、且是刻意采纳的一块（founder 决定 2026-10-30，通用 Agent
kernel-composition 模型）：每个回合 —— 主回合与子回合 alike —— 从进程共享
的 `TurnExecutorRegistry`
（`crates/agent-runtime/src/engine/executor_composition.rs`）解析自己的工
厂，`HostAgentExecutor` 是生产循环，`rhai` 注册表项承载脚本循环，
`TurnLease`（`crates/agent-runtime/src/turn_lease.rs`）是结构所有权，
`verify-single-turn-loop.py` 守护单循环契约。见
[ARCHITECTURE_cn.md](ARCHITECTURE_cn.md) 的边界说明。

### Channel —— 结果送回人群

UI 事件通道（`crates/agent-runtime/src/events.rs`）、spawn 树 `Mailbox`
（`crates/agent-runtime/src/mailbox.rs`）与 `ChildEventSink`、runtime API
线程（HTTP）、桌面应用的 loopback 桥（[DESKTOP_cn.md](DESKTOP_cn.md)）。

## 刻意不采纳

- **不把文章词汇用作类型名。** 映射只存在于本文档；`Thread`、`Policy`、
  `Eval`、`Executor`、`Channel` 在这里已指别的东西，新代码沿用仓库命名
  （`RuntimeEventKind`、`DelegationPolicy`、……）。
- **不加第二个收件箱或循环。** `engine/inbox.rs` 是既有机制之上的词汇；
  在它们旁边再立一个队列就是"戴帽子的第二个收件箱"。
- **不做可寻址的团队/工作流收件箱**（通用 Agent 式 per-agent 投递）—— 收件箱
  seam 上的未来工作，不是现有词汇。
- **不开新的日志洞：** 子代理转写继续无日志（已记录的限制，
  `ARCHITECTURE.md`）；注入帧因为帧文本本身就是记录而被记下
  （"model-visible means logged"）。
