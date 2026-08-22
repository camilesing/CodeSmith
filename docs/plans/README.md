# 改进计划索引

源自 2026-08-20 对 CodeSmith 的整体评审。四个方向各自成篇，可独立排期：

| 文档 | 方向 | 状态 |
| --- | --- | --- |
| [agent-capability-strengthening.md](agent-capability-strengthening.md) | Agent 核心能力补强（文件新鲜度校验、真实 tokenizer、ripgrep 化 grep、edit_file 精确替换） | 已完成（2026-08-21） |
| [context-engineering.md](context-engineering.md) | 上下文工程（语义检索、prompt zones Phase 2、容量自适应、MCP 客户端补全） | §3 容量自适应已完成（2026-08-21），其余规划 |
| [codebase-health.md](codebase-health.md) | 代码库健康（巨型文件拆分、迁移残留清理、废弃工具下线、rebrand 收尾） | 规划 |
| [product-polish.md](product-polish.md) | 产品补全（原生多模态、配置精简、provider 分层提示词、notebook、git 工具、任务重连） | §1 原生多模态已完成（2026-08-22），其余规划 |

优先级建议（按对任务成功率的影响排序）：

1. 文件新鲜度校验 —— 直接消除"基于陈旧内容编辑"这一主要失败模式（agent-capability Stage 2）✅ 已完成
2. 真实 tokenizer + 按模型自适应压缩阈值 —— 上下文工程的地基（agent-capability Stage 4 ✅ → context-engineering §3 ✅ 已完成）
3. `host_executor.rs` 拆分 —— 后续一切引擎改动的可维护性前提（codebase-health §1）

## 执行约定（跨会话继续时必读）

后续会话执行这些计划时，除各计划文档自身的验收标准外，遵循以下约定
（源自 2026-08-21 执行 agent-capability 时的实际经验）：

1. **未提交改动隔离**：工作区可能存在用户未提交的在途改动（如 personality
   系列）。保持原样，不提交任何东西，除非用户明确要求。
2. **不要全仓库 `cargo fmt --all`**：本地 rustfmt 1.90 的格式规则（模块排序、
   链式换行）会重排 90+ 个无关文件，产生巨大 diff。只格式化本次改动的文件：
   `rustfmt --edition 2024 <改动文件>`。
3. **clippy 以"改动文件零错误"为准**：rustc 1.90 的 clippy 对仓库预存代码报
   ~62 个错误（`collapsible_if` 等 1.89+ 变严的 lint，集中在
   host_executor.rs / task_v2.rs / claudemd.rs），CI stable 升级后会踩到。
   执行计划时只需保证自己改动的文件零 clippy 错误；预存错误的统一修复属于
   codebase-health 范畴。
4. **每阶段验证**：改动 crate 的 `cargo test` 必须全绿；已知 flaky：
   agent-runtime 的 MCP streamable HTTP 测试（本地 mock 服务器时序），
   单独复跑即可确认；extensions 的 dylib fixture 测试失败时先
   `cargo build -p extensions-fixture-dylib` 再跑。
5. **已落地能力**（避免重复/冲突）：`edit_file` 已支持 replace_all/occurrence；
   文件新鲜度校验在 `agent-runtime/src/tools/freshness.rs`（包装器模式，
   `[features].file_freshness`）；`grep_files` 已是 ripgrep 引擎（ignore walker
   + grep-regex，支持 multiline）；token 计数统一走
   `agent-runtime/src/tokenizer.rs`（`[context].tokenizer_path` 注入 HF
   tokenizer，默认 chars÷3 启发式）。
