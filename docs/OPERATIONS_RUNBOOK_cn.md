# codesmith 运维手册

本手册涵盖本地 CLI/TUI 运行时的实用调试与事件响应。

## 快速分诊

1. 确认二进制与配置：
   - `cargo run -p codesmith-tui -- --version`
   - `cat ~/.codesmith/config.toml`（或查看已配置的 profile）
2. 开启详细日志：
   - `RUST_LOG=codesmith_tui=debug cargo run -p codesmith-tui`（tracing 输出写入 `~/.codesmith/logs/tui-YYYY-MM-DD-<pid>.log`）
   - 查看提供商 HTTP 重试/重连：`RUST_LOG=codesmith_providers=debug,codesmith_agent=debug cargo run -p codesmith-tui`
3. 捕获当前状态：
   - `ls ~/.codesmith/sessions`
   - `ls ~/.codesmith/sessions/checkpoints`
   - `ls ~/.codesmith/tasks`

## 事件：回合挂起或流式输出停止

症状：
- TUI 一直处于加载状态
- 助手输出不完整且没有结束

检查：
1. 检查重试/健康日志（`~/.codesmith/logs/` 中的 `codesmith_providers` / `codesmith_agent` target）
2. 验证端点连通性：
   - `curl -sS https://api.deepseek.com/beta/models -H "Authorization: Bearer $DEEPSEEK_API_KEY"`
3. 确认工具输出中不存在本地沙箱/权限死锁

处置：
1. 如果有前台 shell 命令正在运行，按 `Ctrl+B`，选择将其转入后台或取消当前回合。
2. 如果命令已在后台启动，让助手用 `exec_shell_cancel` 和返回的任务 id 取消它。
3. 想停止请求本身时，用 `Esc` 或 `Ctrl+C` 中断当前回合。
4. 重试提示词；如果仍然失败，重启 TUI。
5. 重启后，确认之前排队中/进行中的运行时回合显示为已中断，而不是停留在运行中状态。

## 事件：网络中断 / 离线行为

预期行为：
- 离线模式激活时，新提示词会进入队列
- 队列状态持久化到 `~/.codesmith/sessions/checkpoints/offline_queue.json`

检查：
1. 在 TUI 中打开队列：`/queue list`
2. 确认持久化的队列文件存在且时间戳在更新

处置：
1. 恢复网络连接
2. 重新发送队列中的条目（通过 `/queue edit <n>` + Enter，或正常输入流程）
3. 确保队列为空时队列文件被清除

## 事件：需要崩溃恢复

预期行为：
- 检查点存储于 `~/.codesmith/sessions/checkpoints/latest.json`
- 除非提供 `--resume`/`--continue`，否则启动时开始全新会话

处置：
1. 通过 `codesmith --resume <id>` 或 TUI 中的 `Ctrl+R` 显式恢复之前的工作
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
3. 在 TUI 历史/日志中检查沙箱拒绝记录

处置：
1. 带上所需审批后重试（或仅在合适时使用 YOLO 模式）
2. 暂时禁用出错的 MCP 服务器以隔离问题
3. 通过 `/mcp` 诊断验证后再重新启用

## 遥测（本地 jsonl 数据汇）

CodeSmith 不附带任何联网遥测。选配的 `telemetry = true` 配置标志会将容量
决策分析事件写入 `~/.codesmith/telemetry/events.jsonl` 这个**仅本地**的
jsonl 文件。

- 默认关闭；该标志未设置或为 `false` 时，文件永远不会被创建。
- 数据汇在信任判定之前构造（事件先在内存中排队），只有在通过工作区信任
  边界之后才开始写入——因此在获得同意之前，任何受工作区控制的数据都
  不会落盘。
- 事件携带临时的 `telemetry_session_id`（每次会话重新生成，不持久化）；
  有意不发出持久线程 id。

查看：`cat ~/.codesmith/telemetry/events.jsonl | jq .`
禁用：在 `~/.codesmith/config.toml` 中取消设置 `telemetry`（或设为
`telemetry = false`）。该文件可随时安全删除。

## 事后检查清单

1. 保留日志和相关状态文件
2. 记录触发原因、影响范围和缓解措施
3. 新增或更新回归测试（重试/恢复/schema）
4. 若行为有变，更新本手册和架构文档
