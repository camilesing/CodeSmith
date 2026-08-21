# 代码库健康改进计划

状态：规划
原则：每条独立成 PR，行为不变（parity 测试 + clippy -D warnings 守门）。

## 1. `host_executor.rs` 拆分

现状：`crates/agent-runtime/src/engine/host_executor.rs` 约 17,000 行
（含 ~700 行模块文档与内嵌测试）、79 处锁；是引擎迁移（Engine → HostAgentExecutor）
过程中的聚合产物。类似的还有 `crates/tui/src/runtime_threads.rs`（5,539 行）、
`tui/ui.rs`（9,006 行）、`tui/main.rs`（8,320 行）。

拆分路线（按模块文档 §1-§310 的既有分节）：

1. `engine/turn/` 新目录：`stream.rs`（reduce_stream + early-tool-start）、
   `batches.rs`（plan_tool_execution_batches + 并行执行）、`approval.rs`
   （request_approval + 批准竞争）、`seams.rs`（7 个取消检查点 + steer drain）、
   `postprocess.rs`（子代理回收 / thinking-only / 诊断收集）。
2. 内嵌测试随实现迁移到各文件 `#[cfg(test)]`，逐步转集成测试
   （`tests/protocol_recovery.rs` 已有先例）。
3. `runtime_threads.rs` 按同样思路拆 turn 生命周期 / 事件广播 / durable 任务。
4. 验收：`cargo clippy --workspace --all-features -- -D warnings` +
   workspace 测试全绿；文件行数目标 < 3,000。

## 2. 引擎迁移残留清理

- `#[allow(dead_code)]` 孤儿字段（`engine/mod.rs` 的 `tool_exec_lock`、
  `rx_user_input` 等）：确认无引用后删除。
- "retired with handle_deepseek_turn" 类指向已删代码的注释：成批清理。
- TUI 侧镜像模块（`tui/src/compaction/`、`tui/src/prompts.rs`、`tui/src/mcp.rs`、
  `tui/src/sandbox/`、`tui/src/execpolicy/`）：逐个确认为纯 re-export 后合并/删除
  （`tui/src/prompts.rs` 已是 re-export shim，可作模板）。
- 验收：`grep -rn "allow(dead_code)" crates/agent-runtime/src/engine/` 归零
  （或每处附 migration issue 链接）。

## 3. 废弃子代理工具下线

现状：`agent_spawn` / `agent_result` / `agent_wait` / `delegate_to_agent` 等
约 12 个 v0.8.33 起废弃的工具仍注册在目录中（`crates/tui/src/tools/subagent/`），
占用工具面与提示词预算。

方案：按 CHANGELOG 中的废弃承诺排期删除；`_deprecation` 提示改为
"tool no longer exists, use agent_open/agent_eval/agent_close"。工具目录
（`tool_catalog.rs`，27 个默认工具）随之缩减。验收：SWE-bench 评测跑一轮无回归。

## 4. Rebrand 收尾

现状：代码内仍有 "DeepSeek CLI" 模块文档、`~/.deepseek` 路径
（如 `crates/agent-runtime/src/tools/spec.rs:185` 的 workspace-trust 注释）、
legacy `deepseek-tui` npm 包与 shim（`crates/tui/src/bin/*legacy_shim.rs`，
计划 v0.9.0 移除）。

方案：按 `docs/REBRAND.md` 的清单推进；路径迁移做兼容读取（旧路径存在则
提示迁移）；v0.9.0 删除 legacy shim。验收：`grep -rn "deepseek" crates/ --include='*.rs'`
仅剩 provider 本名（deepseek provider 是合法功能名）与迁移兼容代码。

## 5. 依赖与构建卫生

- `crates/tui/Cargo.toml` 约 20 个内联版本号改 `workspace = true`
  （anyhow / axum / clap / chrono / reqwest / serde / tokio / toml 等），
  消除与 workspace 根的漂移风险。
- `reqwest` 的 `blocking` feature 仅保留在 cli/release 启动路径；
  `crates/tui/src/config.rs:4882` 的 Kimi OAuth 同步刷新改 async 或
  移出 config-load 关键路径。
- 新增 `clippy.toml` / `rustfmt.toml`（哪怕只是锁默认值 + 关键 lint 上调），
  让本地与 CI 行为一致；考虑 pre-commit。
- `hooks`（5 个测试）/ `tools`（4 个）/ `state` / `protocol` crate 补测：
  parity 测试之外补单元路径。

## 6. 文档体量治理

- `ROADMAP.md`（503KB）：已完成条目归档到 `docs/archive/roadmap/`，正文保持
  < 50KB 的活跃条目。
- `CHANGELOG.md`（318KB）：按版本年切分（CHANGELOG-2026H1.md 之类），保留索引。
- 补 `docs/TESTING.md`：测试分层（单元 / PTY 集成 / mock-LLM / parity）与
  本地复现命令。
