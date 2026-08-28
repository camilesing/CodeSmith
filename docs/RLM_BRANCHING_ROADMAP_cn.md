# RLM 分支路线图

本说明记录了 v0.8.45 阶段 RLM、DSPy、GEPA 与 Model Lab 的设计方向，
不引入运行时依赖，也不改动现有的 agent 循环。

## 分支原语

CodeSmith 在三个尺度上使用同一分支原语：

1. 发布轨道。每个里程碑扇出为多条命名轨道。每条轨道必须保持可独立审查、
   可合并、可延期。未完成的工作向后顺延，而不是阻塞发布。
2. 能力工作集。Model Lab 的能力（如 Hugging Face、可观测性、evals、
   serving、DSPy、GEPA 和训练基础设施）以可选工作集的形式交付，各自带有
   特性开关、安装路径、许可说明和遥测姿态。
3. Pareto 编译分支。可优化的模块保留候选的 `(instructions, demos,
   score)` 三元组。违反固定宪法条款的分支会被剪除；在至少一项 eval 中
   获胜的分支保持在前沿，直到维护者将其落地或拒绝。

前沿点由维护者选择。CodeSmith 不应过早收拢分支。

## v0.8.45

- 在更大范围的扇出开始之前，关闭当前的控制平面与工作台 issue：#1982、
  #2027、#2032、#2016 和 #2034。
- 保持 `AGENTS.md` 和 `CLAUDE.md` 仅维护者本地可见。从本里程碑起，
  `AGENTS.md` 被忽略。
- 落地 RLM 符号对象基座：活跃 prompt、会话元数据、transcript、最新用户
  消息和逐消息 refs 都是命名对象，RLM 可以直接打开它们，而无需把原始
  prompt/历史文本复制进父 transcript。

## v0.8.46

- 将 Fin 泛化为结构化反馈验证器基座。
- 添加首批从现有轨迹采集的重放 eval 定义。
- 搭建 Repeatability Score 页脚槽位并标记为 pending，直到由 eval 填充。
- 仅以 Rust 类型添加模块 artifact schema v0。
- 起草“Compiled Word”宪法条款。

## v0.8.47

- 通过 Inference Providers 与 Router 将 Hugging Face 提升为一等 provider。
- 添加确定性 RLM 重放：上下文快照、随机种子、子模型 ID 和温度。
- 将大型日志和载荷路由到 RLM 工作台会话，而非父 transcript。
- 添加以 prompt、上下文哈希和模型为键的子查询记忆化。
- 在 Rust 注册表层面强制执行 RLM 预算：深度、调用数、墙上时间和成本。

## v0.8.48

- 移除遗留的 `deepseek` 与 `deepseek-tui` shim 二进制。
- 完成 Docker 和 Homebrew 的改名清理。
- 由随核心交付的小型离线 eval 套件填充 Repeatability Score。

## v0.9.0

- 逐回合输出 `trajectory.jsonl` 作为 trainset 基座。
- 添加用于确定性重放的 `codesmith replay <turn_id>`。
- 通过 Rust 适配器渲染 `[[ ## field ## ]]` 形式的模块 artifact。
- 落地 eval 管道：套件、重放 eval 与度量基座。
- 添加解释离线循环的 `/compile` 命令存根。

## v0.10.0

- 为 DSPy 和 GEPA 添加可选的 Model Lab 工作集安装器。默认安装保持零
  Python 依赖。
- 构建首个离线编译管道：Rust 采集 trainset，Python 边车运行优化器，
  CodeSmith 产出经过审查的 Module JSON artifact。
- 添加 Compile TUI 面板，包含 Pareto 前沿、谱系树以及
  Land/Reject/Revise 操作。
- 通过 PR 落地首批优化后的工具描述与 agent prompt artifact。宪法条款在
  优化区域之外保持固定。
- 添加鲸种模块护照，例如 `Sei: codesmith-agent-prompt.v0.10.0-gepa-1`。

## 信任边界

编译是离线的。运行时消费经过审查的 JSON artifact。在线闭环优化不在范围
内，因为对抗性用户可能会操纵实时代码 harness。任何工作集都可以独立失败，
而不会把发布、核心运行时或其他 Pareto 分支一起拖垮。
