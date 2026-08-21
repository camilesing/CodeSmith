# 上下文工程改进计划

状态：规划（§3 已于 2026-08-21 执行完毕，见该节落地记录）
前置：[agent-capability-strengthening.md](agent-capability-strengthening.md) Stage 4
（集中式 TokenCounter）是本计划多数条目的地基。

## 1. 语义检索（embedding backend）接入 index seam

现状：`crates/index/src/backend.rs:133-146` 已为 embedding 检索预留 seam
（注释明确 "not implemented"）。当前只有文件清单 walk + tree-sitter 符号索引
（SQLite 存储 + backend registry，PR #7 引入）。

分阶段：

1. **定义 `EmbeddingBackend` trait**：`embed(texts: &[&str]) -> Result<Vec<Vec<f32>>>`，
   挂入现有 backend registry，与 symbol backend 平级。
2. **本地嵌入先行**：接入一个可在本地推理的小型 embedding 模型
   （候选：FastEmbed / ort 跑 bge-small / jina-embeddings-v3 蒸馏版），
   避免强制依赖外部 API。模型文件放 `~/.codesmith/models/`，首次使用按需下载。
3. **索引管道**：按符号（tree-sitter node 粒度）+ 按文件块（chunk）两种粒度
   建向量，存入现有 SQLite（新增 vecs 表 + 暴力余弦扫描即可起步；
   数据量上万后再考虑 sqlite-vec）。
4. **工具面**：`symbol_search` 增加语义模式，或新增 `semantic_search` 工具；
   命中结果带符号/文件/行号，与现有 symbol 结果同构。
5. **验收**：在 CodeSmith 自身仓库上，对"处理压缩的代码在哪"类自然语言查询
   能命中 `host_executor.rs` / `compaction/`，且延迟 < 1s（索引建好后）。

## 2. Prompt zones Phase 2 接线

现状：`crates/agent-runtime/src/compaction/prompt_zones.rs:20` 自述三区契约
（PinnedPrefix / AppendLog / TurnScratch）仅完成 Phase 1，
AppendLog/TurnScratch 未接入请求路径。前缀缓存对齐（cache-aligned summary
budget 85%）已在压缩侧做了一半。

分阶段：

1. 请求路径按三区组装：系统提示 + 工具目录 = PinnedPrefix；历史消息 =
   AppendLog；当轮临时注入（LSP 诊断 flush、steer 注入）= TurnScratch。
2. AppendLog 内消息保持 append-only（禁止改写历史消息内容，只能追加），
   从引擎层面校验（debug assert 起步）。
3. TurnScratch 注入消息在使用后显式标记可清除，避免污染后续 turn 的缓存前缀。
4. 验收：同一会话连续 turn 的 provider 端 prefix cache hit 可观测提升
   （依赖 provider 返回的 usage 缓存字段，`/tokens` 面板展示命中率）。

## 3. 容量控制器按模型自适应

现状：capacity controller 自 v0.8.11 起默认禁用，押注 DeepSeek V4 的 1M 窗口；
换小窗口模型/其他 provider 时容易在压缩介入前就撞 prompt-too-long
（目前靠 peel-retry 兜底，浪费一次往返）。

方案：

1. 以模型窗口元数据（`[providers.*]` 已有 context window 信息）驱动：
   窗口 < 200K 的模型自动启用容量预检 + 收紧压缩触发阈值；
   ≥ 500K 维持现状（信任大窗口 + 显式 `/compact`）。
2. 阈值与 Stage 4 的真实 tokenizer 计数联动，替代 chars/3 近似。
3. 验收：mock provider 上模拟 128K 窗口模型，长会话不再出现
   prompt-too-long 重试；默认 deepseek 配置行为不变。

### 落地记录（2026-08-21）

执行时澄清的两个事实（与上方方案原文的假设不同）：

- `[providers.*]` 配置里**没有** context window 字段；窗口元数据从模型名
  派生（`crates/agent/src/models.rs` 的 `context_window_for_model`：
  `_Nk` 后缀 → DeepSeek 启发 → 已知模型表 → claude 200K）。`_Nk` 后缀
  （如 `test-128k`）即 mock/自托管场景声明窗口的方式。
- 容量预检（`run_capacity_preflight`，硬预算门）在生产路径**始终开启**，
  无需"再启用"；v0.8.11 默认禁用的是 Gate A CapacityController（会改写
  历史消息的护栏干预）。经确认 Gate A 维持默认禁用，本条目落在
  "预检常开 + 收紧相关阈值"。

实际根因是**排序倒挂**：自动压缩用原始计数对比阈值，预检用保守计数
（×3/2）对比预算；128K 模型下旧阈值（95,000 原始）高于预检触发点
（≈81,920 原始当量），干净的自动压缩永远轮不到，清理总走紧急恢复或
provider 拒绝 + peel-retry。

落地内容：

1. `SMALL_CONTEXT_WINDOW_TOKENS = 200_000`；窗口 <200K 时压缩阈值改为
   `(有效窗口 − 13K) × 2/3`（与保守估算的 3/2 放大互为倒数，系数提取为
   `agent::models` 共享常量防止漂移）。128K 例：95,000 → 63,333；
   200K–500K（claude/GLM）与 ≥500K（默认 deepseek 967,000）公式逐字节
   不变；未知模型 fallback 95,000 不变。
2. `engine/context.rs` 的本地 chars/3 估算副本删除，统一 re-export
   `compaction` 的 TokenCounter 版本——预检/紧急恢复/容量观测的 system
   prompt 计数从此也走 TokenCounter（装 tokenizer.json 即精确）。
3. 窗口 <200K 的预检预算额外预留 window/100（128K 例 +1,280），≥200K
   预算不变（默认 deepseek 1M 预算 736,832 有断言守卫）。
4. 验收测试：`capacity_small_window_auto_compacts_before_preflight_budget`
   （agent-runtime，证明压缩先于预检触发）、
   `engine_128k_window_long_session_compacts_without_prompt_too_long`
   （tui，128K 模型长会话零 prompt-too-long 当量请求 + 压缩发生）；
   deepseek 默认行为回归由 models.rs 阈值快照与预算分层测试守卫。

## 4. MCP 客户端能力补全

现状：`crates/agent-runtime/src/mcp.rs`（客户端）支持 tools / resources /
prompts；缺 sampling（`sampling/createMessage`）、elicitation、roots、
OAuth。

按生态急需程度排序：

1. **sampling**：MCP server 反向借用 agent 的 LLM。实现为：收到
   sampling/createMessage 请求 → 转成内部子代理调用（复用 utility model 或
   当前模型）→ 回包。需配置开关（`[mcp].allow_sampling`，默认关）+ 审批策略。
2. **roots**：把 workspace root 列表通过 `roots/list` 暴露给 server，
   支持变更通知（worktree 切换时）。
3. **elicitation**：server 向用户请求结构化输入。路由到 TUI 的
   request_user_input 通道（复用现有 approval UI 骨架）。
4. **OAuth**：`[mcp.servers.*]` 支持 authorization code 流，token 存入
   `crates/secrets` keyring。
5. 验收：对 MCP 官方 compliance 测试集（typescript-sdk 的 tests）跑通
   对应用例；`docs/MCP.md` 同步。

## 5. 压缩质量观测

现状：压缩有 circuit breaker / partial / responsive 多档，但没有质量指标。

方案：压缩前后各采样一轮"事实问答"（文件路径、行号、决策点，由 utility
model 出题并判分），得分低于阈值时升级为 checkpoint-restart cycle
（复用 `cycle_manager.rs`）。验收：`/cycles` 面板能看到每次压缩的质量分。
