# 上下文工程改进计划

状态：规划
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
