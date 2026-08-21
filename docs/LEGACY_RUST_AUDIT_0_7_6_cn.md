# v0.7.6 遗留 Rust 审计

状态日期：2026-04-29

本次审计刻意保持非破坏性。除非测试证明公开 CLI、已保存会话、工具 schema 和已载入文档的命令路径不再依赖某段兼容代码，否则 v0.7.6 不会移除它。

## 摘要

| 面 | 归属模块 | 当前消费者 | 引用检查 | 保留的兼容原因 | 当前警告 | 建议动作 |
|---|---|---|---|---|---|---|
| 遗留 MCP 同步 API（`McpServerInput`、`list`、`add`、`remove`、`call_tool`、`load_legacy`） | `crates/tui/src/mcp.rs` | 未接入当前 `/mcp` 命令路径；靠 `#[allow(dead_code)]` 保留 | 已检查直接的 Rust 引用和当前 MCP 命令路径；已保存/配置 JSON 的兼容性仍需专门的冒烟测试 | 在异步 MCP 管理器作为活跃路径的同时，保留包含 `mcpServers` 别名和同步调用助手的旧 JSON 形状 | 仅有代码 TODO | 收敛到显式的遗留模块中，或在 CLI/运行时对齐测试证明无调用方使用后再移除。由 #218 跟踪。 |
| 遗留提示词常量/函数（`AGENT_PROMPT`、`YOLO_PROMPT`、`PLAN_PROMPT`、`base_system_prompt`、`normal_system_prompt` 等） | `crates/tui/src/prompts.rs` | 仍在直接导入提示词常量的测试和旧调用方 | 直接 Rust 引用仍然存在；尚未证明公开 crate 和旧 harness 不再导入 | 分层提示词 API 取代了单体提示词，但旧调用点可能仍按常量编译 | 无 | 在 v0.7.6 中保留；仅在内部调用方迁移完成后再添加弃用标注。由 #219 跟踪。 |
| `/compact` 斜杠命令的定位 | `crates/tui/src/commands/mod.rs` | 公开斜杠命令注册表和帮助浮层 | 公开命令注册表/文档路径仍引用它 | 当前的循环/接缝策略更倾向重启/循环流程，但用户仍可能手动运行 `/compact` | 描述标注为遗留并指向循环重启 | 作为手动兼容命令保留；在上下文/token 问题解决之前不要移除。 |
| `todo_*` 兼容工具 | `crates/tui/src/tools/todo.rs` | 仍在使用 `todo_add`、`todo_update`、`todo_list`、`todo_write` 的工具注册表/模型调用 | 工具注册表兼容性与已保存工具调用风险仍然存在 | `checklist_*` 是规范名称，但旧工具名可能出现在已保存提示词、trace 或模型先验中 | 元数据标记 `compat_alias: true`；描述说明是兼容别名 | 先添加带目标版本的显式弃用元数据，再在拿到工具 schema 迁移证据后移除。由 #220 跟踪。 |
| 已弃用的子智能体别名工具（`spawn_agent`、`send_input`、delegate 别名） | `crates/tui/src/tools/subagent/mod.rs` | 工具注册表和模型/工具调用兼容性 | 工具注册表兼容性与已保存工具调用风险仍然存在 | 规范名称是 `agent_spawn`、`agent_send_input` 等；别名保留旧的工具调用兼容性 | `_deprecation` 元数据和 tracing 警告；移除目标是 `v0.8.0` | 保留至 v0.7.x 结束；移除所需元数据已就位。由 #221 跟踪。 |
| 遗留的根/provider TOML `api_key` 兼容 | `crates/tui/src/config.rs`、`crates/config/src/lib.rs` | 配置解析器；配置文件中已有 `api_key` 的用户 | 公开配置加载和文档仍提及迁移行为 | 更倾向 Keyring 迁移，但破坏现有配置会阻断启动/认证 | tracing 警告指向 `deepseek auth set` / `deepseek auth migrate` | 保留；这些警告可引导用户采取行动。移除应等待迁移命令和发布说明窗口。 |
| 模型别名规范化（`deepseek-chat`、`deepseek-reasoner`、更旧的 V3/R1 别名） | `crates/tui/src/config.rs`、`crates/config/src/lib.rs` | 配置/环境变量/模型选择器归一化 | 公开文档和现有配置仍可能使用别名 | 保留过去已载入文档的 DeepSeek 别名并映射到 `deepseek-v4-flash` | 设计上即为静默认别名 | 保留；移除别名只会破坏现有配置而没有实际收益。 |
| 已弃用的调色板常量和别名 | `crates/tui/src/palette.rs`、`crates/tui/tests/palette_audit.rs` | 现有调用点和审计测试 | 调色板审计强制执行剩余的允许列表 | 更倾向语义别名，但保留旧常量是为了避免大范围样式变动 | 调色板审计会阻止在允许列表之外直接使用已弃用项 | 保留别名；继续伺机把调用点迁移到语义角色。 |

## 后续移除候选

以下内容在 v0.7.6 中移除并不安全：

1. #218 遗留 MCP 同步 API：需要对 `/mcp`、`deepseek mcp` 和 MCP 服务器校验流程做调用图检查和显式的 CLI/运行时对齐测试。
2. #219 遗留提示词常量/函数：需要证明没有公开 crate 或旧测试 harness 导入它们。
3. #220 `todo_*` 工具别名：需要弃用元数据和已保存 trace/工具 schema 的迁移窗口。
4. #221 已弃用的子智能体别名工具：移除目标已设定为 `v0.8.0`，但实际移除应单独跟踪并测试。

## 验证清单

在移除任何兼容面之前：

1. 用 `rg` 搜索直接的 Rust 引用。
2. 搜索文档和 README 中的命令示例。
3. 以全部特性运行工作区测试。
4. 如果该面影响工具 schema 或持久化历史，运行已保存会话/工具调用兼容性冒烟测试。
5. 保留发布说明条目；对于用户可见的配置/工具变更，至少在一个次要版本中保留迁移提示。
