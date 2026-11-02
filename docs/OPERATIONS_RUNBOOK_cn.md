# codesmith 运维手册

本手册涵盖本地 CLI 运行时的实用调试与事件响应。

## 快速分诊

1. 确认二进制与配置：
   - `cargo run -p codesmith-cli -- --version`
   - `cat ~/.codesmith/config.toml`（或查看已配置的 profile）
2. 开启详细日志（仅输出到控制台；没有文件日志）：
   - `CODESMITH_LOG_LEVEL=debug cargo run -p codesmith-cli`（或传 `--verbose`；`info`/`trace` 同样生效）
   - `RUST_LOG` 不会被 CLI 自身读取——它只会传递给派生的子进程
3. 捕获当前状态：
   - `ls ~/.codesmith/sessions`
   - `ls ~/.codesmith/sessions/checkpoints`
   - `ls ~/.codesmith/tasks`

## 事件：回合挂起或流式输出停止

症状：
- REPL 状态行持续显示回合处于运行中
- 助手输出不完整且没有结束

检查：
1. 以 `CODESMITH_LOG_LEVEL=debug`（或 `--verbose`）重跑，在控制台观察重试/健康消息
2. 验证端点连通性：
   - `curl -sS https://api.deepseek.com/beta/models -H "Authorization: Bearer $DEEPSEEK_API_KEY"`
3. 确认工具输出中不存在本地沙箱/权限死锁

处置：
1. 按 `Ctrl+C` 取消当前回合，包括它正在运行的前台 shell 命令。
2. 如果命令已在后台启动，让助手用 `exec_shell_cancel` 和返回的任务 id 取消它。
3. 重试提示词；如果仍然失败，重启 REPL。
4. 重启后，确认之前排队中/进行中的运行时回合显示为已中断，而不是停留在运行中状态。

## 事件：网络中断 / 离线行为

预期行为：
- 离线模式激活时，新提示词会进入队列
- 队列状态持久化到 `~/.codesmith/sessions/checkpoints/offline_queue.json`

检查：
1. 在 REPL 中运行 `/queue` —— 离线队列是 composer 时代的特性，目前没有
   行模式等价物；该命令会提示回合期间发送的消息通过 `Ctrl+C` 转向/取消
2. 确认持久化的队列文件存在且时间戳在更新

处置：
1. 恢复网络连接
2. 通过正常输入流程重新发送队列中的条目（`/queue edit <n>` 是 composer
   命令，没有行模式等价物）
3. 确保队列为空时队列文件被清除

## 事件：需要崩溃恢复

预期行为：
- 检查点存储于 `~/.codesmith/sessions/checkpoints/latest.json`
- 除非提供 `--resume`/`--continue`，否则启动时开始全新会话

处置：
1. 通过 `codesmith --resume <id>`，或在 REPL 中使用 `/resume <id>` 显式恢复之前的工作
2. 如需检查检查点，查看 `latest.json` 以了解 schema 不匹配等细节
3. 如果 schema 比二进制支持的更新，升级二进制或删除过期的检查点

## 事件：持久化状态 schema 错误

症状：
- 出现类似 `schema vX is newer than supported vY` 的错误

受影响的存储：
- 会话（`~/.codesmith/sessions/*.json`）
- 运行时线程/回合/条目记录
- 任务（`~/.codesmith/tasks/tasks/*.json`）

处置：
1. 确认二进制版本与迁移预期
2. 编辑前先备份状态目录
3. 二选一：
   - 使用更新的兼容二进制运行，或
   - 归档不兼容记录并重建状态

## 事件：MCP/工具执行失败

检查：
1. 校验 `~/.codesmith/mcp.json` 的 schema 与服务器命令路径
2. 确认服务器进程可以手动启动
3. 在 REPL 会话记录/日志中检查沙箱拒绝记录

处置：
1. 带上所需审批后重试（或仅在合适时使用 YOLO 模式）
2. 暂时禁用出错的 MCP 服务器以隔离问题
3. 通过 `/mcp` 诊断验证后再重新启用

## 遥测（本地 jsonl 数据汇）

CodeSmith 默认不附带任何联网遥测。选配的 `telemetry = true` 配置标志会将
容量决策分析事件写入 `~/.codesmith/telemetry/events.jsonl` 这个**仅本地**
的 jsonl 文件。

- 默认关闭；该标志未设置或为 `false` 时，文件永远不会被创建。
- 数据汇在信任判定之前构造（事件先在内存中排队），只有在通过工作区信任
  边界之后才开始写入——因此在获得同意之前，任何受工作区控制的数据都
  不会落盘。
- 事件携带临时的 `telemetry_session_id`（每次会话重新生成，不持久化）；
  有意不发出持久线程 id。

查看：`cat ~/.codesmith/telemetry/events.jsonl | jq .`
禁用：在 `~/.codesmith/config.toml` 中取消设置 `telemetry`（或设为
`telemetry = false`）。该文件可随时安全删除。

## 遥测（选配 OTLP/HTTP 导出）

config.toml 的 `[telemetry_otlp]` 开启向自选收集器（如 `:4318` 的
OpenTelemetry Collector）的 OTLP/HTTP-JSON 导出。这是选择加入的联网遥测
——端点由运维者配置，收集器能看到什么也由运维者负责。

- 表不存在（或 `url` 为空）⇒ 导出器不存在，绝不发出任何网络调用；两个
  通道都关时导出器同样零调用（有测试保障的合同）。
- 两个独立开关的通道：`events`（容量决策等，表存在时默认 `true`）与
  `session_log`（字节封顶的按回合摘要，默认 `false`，硬上限
  256 KiB/进程）。
- 记录为每 POST 一条 OTLP logs 记录，发往 `<url>/v1/logs`，即发即忘：
  收集器不可用时记录被丢弃，绝不阻塞或破坏引擎，没有重试。
- 导出只在通过工作区信任边界后开始；信任前的记录有界地在内存中排队
  （256 条，最老先丢）。
- 记录携带稳定的匿名安装 id（`~/.codesmith/anonymous-id`，随机 UUID，
  只生成一次）。它只被导出器读取，只出现在 OTLP resource attributes
  中，绝不进入模型请求、会话文件或提示词。删除该文件即可轮换 id。
- 收集器收到的内容：镜像的容量/回合摘要计数器与 id——绝无文件路径、
  代码或提示词内容（字符串字段经过
  `crates/agent-runtime/src/telemetry.rs` 的脱敏屏障）。

禁用：删除 `[telemetry_otlp]` 表（或清空其 `url`）。

## 事后检查清单

1. 保留日志和相关状态文件
2. 记录触发原因、影响范围和缓解措施
3. 新增或更新回归测试（重试/恢复/schema）
4. 若行为有变，更新本手册和架构文档
