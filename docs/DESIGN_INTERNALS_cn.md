# 架构 —— 可插拔框架核心

本文档描述 "foundation slice" 重构引入的 **provider 可插拔** 层：CodeSmith
技术栈如何将 LLM 的*抽象*与*实现*分离，以及宿主如何在构建时像搭 Lego
积木一样组装 provider。

更平缓的全代码库概览见 [ARCHITECTURE.md](ARCHITECTURE.md)。
扩展本 slice 的待办工作清单见 [`ROADMAP.md`](../ROADMAP.md)。

## 设计目标

1. **框架核心，LangChain 风格** —— 一小组 trait（`LlmClient`、
   `ProviderFactory`）加上一个任何 provider 都能插入的注册表，核心中
   不编译任何具体客户端。
2. **抽象 / 实现分离，pi-mono 风格** —— 宿主从不指名具体客户端类型；
   它构建一个中立的 `ProviderConfig`，向 `ProviderRegistry` 请求客户端。
   开发者可以通过注册另一个工厂来替换任何实现。
3. **安装时的 Lego 积木** —— provider 住在独立的 `codesmith-providers`
   crate 中、位于 Cargo feature 之后；宿主只引入自己需要的部分。

## Crate 分层

```
                         ┌───────────────────────────┐
                         │ codesmith-config           │  ProviderKind, config TOML
                         │ codesmith-secrets          │  key resolution
                         └─────────────┬─────────────┘
                                       │ dep
              ┌────────────────────────┴────────────────────────┐
              ▼                                                  ▼
┌──────────────────────────────┐                ┌─────────────────────────────┐
│ codesmith-agent (CORE)       │                │ codesmith-providers (IMPLS) │
│  • llm_client::LlmClient     │   traits ─────▶│  • mock (echo, no network)   │
│  • provider::{ProviderId,    │   ◀──── cfg    │  • openai-compat  (ROADMAP) │
│      ProviderConfig,         │     features   │  • anthropic      (ROADMAP) │
│      ProviderFactory,        │                └─────────────────────────────┘
│      ProviderRegistry}       │                            ▲
│  • models, retry             │                            │ path dep
└──────────────┬───────────────┘                            │
               │ path dep                                    │
               ▼                                             │
┌──────────────────────────────┐                            │
│ codesmith-agent-runtime      │                            │
│  • Engine, prompt_runtime,   │                            │
│    retry_status, config_types│                            │
└──────────────┬───────────────┘                            │
               │ path dep                                    │
               ▼                                             │
┌──────────────────────────────────────────────────────────┐ │
│ codesmith-tui  (HOST / binary)                            │─┘ (optional)
│  • build_engine → resolve_llm_client → registry.build     │
│  • Config, logging, retry_status (UI globals)             │
└───────────────────────────────────────────────────────────┘
```
真正要紧的箭头：**`codesmith-tui` 依赖 `codesmith-providers`
（可选、feature 门控），绝不反向依赖。** Provider 只依赖
`codesmith-agent`（以及目前出于共享全局量暂时依赖的
`codesmith-agent-runtime` —— 移除它的事项见 ROADMAP §B）。

## provider 接缝

客户端的构建不需要宿主指名任何具体类型：

```text
  Host (tui)                      codesmith-agent                  codesmith-providers
  ─────────                       ──────────────                   ───────────────────
  Config ──▶ resolve_llm_client
                 │ builds ProviderConfig
                 │ (6 neutral fields + on_retry)
                 ▼
               ProviderRegistry::build(&cfg)
                 │ resolves factory by cfg.provider
                 ▼
               ProviderFactory::build(&cfg) ───────▶ MockClient / RigLlmClient / ...
                 │
                 ▼
               LlmClientHandle (Arc<dyn LlmClient>)
```
- **`ProviderId`** —— 开放联合类型：已知 provider 用 `Builtin(ProviderKind)`，
  其余任何东西用 `Custom(String)`。镜像 pi-ai 的 `KnownProvider | string`。
- **`ProviderConfig`** —— 中立的构造输入（`api_key`、`base_url`、
  `default_model`、`retry`、`http_headers`、`on_retry`）。不依赖 TUI 的
  `Config`，因此 provider crate 保持宿主无关。
- **`ProviderFactory`** —— `id()` + `build(&cfg) -> LlmClientHandle`。在
  `codesmith-providers`（或你自己的 crate）中实现并注册它。
- **`ProviderRegistry`** —— `HashMap<ProviderId, Arc<dyn ProviderFactory>>`。
  `register` 执行 upsert（后写胜出，类似 pi-ai 的 `setProvider`）；`build`
  解析并委托，若无匹配则以已注册的 id 集合报错。

## 框架核心 agent 接缝（§E）

上文的 provider 接缝是第一个 LangChain 类比。§E 用四个宿主无关的 trait
将核心向更完整的 agent 框架延伸，它们分别镜像 LangChain 的 `BaseTool` /
`Memory` / `Callbacks` / `AgentExecutor`。它们位于 `codesmith-agent`，
因此任何 provider 或宿主都能驱动 agent 循环，而不依赖
`codesmith-agent-runtime` 的生产 `Engine`。

```text
  Host                             codesmith-agent (CORE)
  ────                             ──────────────────────
  Arc<dyn AgentExecutor> ◀── built from LlmClientHandle + Arc<ToolSet> + Arc<dyn Callback>
        │
        ▼  AgentExecutor::run(&mut dyn ChatHistory, user_text)
   ┌────┴────────────────────────────────────────────────────┐
   │ DefaultAgentExecutor loop (cap = config.max_steps):      │
   │   build MessageRequest from ChatHistory + ToolSet        │
   │   ▶ Callback::on_llm_start  → LlmClient::create_message  │
   │                                _stream                   │
   │   ▶ accumulate StreamEvent → Vec<ContentBlock>           │
   │   ▶ Callback::on_llm_end    → push assistant Message     │
   │   extract ContentBlock::ToolUse{ id, name, input }       │
   │   if none → Callback::on_complete(NoToolCalls); return   │
   │   for each tool_use:                                     │
   │     ▶ Callback::on_tool_start → Tool::run(input)         │
   │     ▶ Callback::on_tool_end   → push ToolResult Message  │
   │   ▶ Callback::on_step; if step+1 >= max_steps → return  │
   └──────────────────────────────────────────────────────────┘
```
- **`Tool`**（`tools::Tool`）—— 可执行工具契约（LangChain `BaseTool`
  的类比）。宿主无关：每个实现自带自己的依赖，`tools::Tool::run`
  只接收一个已解析的 `input` —— 核心中**没有每次调用的胖
  `ToolContext`**（那个位于 `codesmith-agent-runtime::tools::spec`）。
  桥接到生产 `ToolSpec`+`ToolContext` 的是 `ToolSpecAdapter`（位于
  `codesmith-agent-runtime::tools::framework_adapter`，§E）：它捕获一个
  共享的 `ToolContext`，并把 `run` 委托给 `ToolSpec::execute`。发送给
  模型的 wire 定义是独立的 `models::Tool`；`ToolSet` 通过 `to_api_tools()`
  将可执行形式转换为 wire 形式。
- **`ChatHistory`**（`memory::ChatHistory`）—— 转录视图（LangChain
  `Memory` 的类比）：`messages` / `push` / `clear`。`VecChatHistory` 是
  内存中的默认实现；宿主通过 `SessionChatHistory`（位于
  `codesmith-agent-runtime::session_history`，§E）以自己的 `Session`
  为其支撑。
- **`Callback`**（`callback::Callback`）—— 观察钩子（LangChain
  `Callbacks` 的类比）：`on_llm_start` / `on_llm_end` / `on_tool_start` /
  `on_tool_end` / `on_step` / `on_complete`，全部默认为空操作。
  `CallbackSet` 扇出到多个观察者；`NoopCallback` 是默认值。桥接到宿主
  `Event` UI 通道 + `HookHost` shell 钩子的是 `CallbackBridge`（位于
  `codesmith-agent-runtime::callback_bridge`，§E）：它把工具生命周期
  钩子转发到两条路径上；LLM/step/complete 钩子是已文档化的空操作
  （那些归生产调用方与流归约代码所有）。
- **`AgentExecutor`**（`executor::AgentExecutor`）—— 驱动循环；
  `DefaultAgentExecutor` 是参考实现（核心内）。宿主侧的
  `HostAgentExecutor`（位于
  `codesmith-agent-runtime::engine::host_executor`，§E）镜像了跑在三个桥
  之上的裸循环，并且是逐片吸收生产 `Engine` 护栏的指定归宿 —— 目前
  已吸收 **十项**：**loop-guard**（拦截第 3 次相同调用，3/8 次连续失败
  时警告/中止）位于其 per-tool / post-tool 接缝；**LSP flush**（每次
  成功的 edit 后收集诊断，在下一次请求前作为一条 user 消息冲刷出去）
  位于其 per-tool / per-step-pre-request 接缝；**transparent-retry**
  （流在中途死亡且尚无任何内容提交时重发请求，最多 3 次；一轮健康后
  重置预算）位于其 per-step post-stream 接缝；**steer**（在下一次请求
  前把排队的用户输入排空为 `user` 消息）位于其 per-step pre-request
  接缝；**approval**（把写/代码执行工具挡在用户许可之后：按 wire 工具
  id 发出 `ApprovalRequired` 并阻塞在决策通道上；被拒绝 ⇒
  `permission_denied` 错误、工具跳过）位于其 per-tool 接缝；
  **compaction**（不经过 LLM 调用即对超过 32KB 缓存触发线的陈旧工具
  结果做 micro-compact，随后在 `should_compact` 通过时经由 LLM 摘要自动
  压缩；两者都通过 `clear()`+`push()` 整体替换）位于其 per-step
  pre-request 接缝；另有 **capacity**、**subagent**（完成挂起 + 哨兵）、
  **early-tool-start**，以及 **cycle** 护栏（完整清单见
  `host_executor.rs` 模块文档的 "Absorbed guardrails"）。per-step 机制按
  阶段拆分为 `engine/turn/` 下的私有子模块（`stream.rs`、`batches.rs`、
  `approval.rs`、`seams.rs`、`postprocess.rs`）；`host_executor.rs` 保留
  step 循环本身以及它直接拥有的横切护栏。LSP 累加器、steer 接收器、
  approval 接收器与压缩探针是**内部可变性（interior-mutability）切片**：
  `LspProbe.pending` 是 `Arc<std::sync::Mutex<Vec<DiagnosticBlock>>>`
  （LSP：锁绝不跨 `await` 持有，与 `CallbackBridge` 一致），而 `steer`
  是 `Option<Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>>` —— 用
  `tokio::sync::Mutex`（而非 `std`）是为了让守卫可以跨越子代理阻塞挂起
  的 `biased select!` steer 臂里阻塞的 `recv().await`（与 `approval`
  同理；请求前的 `try_recv` 排空是非阻塞且无竞争的 —— 单消费者 ——
  因此 tokio mutex 在那里是一次零成本升级）；两者都内部可变，因为
  `AgentExecutor::run` 是 `&self`，而累加器在 collect/flush 时变更、
  `try_recv`/`recv` 取 `&mut self`；它们跨 `run` 调用持久存在，因此
  某个以 `MaxSteps` 结束的 turn 里一次 edit 的诊断会在下一个 turn 的
  首次 flush 时浮现，turn 之间排队的 steer 会在下一个 turn 的首次排空
  时被拾取。`approval` 使用 `tokio::sync::Mutex`
  （`Option<Arc<tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>>>`，
  因为守卫必须跨越阻塞的 `recv().await`；std mutex 守卫不是 `Send`）。
  `compaction` 携带 `micro_state:
  Arc<std::sync::Mutex<MicroCompactState>>` 和 `circuit_breaker:
  Arc<std::sync::Mutex<CompactionCircuitBreaker>>`（没有锁跨越 `await`
  —— 消息在异步 `compact_messages_safe` 调用之前被克隆出去），跨
  `run` 调用持久存在，因此 turn N 上一次失败的压缩在 turn N+1 上仍会
  触发熔断器（与 `Engine.micro_compact_state` /
  `.compaction_circuit_breaker` 一致）。压缩与容量探针还携带会话事实
  账本（`Session::fact_ledger`，`compaction/fact_ledger.rs`）：每次压缩
  的丢弃集都会喂给它经规则抽取的绝不可丢失事实（任务约束、关键路径、
  失败原因），它渲染出的小节搭乘每一次压缩摘要与 cycle 重置种子，而
  第一条任务指令在低于 `TASK_INSTRUCTION_PIN_TOKEN_CAP` 时由
  `plan_compaction` 逐字钉住。该账本也是反思循环的存储：分层摘要的
  "Refuted Assumptions & Invariants"（被推翻的假设与不变量）小节 ——
  模型从自身失败中得出的教训 —— 会被解析回账本、成为
  `RefutedAssumption` 条目，因此一条学到的不变量比携带它的摘要活得更久
  （不涉及任何外部 playbook 知识）。一个独立的交付物看门狗
  （`engine/deliverables.rs`，执行器上的 `DeliverablesProbe`）共享请求前
  接缝：从任务指令解析出的输出路径每 `DELIVERABLES_CHECK_CADENCE_STEPS`
  步在磁盘上复查一次，缺失的路径作为一条
  `<codesmith:runtime_event kind="deliverables_check">` user 消息推回，
  使缺失的交付物在运行中途浮现，而不是等到评分时。压缩摘要还要通过
  一道分层小节门控（`summary_section_count`）：一次平坦抽取会重试一次。
  transparent-retry 复用局部状态模式（每次运行的 `u32` 计数器，与
  loop-guard 一致）。护栏状态经宿主的 `Event` 通道（`event_tx`）浮出，
  而非经 `Callback`。`StopReason`（`NoToolCalls` / `MaxSteps` / `Error`）
  是终止结果。

现在已有的内容（§E 切换已完成）：生产 `Engine` 的护栏（原先位于现已
删除的 `turn_loop.rs`，`handle_deepseek_turn` 已退役）被吸收进
`HostAgentExecutor` —— 三个宿主→框架桥全部落地（`ToolSpecAdapter`、
`CallbackBridge`、`SessionChatHistory`），宿主侧的 `HostAgentExecutor`
在它们之上运行裸 LLM↔工具循环，**已吸收十项护栏**（loop-guard、LSP
flush、transparent-retry、steer、approval、compaction、capacity、
early-tool-start、subagent post-stream drain、cancel-token；per-step 机制
按阶段拆分为 `engine/turn/{stream,batches,approval,seams,postprocess}.rs`
—— 完整清单见 `host_executor.rs` 模块文档）：**loop-guard** 位于其
per-tool / post-tool 接缝（拦截第 3 次相同调用，3/8 次连续失败时警告/
中止）；**LSP flush** 位于其 per-tool（edit 后收集）/ per-step 请求前
（冲刷）接缝 —— 它是第一个需要 `Engine` 可变状态的护栏，以 `LspProbe`
上的 `Arc<std::sync::Mutex<Vec<DiagnosticBlock>>>` 落地（第一个内部
可变性切片；锁绝不跨 `await` 持有，与 `CallbackBridge` 一致；跨 `run`
调用持久存在，因此以 `MaxSteps` 结束的 turn 的 edit 诊断在下一个 turn
浮现）—— **transparent-retry** 位于其 per-step post-stream 接缝（流在
中途死亡且尚无任何内容提交时重发请求，最多 3 次；一轮健康后预算重置；
对 `Callback` 透明）—— **steer** 位于其 per-step 请求前接缝（在请求
快照之前把排队的用户输入排空为 `user` 消息），以
`Option<Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>>` 落地 —— 用
`tokio::sync::Mutex`（而非 `std`）是为了让守卫可以跨越子代理阻塞挂起的
`biased select!` steer 臂里阻塞的 `recv().await`（与 `approval` 同理；
请求前的 `try_recv` 排空是非阻塞且无竞争的）；跨 `run` 调用持久存在，
因此 turn 之间排队的 steer 在下一个 turn 被拾取）—— **approval** 位于
其 per-tool 接缝（把写/代码执行工具挡在门后：按 wire 工具 id 发出
`ApprovalRequired` 并阻塞在决策通道上；被拒绝 ⇒ `permission_denied`
错误；该护栏用 `tokio::sync::Mutex`，因为守卫必须跨越 `recv().await`
（steer 的子代理阻塞挂起臂同理）；审批从 `Tool::capabilities` 静态推导，
逐输入覆盖 + 沙箱提权推迟到 wire-in）—— **compaction** 位于其 per-step
请求前接缝（不经过 LLM 调用即对超过 32KB 缓存触发线的陈旧工具结果做
micro-compact，随后在 `should_compact` 通过时经由 LLM 摘要自动压缩；
两者都通过 `clear()`+`push()` 整体替换转录；`CompactionProbe` 携带跨
`run` 调用持久存在的 `std::sync::Mutex` 微状态 + 熔断器；摘要提示词
合并已吸收 ✅（slice 25a §E）、附件重注入已吸收 ✅（slice 25b §E）、
压缩后清理已吸收 ✅（slice 25c §E）—— 见 `host_executor.rs` 模块文档；
仅剩增强 + 工作集钉扎推迟到 wire-in）。它的四个接缝（per-step 请求前 /
post-stream / per-tool / post-tool）此后也长出了其余护栏（完整集合见
`host_executor.rs` 模块文档），`handle_deepseek_turn` 在 slice 20 §E
切换中退役。loop-guard 证明了自包含护栏用 `&self` + 局部状态即可；LSP
flush 证明了需要共享可变状态的护栏所适用的 `Arc<Mutex<…>>` 形态（steer
采用同一形态，但用 `tokio::sync::Mutex` 而非 `std` —— 见上文）；
transparent-retry 证明了 seam-2 post-stream 形态（局部计数器 +
`accumulate_stream` 的 `Err` 信号）；approval 证明了带阻塞
`recv().await` 的 seam-3 per-tool 形态（`tokio::sync::Mutex`）；compaction
证明了 seam-1 请求前整体替换形态（先克隆再 `compact_messages_safe`，
以 `clear()`+`push()` 应用）并带跨 `run` 的熔断器。**LSP flush 的已知
缺口（设计如此）：** `apply_patch` 路径推导被推迟（需要
`HostServices::preflight_apply_patch_paths`，没有重量级宿主 trait 就无法
从 `agent-runtime` 触达）；合成冲刷消息不携带 `<turn_meta>` 增强（框架
路径目前任何地方都没有 turn_meta —— 横切的宿主侧关注点，推迟到它自己
的 slice）；推送没有 `emit_session_updated`（与执行器其他消息推送一致；
UI 呈现推迟到 wire-in 步骤）。**transparent-retry 的已知取舍（设计
如此）：** `accumulate_stream` 遇到第一个出错的流条目即放弃并丢弃部分
块，因此即使生产环境本会交付部分内容的场合，重试也会触发（它内联追踪
`any_content_received`）—— 既然部分内容已经丢失，重试是唯一的恢复
路径，而内联流归约（后续某个取代 `accumulate_stream` 的 slice）会补上
这个缺口；流前连接错误（`create_message_stream` 的 `Err`）不重试（生产
环境将其视为上下文恢复 / 硬失败，一个独立的护栏）；cancel-token 短路
（生产环境的 `should_transparently_retry_stream` 检查 `!cancelled`）已
吸收 ✅ —— 检查点 B/C/D 已接线（见 `host_executor.rs` 模块文档）；有界
预算（`MAX_STREAM_RETRIES = 3`）不会永远循环。流式增量
（`MessageDelta`/`ThinkingDelta`）将继续直接经 `Event` 通道流动（没有
对应的 `Callback` 方法），直到某个内联流归约器取代 `accumulate_stream`。
E4（声明式 `providers.toml` + 惰性加载）已落地 —— slice 43 在
`codesmith-config` 中交付了 schema/加载器；slice 44 将 `default_registry`
接线到内置的 `providers.toml`（把 `COMPAT_KINDS` 目录外置）并带
`OnceLock` 缓存；slice 45 填充了 `base_url`/`model` 列，并让工厂在宿主
传入空的 `ProviderConfig` 值时将它们作为回退消费（因此该清单是一个完整
的逐 provider 默认来源）。两个后续事项被推迟（记录在 ROADMAP §E4，
slice 51）：解析器链仍回退到硬编码的 `DEFAULT_*` 常量而非清单（env 覆盖
增强 —— 按 §C6 跨层不可达），flash/kimi-code 模型变体留在宿主侧（无
清单条目）。框架 trait 用一个内联 mock LLM + mock 工具验证（见
`crates/agent/src/executor/mod.rs` 测试）—— 不需要 `codesmith-providers`
依赖，与 provider foundation slice 的 `mock` 样例呼应。`ToolSpec` 适配器
还通过在框架执行器上端到端驱动一个真实 `ToolSpec` 得到额外验证（见
`crates/agent-runtime/src/tools/framework_adapter.rs` 测试）；
`CallbackBridge` 则通过在执行器上驱动一次工具调用往返、同时点亮 mock
`Event` 通道与 mock `HookHost` 得到验证（见
`crates/agent-runtime/src/callback_bridge.rs` 测试）。

`host_executor.rs` 模块文档承载 §E 全部九个领域完整的 "Known gaps
(by design)"（已知缺口，设计如此）—— LSP flush、系统提示刷新、
thinking-only、transparent-retry、approval、compaction、capacity、
early-tool-start 与 subagent。本叙述详述了其中四个最承重的（LSP flush /
transparent-retry / approval / compaction）；其余五个（系统提示刷新 /
thinking-only / capacity / early-tool-start / subagent —— 各自有其推迟
到 wire-in 的事项）请直接查看模块文档，而非在此转录（避免每个未来
slice 都产生行漂移）。

## 扩展系统（§F）

§F 在 §E 框架核心 trait 之上构建扩展系统。同样的三层拆分适用：

- **契约**（`codesmith-agent::extension`）：扩展作者实现的宿主无关
  trait —— `Extension`（工厂）、`ExtensionApi`（命令式注册面）、
  `ExtensionContext` / `ExtensionCommandContext`（只读为主的宿主状态 +
  陈旧上下文守卫）、`ExtensionEvent`（`#[non_exhaustive]` 最小 6 变体
  集）、`Handler`（观察者）、`ToolDefinition` / `CommandDefinition`
  （贡献契约）。扩展 trait 使用 `#[async_trait]`（不同于 §E 手工的
  `Pin<Box<dyn Future>>`），因为它们面向外部 crate 中的扩展作者，在那里
  这个宏友好得多。
- **运行时**（`codesmith-extensions`）：`ExtensionRunner`（按 §8.3 的
  尽力而为事件扇出、按 §7.3 的 `Arc<AtomicU64>` 陈旧上下文守卫、两阶段
  stub→real `ExtensionApi`）、基于 `inventory` 的静态发现
  （`discover_static`）、`EventBus` 骨架（实现在 §F3）、install-source
  trait（实现在 §F5）。
- **适配器**（`codesmith-agent-runtime`）：`ExtensionToolSpecAdapter` 把
  一个 `Box<dyn ToolDefinition>` 包装成 `ToolSpec`，让 agent 循环看到的
  是一个普通工具（镜像 `ToolSpecAdapter`）；`HostAgentExecutor` 持有
  `Option<Arc<ExtensionRunner>>` 并在四个 turn 接缝上发射事件（TurnStart
  / ToolCall ×2 / ToolResult ×2 / TurnEnd ×2）。
- **宿主接线**（`codesmith-tui`）：`build_extension_runtime()` 在引擎
  构建时运行一次 discover → reconcile → load → `bind_core` 序列（共享
  引擎的 `cancel_token`，使处理器能观察到用户按 ESC）；`ExtensionStateStore`
  （镜像 `SkillStateStore`）逐 id 跟踪启用/禁用；`/extension` 命令组
  （list/info/enable/disable/status/reload 可用；install/uninstall 是
  "phase 2" 桩）。

仓库内扩展 `sample_scratchpad` 练习了全部三个贡献点（工具 + 命令 +
处理器）以及完整的 discover → load → configure → bind_core → emit 路径。
`/extension list` 可以看到它。

```
   extension author ──impls──▶ codesmith_agent::extension (contract)
                                        │ used by
                                        ▼
              codesmith_extensions (runtime: Runner + discovery + Bus)
                                        │ bridged by
                                        ▼
              codesmith_agent_runtime (ExtensionToolSpecAdapter + executor seams)
                                        │ wired by
                                        ▼
              codesmith_tui (build_extension_runtime + StateStore + /extension cmd)
```
Slice 1（§F1）落地最小契约 + 运行时 + 适配器 + 宿主接线 + 样例。Slice
2a（§F2a）升级契约 + 运行时核心：完整的 23 变体 `ExtensionEvent` 集合 +
`ExtensionEventKind`/`kind()`、`HandlerOutcome`
（`Continue`/`Cancel`/`Block`/`Transform`）跨处理器链（`Handler::handle`
现在返回 `Result<HandlerOutcome, _>`）、逐变体的
`ExtensionApi::on_variant` 订阅，以及 `ExtensionRunner::emit` 中的
`catch_unwind` 隔离（所有权进 / `EmitOutcome` 出）。Slice 2b（§F2b）
接线宿主接缝：`EmitOutcome` 上的 `#[must_use]`（强制检查接缝返回值）；
在 `ToolCall` 处遵守 `Block`（跳过分发 → permission-denied）；在
`SessionBefore*` 处遵守 `Cancel`（跳过压缩/切换）；在 `Input`/
`BeforeAgentStart`/`BeforeProviderRequest`/`ToolResult` 处遵守
`Transform`（改写可操作字段；`ToolResult` 把 emit→`on_tool_end`→传播到
`outcomes[idx]` 的顺序重新安排）；错位的结果 → `Continue`；发射 22/23
个事件（`ToolExecutionUpdate` 推迟 —— 需要一个 `Callback::on_tool_progress`
流钩子）；完整的 e2e 往返测试；热重载经 `ExtensionRunner::clear_handlers`
+ `reload_extension_runtime`（clear→invalidate→discover→reconcile→load→
bind_core）重新填充共享的 runner `Arc`，因此 `/extension reload` 会同时
更新 `App.extension_runner` 与 Engine 的字段。推迟到 §F2c：
`ToolExecutionUpdate`（流钩子）、重载共享引擎的 `cancel_token`，以及从
App runner 不可触达的 3 个 tui 层接缝（`ProjectTrust` 同步上下文 /
`ResourcesDiscover` 独立 MCP 进程 / `SessionBeforeFork` 死代码 fork 路径）。
推迟到 §F3–§F8：`EventBus` 实现、`registerProvider`、
`registerShortcut`/`registerFlag`/渲染器、dylib 加载（phase 2）、
install-source 实现、embed API。热加载永久排除（规范 §2.4）；只有
install + reload。

## 当前已接线的内容（foundation slice + §D1 对等桥）

| 关注点 | 状态 | 位置 |
|---|---|---|
| 核心抽象（`LlmClient`、`ProviderFactory`、`ProviderRegistry`） | ✅ 完成 | `crates/agent/src/{llm_client,provider}/` |
| 真实引擎循环中的注册表 | ✅ 完成 | `crates/tui/src/core/engine.rs` `resolve_llm_client` |
| TUI 本地 `DeepSeekProviderFactory` 退役 —— rig 的 `DeepSeekFactory`（经 `default_registry()`）取代它（§A1） | ✅ 完成 —— tui 不再持有任何 provider 工厂 | 已从 `crates/tui/src/core/engine.rs` 删除 |
| `DeepSeekClient` 退役 —— rig 的 `RigLlmClient` 取代它（§A1）；`from_parts` 随客户端一同删除 | ✅ 完成 | `crates/tui/src/client.rs` 已删除（slice 41） |
| `codesmith-providers` crate + `mock` provider + Cargo feature | ✅ 完成 | `crates/providers/` |
| rig 适配器 `RigLlmClient<C,S>` 实现 `LlmClient` | ✅ 完成 | `crates/providers/src/rig_adapter/` |
| 四个 rig 支撑的工厂（`openai` / `anthropic` / `deepseek` / `openai-compat` ×13） | ✅ 完成 —— 目录现为声明式（`providers.toml`，§E4）；`base_url`/`model` 已填充，并作为清单默认回退被消费（§E4 slice 45）；后续事项（env 覆盖增强 + flash/kimi-code 变体下沉）推迟 —— 记录在 ROADMAP §E4（slice 51） | `crates/providers/src/{openai,anthropic,deepseek,openai_compat}.rs`, `crates/providers/providers.toml` |
| `resolve_llm_client` 对所有 provider 从 `default_registry()` 播种 | ✅ 完成（§D1 部分 → §A1 完全切换 —— DeepSeek 从 tui 本地工厂迁到 rig 上） | `crates/tui/src/core/engine.rs` |
| `AnthropicClient` 退役 —— rig 的 `AnthropicFactory` 取代它（§A2） | ✅ 完成 | `crates/tui/src/client/anthropic.rs` 已删除 |
| 对等桥：reasoning 启发式 + `shape_messages` / `shape_max_tokens` | ✅ 完成 | `crates/providers/src/rig_adapter/{reasoning,shaper}.rs` |
| 将 `DeepSeekClient` 抽取到 `codesmith-providers`（退役 tui 本地工厂） | ✅ 完成（已被取代 —— 是退役而非抽取）—— `DeepSeekClient` 经 rig 适配器退役；replay-bridge 阻碍被发现并不必要（rig 的 compat 层原生将 `AssistantContent::Reasoning` 序列化为 `reasoning_content`）；tui 的 `client.rs`/`chat.rs` 已删除（slice 41），inspect/warmup 迁移到 `codesmith-agent-runtime` 的 `prompt_inspect`，reasoning 谓词 + `sha256_hex` 去重（slice 42） | ROADMAP §A1 |
| 解耦替换（B3 `ApiProvider`→`ProviderKind`） | ✅ 完成 —— `DeepseekCN` 并入 `Deepseek`（slice 52）；以 `&str` 为键是 §C6 的解耦路径 | ROADMAP §B |
| 宿主经 config 选择 provider（例如 `provider = "mock"` / 自定义 id） | ✅ 完成（9d47942c）—— `custom_provider` 选择器 + `[[providers.custom]]` 表；§D2 slice 46 关闭了收尾打磨 —— `--custom-provider <id>` CLI 标志（经 env 转发给 TUI）+ 逐条目的 `config set/get/unset providers.custom.<id>.<field>`（按 id 查找或创建）；裸的 `provider = "<id>"` 形式保持**设计上即拒绝**（见 9d47942c —— 会让已闭合的 `ProviderKind` 枚举级联穿透 config + overrides + env + 每个 match 臂） | ROADMAP §D2 |
| Agent 执行器循环、工具/记忆抽象（LangChain 对齐） | ✅ 框架核心 trait 已落地（E1/E2/E3）；`ToolSpec`→`Tool` 适配器已落地（§E）；`Event`/`HookHost`→`Callback` 桥已落地（§E）；`Session`→`ChatHistory` 桥已落地（§E）；`HostAgentExecutor` 是活跃的生产路径（slice 20 切换 —— `handle_send_message` 经它路由，`handle_deepseek_turn` 已删除）；全部护栏经 slice 11–40 吸收（loop-guard + LSP flush + transparent-retry + steer + approval + compaction + capacity + subagent + early-tool-start/并行分发 + thinking-only），经 `event_tx`；`LspProbe` + `CompactionProbe` 微状态/熔断器上的内部可变性 `Arc<std::sync::Mutex<…>>`，steer + approval 接收器上的 `tokio::sync::Mutex`（两者都在子代理阻塞挂起的 `biased select!` 中跨越 `recv().await`）；transparent-retry 位于 seam-2 post-stream；steer + compaction 位于 seam-1 请求前；approval 位于 seam-3 per-tool；生产 `Engine` 迁移完成 | `crates/agent/src/{tools,memory,callback,executor}/`, `crates/agent-runtime/src/{tools/framework_adapter,callback_bridge,session_history}.rs`, `crates/agent-runtime/src/engine/host_executor.rs` |
| 扩展系统（§F1 基础核心 + §F2a 契约 + §F2b 宿主接缝接线） | ✅ 完成（slice 1 §F1 + slice 2a §F2a + slice 2b §F2b）—— §F1：最小 6 事件契约（`codesmith-agent::extension`）+ 运行时（`codesmith-extensions`：`ExtensionRunner` + stub→real `ExtensionApi` + `inventory` 发现 + `EventBus` 骨架 + install-source trait）+ 适配器（`ExtensionToolSpecAdapter`）+ `HostAgentExecutor` 的 4 接缝发射（TurnStart/ToolCall/ToolResult/TurnEnd）+ `build_extension_runtime()` + `ExtensionStateStore` + `/extension` 命令组（list/info/enable/disable/status/reload 可用；install/uninstall 是 "phase 2" 桩）+ 仓库内 `scratchpad` 样例；§F2a：完整 23 变体 `ExtensionEvent` 集合 + `ExtensionEventKind`/`kind()` + `HandlerOutcome`（Continue/Cancel/Block/Transform）跨处理器链 + 逐变体 `on_variant` 订阅 + 带 `catch_unwind` 隔离、返回 `EmitOutcome` 的链式 `emit`；§F2b：`EmitOutcome` 上的 `#[must_use]` + 在 7 个 `host_executor` 接缝遵守 Block/Cancel/Transform（错位 → Continue）+ 发射 22/23 个事件（ToolExecutionUpdate 推迟）+ 完整 e2e 往返 + 经 `clear_handlers` + `reload_extension_runtime` 重新填充共享 runner Arc 的热重载；`ToolExecutionUpdate`（流钩子）+ 重载共享引擎 cancel_token + 3 个 tui 层接缝（ProjectTrust/ResourcesDiscover/SessionBeforeFork）推迟到 §F2c；`EventBus` 实现 + dylib + install-source 实现推迟到 §F3–§F8；热加载永久排除 | `crates/agent/src/extension.rs`, `crates/extensions/`, `crates/agent-runtime/src/tools/extension.rs`, `crates/agent-runtime/src/engine/{mod.rs,host_executor.rs}`, `crates/tui/src/{extension_state.rs,commands/extension_commands.rs,core/engine.rs,tui/ui.rs}` |
| 扩展系统文档 | ✅ 完成（slice 1 §F1） | `docs/EXTENSIONS.md` |

## 注册一个 provider（开发者指南）

provider 是一个位于 Cargo feature 之后的 `ProviderFactory` 实现。mock
provider（`crates/providers/src/mock.rs`）是参考样例 —— 照它的形状复制
即可新增一个。

```rust
use std::sync::Arc;
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::provider::{ProviderConfig, ProviderFactory, ProviderId};

pub struct AcmeFactory;
impl ProviderFactory for AcmeFactory {
    fn id(&self) -> ProviderId { ProviderId::from("acme") }
    fn build(&self, cfg: &ProviderConfig) -> anyhow::Result<LlmClientHandle> {
        // construct your client from cfg.api_key / cfg.base_url / cfg.default_model / ...
        todo!()
    }
}
```
宿主播种注册表，并且可以覆盖任何默认项：

```rust
// default_registry() returns a cached &'static ProviderRegistry (built once
// from providers.toml); clone to mutate.
let mut registry = codesmith_providers::default_registry().clone();
registry.register(Arc::new(AcmeFactory));                   // add/replace
let client = registry.build(&cfg)?;                          // never names a concrete type
```