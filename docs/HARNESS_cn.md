# The Harness：CodeSmith 是什么以及它如何工作

模型回答问题。代理完成任务。差别就在 harness —— 一套由规则、证据与
反馈构成的系统，让模型保持定向，而不是漂移。

CodeSmith 就是这样一个 harness，围绕 DeepSeek V4 构建，由三个理念指引：

| 原则 | 运作方式 |
|---|---|
| **从信任开始** | 每个回合都以"A"开场 —— 可能性先于确定性，匠心先于便利 |
| **清晰的管辖权** | 一部成文宪法，九级权威层级。用户意图高于过时指令。验证高于自信。 |
| **递归式改进** | V4 参与撰写了这个 harness。随着 harness 改进，V4 变得更高效 —— 并反过来进一步改进 harness。每个回合的起点都更强。 |

它是开源的、终端原生的，并以配套的 `codesmith` / `codesmith-tui`
Rust 二进制对的形式发布。

## 管辖权：宪法

代理式模型要大规模处理相互冲突的信息：用户意图、项目规则、系统默认值、
工具输出和过时记忆在同一个回合里争夺权威。LLM-as-a-judge 需要管辖权 ——
当各来源不一致时，究竟谁说了算？

CodeSmith 用一部**宪法**（`prompts/base.md`）来回答这个问题。它是一个
形式化的法律层级 —— Article VII 把九个来源从宪法自身的条文一路排到
先前会话的交接。用户当前的消息高于过时的项目指令。实时工具输出高于
假设。验证高于自信。模型每个回合都继承一条清晰的权威链，从不需要
猜测该遵循哪条指令。

七条条文位于该层级之上，定义了模型的身份、职责与能动性：一条验证义务
（Article V —— 每个动作都留下证据，绝不凭信念宣布成功）、一份协作遗产
（Article VI —— 让工作区对下一个智能保持可读），以及一条真实性至上
条款（Article II —— 任何更低层级的规则都不得凌驾于它）。

## 前缀缓存让它切实可行

DeepSeek V4 的前缀缓存让这一切切实可行。宪法很长、很细，但一旦缓存，
每回合的成本大约比冷读低 100 倍。模型递归地引用它 —— 通过 RLM 会话
窥视、扫描、查询 —— 按需重访信息，而不是依赖一次记住式的通读。它的
表现更像一场开卷考试，而非闭卷考试。

## 失败即反馈

因为权威结构是显式的，失败不会被藏起来。非零退出码、回合之间送达的
rust-analyzer 类型错误、沙箱拒绝 —— 这些都会作为纠正向量反馈回来。
模型利用自身的漂移进行自我纠正。

## 录制与重放模型调用

设置 `CODESMITH_RECORD_LLM=<path.jsonl>` 会把解析出的 LLM 客户端包进
一个录制器：通过该客户端服务的每一个模型调用（包括共用它的实用模型、
接缝和压缩调用）都会追加一行 JSONL —— 完整的请求信封与实际发出时
完全一致（经过每个扩展变换之后：消息、系统提示词、完整的工具
schema、采样参数），外加流式响应事件或非流式响应。已知缺口：跨
provider 的 `[utility_model]` 客户端单独构建，不会被录制。对于被录制
的会话，任何历史 provider 请求都是日志的纯函数。无效路径会让启动
当场报错中止。

`ReplayClient::load(path)`（位于 `codesmith-agent` 的
`llm_client::record_replay`）把录制的事件通过真实的 turn 循环喂回去 ——
无密钥、无网络的回归测试。重放是严格的 FIFO（不做请求匹配；重试的
调用会额外消耗一行），并逐字重放流，包括缺失终止 `message_stop` 的
情况（引擎将其视为断连，与原始运行的处理完全一致）。引擎测试
`record_then_replay_round_trip` 是现成的工作示例。Fixture 包含完整的
模型 I/O —— 绝不要提交含密钥的 fixture。

### Provider 调试标志（opt-in，默认关闭）

- `CODESMITH_DUMP_400_PAYLOAD=1` 把完整的失败请求转录连同 provider
  错误正文写入 `/tmp/codesmith-400-dump-<pid>-<n>.json`（仅属主可读
  模式；可用 `CODESMITH_DUMP_400_PAYLOAD_DIR` 重定向目录）。转储无
  上限累积 —— 请手动清理。
- `CODESMITH_REASONING_PASSTHROUGH=1` 让通用 `openai` provider 分支
  为接受该字段的第三方 OpenAI 兼容网关（Zhipu GLM、…）逐字转发
  `reasoning_effort`。

## 模式与沙箱

三种模式控制动作空间。Plan 只读。Agent 把破坏性操作挡在审批之后。
YOLO 在受信任工作区中自动批准。操作系统级沙箱按平台强制执行：macOS
Seatbelt、Linux Landlock + seccomp（外加可选的 bubblewrap），以及
Windows Job Object v1。参见 [PRESETS.md](PRESETS.md) 和
[SANDBOX.md](SANDBOX.md)。

模型自动路由（`--model auto`，默认值）是每个回合运行一次的分类器，
跑在 provider 的最强档上 —— 一次极小的关闭思考调用，为本回合选定
重型或轻型模型以及思考级别。一个免费的本地启发式会短路明显的情形；
`[auto] cost_saving = true` 恢复廉价实用模型路由器。Fin —— 快速的
轻量档路径 —— 仍然处理快速工具工作和后台摘要。

每个回合都会在你的仓库 `.git` 之外记录一份 side-git 快照。
`/restore` 和 `revert_turn` 负责回滚工作区。

子代理并发运行（上限 20）。`agent_open` 立即返回；结果以携带摘要的
完成哨兵标记（sentinel）形式内联到达。完整转录通过 `agent_eval` 留在
有界句柄之后。

其余表面：每次编辑后的 LSP 诊断（rust-analyzer、pyright、
typescript-language-server、gopls、clangd、jdtls、
vue-language-server）、用于批量分析的 RLM 会话、MCP 协议、HTTP/SSE
运行时 API、持久任务队列、面向 Zed 的 ACP 适配器、SWE-bench 导出，
以及带缓存命中/未命中拆分的实时成本跟踪。

## 一段话讲清架构

`codesmith`（调度 CLI）→ `codesmith-tui`（伴生二进制）→ ratatui
界面 ↔ 异步引擎 ↔ OpenAI 兼容流式客户端。工具调用经由一个类型化
注册表（shell、文件操作、git、web、子代理、MCP、RLM）路由，结果
流回转录。引擎管理会话状态、回合跟踪、持久任务队列，以及一个 LSP
子系统 —— 它在下一个推理步骤之前把编辑后诊断喂入模型的上下文。

完整讲解见 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 子代理：并发后台执行

CodeSmith 可以派发多个并行运行的子代理 —— 就像一个并发任务队列：

- **非阻塞启动。** `agent_open` 立即返回。子代理获得自己全新的上下文
  和工具注册表，独立运行。父代理继续自己的工作。
- **后台执行。** 子代理并发执行（默认上限：10，可配置到 20）。引擎
  管理这个池 —— 无需轮询循环。
- **完成通知。** 子代理完成时，运行时会向父代理的转录注入一个
  `<codesmith:subagent.done>` 哨兵标记。人类可读的摘要 —— 包括
  子代理的发现、改动的文件以及任何风险 —— 位于紧邻哨兵标记的上一
  行。父模型读取该摘要并整合发现，无需额外的工具调用。
- **有界的结果检索。** 完整的子代理转录留在 `transcript_handle`
  之后，通过 `agent_eval` 访问。当摘要不够用时，父代理调用
  `handle_read` 获取切片、行范围或 JSONPath 投影 —— 既让父代理
  上下文保持精简，又不失去对细节的访问。

完整的子代理参考见 [SUBAGENTS.md](SUBAGENTS.md)。
