# 框架层独立使用指南

CodeSmith 的 framework group——由 `scripts/check-framework-deps.py` 强制
守护在依赖边界之后的 12 个 crate——本身就是可复用的 agent 框架，而不是
CLI 的内部管道。本文是独立使用指南：LangChain 式能力面是什么、如何仅用
`codesmith-agent` 跑通一个 agent、everything-is-plugin 面在哪里、以及
哪些能力是有意未建的。

一句话分层：`codesmith-agent` 是 `langchain-core` 的类比（契约 + 参考循
环，无具体 provider）；`codesmith-providers` 与所有实现组 crate 是
partner package；CLI 只是宿主之一。

## LangChain 类比表

| LangChain | CodeSmith | 位置 |
|---|---|---|
| `BaseChatModel` | `LlmClient`（dyn-safe、流式优先） | `codesmith-agent` `llm_client` |
| provider 包 | `ProviderFactory` / `ProviderRegistry`；具体客户端在 `codesmith-providers` | `codesmith-agent` `provider` |
| `BaseTool` / `@tool` | `Tool` + `ToolSet` | `codesmith-agent` `tools` |
| `Memory` / 聊天历史 | `ChatHistory` + `VecChatHistory` | `codesmith-agent` `memory` |
| `Callbacks` | `Callback`（含 `StopReason`、`StreamDelta`） | `codesmith-agent` `callback` |
| `AgentExecutor` | `AgentExecutor` + 参考 `DefaultAgentExecutor` | `codesmith-agent` `executor` |
| `ChatPromptTemplate` | `ChatPromptTemplate`（角色 + `{var}` 插值 + 历史占位符） | `codesmith-agent` `prompt_template` |
| `BaseOutputParser` | `OutputParser` + `JsonOutputParser` | `codesmith-agent` `output_parser` |
| `with_structured_output` | `structured_output::<T>()`（schema 强制工具 + 校验重试） | `codesmith-agent` `structured` |
| `Document` / `BaseRetriever` / `Embeddings` / `VectorStore` | `Document` / `Retriever` / `Embeddings` / `VectorStore` + `InMemoryVectorStore` | `codesmith-agent` `retrieval` |
| 模型目录 / profiles | `ModelRegistry` | `codesmith-agent` `models.rs` |
| 持久状态、会话 | 三区 `AppendLog`、`SessionJournal`、`SessionStore` | `codesmith-session` |
| 上下文工程 | 提示装配、前缀缓存、压缩策略 | `codesmith-prompts`、`agent-runtime` |
| MCP 客户端 | 完整连接池、4 种传输 | `codesmith-mcp-client` |

有意不移植的：LCEL 的 `Runnable` 组合层。executor + 工具循环 + 回调
seam 已承担该职责；没有在树消费方的组合代数只会是死表面。

## 仅用框架层跑一个 agent

```
cargo run -p codesmith-agent --example standalone_agent
```

`crates/agent/examples/standalone_agent.rs` 仅凭 `codesmith-agent` 构建出
完整 agent——提示模板、`InMemoryVectorStore` 检索包装成 `Tool`、参考
executor 驱动 `VecChatHistory` 并带录制 `Callback`、以及一次类型化的
`structured_output` 交换——LLM 用的是离线脚本化 `LlmClient`。这个示例就
是"框架层可独立站立"的可执行定义。

真实部署把脚本客户端换成 `LlmClient` 实现：用 `codesmith-providers`
（每个后端一个 feature），或在自己的 crate 里实现该 trait / 注册
`ProviderFactory`——这就是 partner package 的位置。

## Everything-is-plugin：注册面

Cordis 式的性质——能力以注册的形式到达、内核只做选择 chokepoint——分布
在两个面上：

- **扩展注册**（任意层级：Rhai mod、内建 Rhai、Rust 扩展、安装的
  dylib）经 `ExtensionApi`——见
  [EXTENSIONS_cn.md](EXTENSIONS_cn.md) 与 [MODS_cn.md](MODS_cn.md)：
  工具、命令、事件处理器（32 种事件 × 3 种派发模式）、提示分段、技能、
  provider、消息投影、守卫、工具替换、压缩策略、脚本循环
  （`register_loop`）。
- **注册表注册**（编译进进程的 crate）：下述 `TurnExecutorRegistry`、
  `ProviderRegistry`、模型目录。

### turn-executor seam（host 档 + dylib 档）

一个 turn 跑哪个循环是一次*选择*，每 turn 从进程共享的
`TurnExecutorRegistry`
（`agent-runtime/src/engine/executor_composition.rs`；配置
`turn_executor`，默认 `builtin`）解析。契约类型
（`TurnDeps`/`TurnExecutorFactory`/`BuiltTurn`/`TurnHarvest`）已公开，
注册分两档：

- **host 档** —— 编译进进程的 crate 在启动时注册、自持 RAII guard：

  ```rust,ignore
  use codesmith_agent_runtime::engine::{shared_turn_executors, TurnExecutorFactory};

  let guard = shared_turn_executors()
      .register(std::sync::Arc::new(MyLoopFactory))
      .expect("first registration is never refused");
  // 配置：turn_executor = "my-loop"；drop(guard) 即注销。
  ```

  可执行证明在
  `crates/extensions-fixture-dylib/tests/turn_executor_seam.rs`。

- **dylib 档** —— 安装的 Rust dylib 经
  `ExtensionApi::register_turn_executor` 贡献从零编写的循环（载体类型擦
  除，因为 dylib 无法写宿主侧注册表静态量——任何静态量在 dlopen 的映像
  里都是私有的）。引擎在下一个 turn 装配时收养贡献并打世代戳；reload 的
  op-loop 清扫在两阶段 dylib 卸载**之前**移除它——注册表永远不会持有指向
  已解除映射内存的 vtable。端到端证明：
  `crates/extensions/src/runner.rs` 测试
  `fixture_dylib_contributes_a_generation_bound_turn_executor`。

有 in-flight turn 的 id 拒绝被替换（`turn_lease`，报错响亮）；注册在下一
个 turn 装配时生效。

## 已知限制与迭代路线图

- **脚本 mod 不能经此 seam 提供 executor**——设计如此，脚本走
  `register_loop`（`turn_executor = "rhai"`）。
- `turn_executor` 对所有 agent 生效（暂无按 agent-type 的选择）。
- **检索只有契约 + 内存默认实现**——树内没有向量库集成、loader、
  splitter；`IndexServiceApi`（代码索引）是另一个更早的 seam。
- **三个工具注册面并存**（框架 `ToolSet`、`codesmith-tools` 值层、
  `agent-runtime` 生产 `ToolSpec`/`ToolRegistry`）；统一它们是
  ROADMAP §E 的常设事项。
- **运行时 mod 状态住独立存储，这是设计**——激活同意、启停与 KV 经
  `ModStateStore` 持久化（`mod_state` 文件 + 即时 reload），**不是**回写
  手写的 `config.toml`（Cordis 之所以回写 YAML，是因为配置是它唯一的
  事实源；CodeSmith 有意把声明式配置与运行时状态分开，且配置标识符只能
  经显式迁移变更）。
- **尚无发布边界**：framework group 仍是 workspace 内部物。独立版本发布
  （字面意义的 `langchain-core` 式拆包、或拆库）是创始人决策，不是代码
  缺口。
- 可选、未决：出现真实消费方后，最小 Runnable 式组合层。
