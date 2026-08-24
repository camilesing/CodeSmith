# Model Lab 路线图

Model Lab 是 CodeSmith 规划中的开放模型工作台。北极星目标很简单：CodeSmith 应当成为跨每一个提供此类模型的 provider、面向开源与开放权重模型的最佳终端编程智能体。Model Lab 让这些模型变得可发现、可评测、可路由、可服务、可导出，同时不削弱当前的终端智能体契约：本地工作区控制、显式的 provider 认证、审批闸门和清晰的隐私边界。

本文档是路线图性质的表述。它不代表下文的每一个工作集今天都已实现。

## 当前已实现

- DeepSeek 是当前的一等默认 provider，提供 `deepseek-v4-pro`、`deepseek-v4-flash`、流式思考块、Fin 路由、`CODESMITH_*` 环境变量以及 `~/.codesmith` 配置兼容。
- OpenRouter、Novita、Fireworks、NVIDIA NIM、AtlasCloud、万界方舟（Wanjie Ark）、通用 OpenAI 兼容端点、SGLang、vLLM 和 Ollama 是受支持的 provider 路径，前提是它们的 ID 出现在 `/provider`、`codesmith --provider` 或 `codesmith models` 中。
- 模型自动路由会为每个对话轮选择具体的 DeepSeek 模型和思考级别。它不是 TUI 模式。
- Fin 是快速的 `deepseek-v4-flash` 关闭思考路径，用于路由、摘要、廉价检查、RLM 子调用、唤醒验证和二进制完成检查。
- 自托管的 OpenAI 兼容端点可以通过 SGLang、vLLM、Ollama 或通用的 `openai` provider 配置使用。

## 尚未实现

- 原生 Hugging Face provider 或 Hub 浏览器。
- 内置的 Hugging Face 模型卡、数据集、adapter、safetensors 或 Jobs 工作流。
- 原生的 Unsloth、NeMo 或 Arcee 集成。
- 专门的 Model Lab UI 标签页。
- 内置基准测试套件、评测排行榜、托管可观测性或训练基础设施编排。

在这些落地之前，请使用上述 provider 路径、MCP 服务器或由用户显式配置的外部工作流。

## Model Lab 原则

Model Lab 应当帮助用户回答这些实际问题：

- 这一轮应该由哪个模型处理？
- 哪个开放或开放权重模型可以在本地或通过受信任的 provider 运行？
- 哪个 provider 能以我需要的延迟、价格、上下文窗口、许可证和隐私姿态提供该模型？
- 这个模型花了多少成本、表现如何、有哪些数据离开了我的机器？
- 我能否复现、导出或自托管这条路由？

它绝不应隐藏 provider 边界、静默上传本地产物，或在 CodeSmith 实际能够路由到某个模型之前就宣称它可用。

## Hugging Face 工作集

计划范围：

- Hub API 认证与模型发现。
- 以终端友好的方式呈现模型卡、许可证、标签、safetensors 元数据、adapter 和
  数据集链接。
- 当用户完成配置后，将 Inference Providers 作为显式的 provider 选项。
- 将 Hugging Face Jobs 作为用户批准实验的可选远程执行路径。

当前的非目标：在代码实现之前宣称存在原生 Hugging Face provider。

## Unsloth 工作集

计划范围：

- 面向已经拥有数据和算力路径的用户的微调配方与 adapter 工作流。
- 保持数据集、adapter 和 checkpoint 位置显式的导出指引。
- 面向那些可以回到本地服务或托管 OpenAI 兼容端点的模型的兼容性说明。

## NeMo 工作集

计划范围：

- 面向运行以 NVIDIA 为中心的基础设施的用户的训练与对齐工作流说明。
- 在今天已存在的 NVIDIA NIM 推理支持与未来的 NeMo 训练或定制工作流之间
  划清边界。

## Arcee 工作集

计划范围：

- 小模型路由与专门化实验。
- 可导出的路由，能清晰标明任务何时由较小的模型、Fin 或完整 DeepSeek 推理
  处理。

## 服务工作集

计划范围：

- 为 SGLang、vLLM、Ollama 和 OpenAI 兼容网关提供更好的本地与私有服务体验。
- 健康检查、模型列表、上下文窗口元数据和路由验证。
- 不允许静默的网络暴露：公开端点必须显式配置。

## 评测工作集

计划范围：

- 面向编码、评审、文档、发布检查和长上下文工作流的可复现任务套件。
- 并排路由对比，其中确切的模型、provider、思考级别、提示词和工具策略都会
  被记录。

## 可观测性工作集

计划范围：

- 面向对话轮路由、工具调用、审批、成本、缓存行为和上下文压力的本地优先
  trace。
- 导出规则会对秘密进行脱敏，并要在数据离开机器之前由用户显式操作。

## 训练基础设施工作集

计划范围：

- 数据集准备、adapter 训练、产物命名以及晋升进入服务阶段的配方。
- 本地/私有产物与任何发布到 hub 或注册表的内容相互分离。

## 隐私与导出规则

- 本地文件、提示词、对话记录、trace、模型输出、评测结果、adapter、数据集和
  checkpoint 应保持本地，除非用户显式选择某个 provider 或导出目的地。
- Provider 认证必须保持显式。`CODESMITH_*`、OpenRouter、Hugging Face 和自托管
  的凭据不应从无关的配置中推断。
- 可导出的产物应包含来源信息：源模型、provider、路由、工具策略、评测输入和
  脱敏状态。
- 公开分享、托管遥测、赞助徽章和外部品牌需要维护者批准。
