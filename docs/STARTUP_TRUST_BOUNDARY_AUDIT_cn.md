# 启动信任边界审计

状态日期：2026-06-20。2026-10-12 更新：ratatui TUI 于 2026-10-11 移除
（commits f88f4443/b47af98c，`crates/cli` 中的行式 REPL 成为唯一前端）后，
本审计已重新指向当前代码树，且"发现"一节已与已实现的启动边界对齐。

本审计刻意保持非破坏性。在当前的信任前/信任后分类经过评审并被拆分为
独立的实现变更之前，不移动任何启动行为。

## 范围

本审计覆盖单一 `codesmith` 二进制（位于 `crates/cli`）的启动和工作区
信任边界：

- `crates/cli/src/main.rs`（薄壳：`run_cli()`）
- `crates/cli/src/lib.rs`（CLI 分发器，`run_cli`）
- `crates/cli/src/main_root.rs`（启动主干：CLI 参数、panic 钩子、
  信号清理、工作区解析、启动信任边界（`WorkspaceInitBoundary`）、
  `.env` 加载、项目配置门控、serve / exec 入口）
- `crates/cli/src/interactive/`（行式 REPL 前端：状态、命令分发，
  含 `commands/config_cmds.rs` 中的 `/trust`）
- `crates/cli/src/hooks.rs` 与 `crates/agent-runtime/src/hooks.rs`
  （钩子执行器与 `HookEvent` 枚举）
- `crates/cli/src/mcp.rs`（MCP 配置加载）
- `crates/cli/src/workspace_trust.rs`（按工作区外部路径允许列表）
- `crates/agent-runtime/src/workspace_trust.rs`（持久化工作区信任）
- `crates/agent-runtime/src/onboarding.rs`（引导标记，`needs_trust`）
- `crates/agent-runtime/src/tools/spec.rs`（工具路径强制执行）
- `crates/agent-runtime/src/sandbox/process_hardening.rs`

审计历史：2026-06-20 的原始范围指向 ratatui TUI（`crates/tui/src/main.rs`、
`tui/app.rs`、`tui/onboarding/`、`tui/ui.rs`、`tui/config.rs` 等）。
这些文件已随 2026-10-11 的 TUI 移除而不存在；本审计现跟踪取代它们的
CLI/REPL 前端。

本审计关注 CodeSmith 首次读取或执行工作区敏感输入的时机，
以及这些操作发生在用户信任该工作区之前还是之后。

## 摘要

CodeSmith 具备工作区信任概念，并且自本审计最初日期以来，已实现显式
启动边界（`crates/cli/src/main_root.rs` 中的 `WorkspaceInitBoundary`）：
交互式启动先解析工作区与信任边界，工作区控制的初始化输入（`.env`、
项目配置、遥测落盘）都被门控在边界之后。原 TUI 引导门随 ratatui TUI
于 2026-10-11 一并移除；行式 REPL 没有引导状态推迟（引导标记仅是
簿记——见下文"信任概念"），边界现在完全由
`resolve_workspace_init_boundary` 及其调用方承载。

当前状态：

- 交互式启动通过 `dotenvy::from_path(workspace.join(".env"))` 显式加载
  `workspace/.env`，且仅当允许启动工作区初始化时
  （`load_workspace_dotenv_if_allowed`，
  `crates/cli/src/main_root.rs:4072`–`4094`，在 `:5407` 边界解析之后于
  `:5409` 调用）。树中已不存在基于 cwd 的 `dotenv().ok()` 调用。
- 项目配置覆盖层被门控在同一启动边界之后
  （`should_load_project_config` `:4065`–`4069`、`merge_project_config`
  定义于 `:5255`、在 `:5422`–`5423` 应用）；未受信任的交互式工作区在
  信任决策之前不会读取 `$WORKSPACE/.codesmith/config.toml`，且项目作用域
  拒绝列表（`:5297`–`5350`）仍作为针对已受信任/已绕过项目配置的纵深
  防御检查。
- TUI 中"`OnboardingState::TrustDirectory` 可见期间推迟 `SessionStart`
  钩子"的机制已随 TUI 移除。当前树中 `HookEvent::SessionStart` 仍存在于
  枚举（`crates/agent-runtime/src/hooks.rs:18`），但没有任何生产路径触发
  它，因此无从推迟；`OnboardingState::TrustDirectory` 仅以过期注释幸存
  （`crates/agent-runtime/src/engine/host_executor.rs:16942`、`:16948`）。

剩余的最高优先级跟进事项：

1. 决定项目配置应在信任接受后动态重新加载，
   还是仅在下一次启动时生效。
2. 为 `exec --auto`、`serve --mcp`、
   `serve --http` 和 `serve --acp` 定义非交互式信任策略。
3. 重新审视 `--skip-onboarding` 是否应继续绕过启动
   工作区初始化。
4. 在面向用户的文档中记录持久化工作区信任、运行时
   `trust_mode` 以及按工作区外部路径允许列表之间的区别。

## 信任概念

CodeSmith 目前有三个相关但彼此不同的信任概念。

### 持久化工作区信任 / 引导信任

持久化工作区信任回答的问题是："该工作区是否应再次显示引导
信任提示？"

- 以 `[projects."<workspace>"].trust_level =
  "trusted"` 的形式存储在全局配置中。
- 通过 `is_workspace_trusted(workspace)` 读取
  （`crates/agent-runtime/src/workspace_trust.rs:100`；级别解析在
  `:79`–`:99`）。
- 通过 `save_workspace_trust(workspace)` 写入
  （`crates/agent-runtime/src/workspace_trust.rs:142`）。注意：TUI 信任
  提示曾是它的交互式写入方；TUI 移除后该函数已无生产调用方（仅
  `crates/cli/src/config.rs:5617` 的测试使用），持久化信任目前只能通过
  手工编辑配置或下方的工作区本地标记建立。
- `.codesmith/` 下的工作区本地标记（`trusted`、`trust.json`）也被
  `needs_trust(workspace)` 接受
  （`crates/agent-runtime/src/onboarding.rs:39`–`46`）。

这是一个启动/引导决策。它与运行时 `trust_mode` 相关，但不等同于
运行时 `trust_mode`。随着 TUI 引导 UI 移除，引导标记
（`~/.codesmith/.onboarded`，`crates/agent-runtime/src/onboarding.rs:19`–`35`）
仅是簿记：干净启动会静默标记引导已完成
（`crates/cli/src/main_root.rs:5535`–`5548`），`/logout` 会移除该标记
（`crates/cli/src/interactive/commands/mod.rs:826`–`837`）；没有任何
交互式提示驱动它。

### 运行时 `trust_mode`

运行时 `trust_mode` 回答的问题是："在此会话期间，文件工具是否应绕过
常规的工作区路径边界？"

- 通过 `ReplState`（`crates/cli/src/interactive/state.rs`）、会话状态与
  `ToolContext`（`trust_mode` 字段，
  `crates/agent-runtime/src/tools/spec.rs:173`）传递。
- 由 YOLO 模式和 `/trust on`
  （`crates/cli/src/interactive/commands/config_cmds.rs:255`–`281`）启用。
- 原"接受引导信任提示时也为当前会话设置该模式"的路径属于 TUI 引导
  行为，已随之移除。
- 在 `ToolContext::resolve_path()`
  （`crates/agent-runtime/src/tools/spec.rs:526`）中，
  `trust_mode == true` 绕过常规的工作区路径检查。

这是一个宽泛的能力开关，不应与仅仅将工作区标记为
未来引导受信任相混淆。

### 按工作区外部路径允许列表

外部路径允许列表回答的问题是："当 `trust_mode` 为 false 时，
CodeSmith 文件工具可以访问该工作区之外的哪些具体路径？"

- 由 `/trust add <path>`、`/trust remove <path>` 和 `/trust list` 管理。
- 通过 `WorkspaceTrust::load_for(workspace)` 加载
  （`crates/cli/src/workspace_trust.rs:52`）。
- 通过 `ToolContext::trusted_external_paths` 应用
  （`crates/agent-runtime/src/tools/spec.rs:204`）。
- 仅授予通过 CodeSmith 文件工具访问的权限；它不会放宽 shell
  OS 沙箱。

## 分类规则

| 分类 | 含义 |
|---|---|
| 信任前安全 | 该操作不读取或执行工作区控制的输入，可在工作区信任之前发生。 |
| 受约束的信任前 | 该操作在信任之前读取工作区敏感输入，但代码当前约束了危险字段或副作用。该约束必须被记录并评审。 |
| 仅信任后 | 该操作读取、执行或应用工作区控制的输入，应仅在做出受信任工作区决策之后发生。 |
| 不确定 / 需要评审 | 从当前审计来看，该操作的数据来源、副作用或信任影响不够清晰。 |

## 启动操作审计

| 启动操作 | 位置 | 当前阶段 | 分类 | 风险 | 建议操作 |
|---|---|---|---|---|---|
| CLI 分发器直接命令与交互式委托 | `crates/cli/src/lib.rs`（`run_cli` `:418`）；交互式路径续于 `crates/cli/src/main_root.rs` | REPL 启动之前 | 纯分发改动属信任前安全；命令特定行为各异 | `run_interactive` 之外的委托命令在没有共享启动边界文档的情况下进入运行时。 | 保持分发器行为，但让 `main_root.rs` 启动边界成为运行时事实来源。 |
| 进程加固 | `crates/agent-runtime/src/sandbox/process_hardening.rs` | 已不再在启动时调用 | 不确定 / 需要评审 | `apply_process_hardening()`（`:53`）幸存，但 TUI 移除后已无生产调用方——TUI `main()` 曾是其唯一调用点。 | 决定 CLI 是否应在 Tokio runtime 之前调用它，还是将其退役。 |
| Panic 钩子 / 崩溃转储设置 | `crates/cli/src/main_root.rs:937`（`std::panic::set_hook`） | 启动中极早期 | 信任前安全 | 可能写入诊断状态，但不读取工作区控制的启动输入。 | 保持信任前；确保崩溃转储不泄露机密。 |
| 信号清理任务 | `crates/cli/src/main_root.rs:471`（`spawn_signal_cleanup_task`） | 启动中极早期 | 信任前安全 | 清理注册是进程作用域的。 | 保持信任前。 |
| 工作区 `.env` 加载 | `crates/cli/src/main_root.rs:4072`–`4094`（`load_workspace_dotenv_if_allowed`），于 `:5409` 调用 | 交互式启动中，位于工作区解析和启动边界计算（`resolve_workspace_init_boundary`，`:5407`）之后 | 仅信任后 / 显式绕过 | 交互式启动仅通过 `dotenvy::from_path` 加载 `workspace/.env`，且仅当工作区已被信任或被 YOLO/skip-onboarding 显式绕过时。非交互式 dotenv 策略仍待定。 | 保持显式路径加载。在后续工作中定义非交互式 dotenv 行为。 |
| CLI 参数解析 | `crates/cli/src/main_root.rs`（`Cli` 参数结构体） | 启动早期 | 信任前安全 | CLI 参数是用户提供的进程输入，不是仓库控制的文件。 | 保持信任前。 |
| 全局配置加载 | `crates/cli/src/main_root.rs`、`crates/cli/src/config.rs` | 运行时分发之前 | 信任前安全 | 用户拥有的全局配置可以启用稍后执行的钩子或路径。但它仍不是工作区控制的输入。 | 保持信任前；记录全局配置是受信任的用户输入。 |
| 日志设置 | `crates/cli/src/logging.rs`，由 `main_root.rs` 接线 | 命令分发之前 | 信任前安全 | 日志目的地可能包含来自用户配置的路径。 | 若仅来源于用户/CLI 配置则保持信任前。 |
| 工作区解析 | `crates/cli/src/main_root.rs` | `run_interactive()` 早期 / 命令特定路径 | 信任前安全 | 决定信任状态所需。 | 保持信任前。 |
| 工作区信任检查 | `crates/agent-runtime/src/workspace_trust.rs`、`crates/agent-runtime/src/onboarding.rs`（`needs_trust` `:39`–`46`），由 `resolve_workspace_init_boundary`（`main_root.rs:4053`）消费 | 启动边界解析期间 | 信任前安全 | 读取全局受信任工作区列表和工作区本地标记路径。工作区标记是工作区本地输入。 | 保留；评审工作区标记是否应被视为充分的信任。 |
| 来自 `$WORKSPACE/.codesmith/config.toml` 的项目配置覆盖 | `crates/cli/src/main_root.rs:5255`（`merge_project_config`），在 `:5422`–`5423` 门控 | 仅当允许启动工作区初始化时在 `run_interactive()` 中 | 仅信任后 / 显式绕过 | 未受信任的交互式工作区不会在信任决策之前读取项目配置。拒绝列表（`:5297`–`5350`）仍作为针对已受信任/已绕过项目配置的纵深防御检查。接受信任后的运行时重新加载未实现。 | 决定在信任接受后重新加载项目配置，还是仅在下次启动时生效。更新文档以匹配拒绝列表。 |
| 配置文件创建/迁移 | `crates/cli/src/main_root.rs:5458`–`5467`（`ensure_config_file_exists`） | REPL 启动之前 | 若仅涉及用户状态则信任前安全 | 写入用户配置/状态。 | 若不应用工作区控制的输入则保持信任前。 |
| 系统技能安装 | `crates/cli/src/main_root.rs:5488`–`5492`（`install_system_skills`） | REPL 启动之前 | 若仅限捆绑/全局则信任前安全 | 将捆绑技能安装到用户状态不是工作区控制的，但工作区技能发现是独立的。 | 捆绑技能保持信任前；单独审计工作区本地技能发现。 |
| 工作区快照清理 | `crates/cli/src/main_root.rs:5493`–`5499`（`prune_workspace_snapshots`） | REPL 启动之前 | 不确定 / 需要评审 | 使用工作区路径并删除旧的快照元数据。它很可能影响 CodeSmith 管理的状态，但工作区信任影响应当被记录。 | 对确切的存储目标分类，并仅将 CodeSmith 拥有的缓存清理保留在信任前。 |
| 溢出/截断缓存清理 | `crates/cli/src/main_root.rs:5501`–`5520`（`truncate::prune_older_than`） | REPL 启动之前 | 若仅限缓存则信任前安全 | CodeSmith 拥有的缓存维护。 | 若缓存路径是用户状态路径则保持信任前。 |
| 旧会话清理 | `crates/cli/src/main_root.rs:5522`–`5526`（`cleanup_old_sessions`） | REPL 启动之前 | 若仅限状态则信任前安全 | CodeSmith 拥有的会话/状态维护。 | 若不读取工作区控制的会话钩子/配置则保持信任前。 |
| 引导标记簿记 | `crates/agent-runtime/src/onboarding.rs:19`–`35`；`crates/cli/src/main_root.rs:5535`–`5548` | 启动边界之后、REPL 启动之前 | 信任前安全 | 原 TUI 引导状态计算（`App` 构建时决定是否显示 `TrustDirectory`）已不存在；干净启动静默标记引导完成，`/logout` 移除标记（`interactive/commands/mod.rs:826`–`837`）。 | 保持信任前；该标记不再门控任何交互式内容。 |
| REPL 状态构建 | `crates/cli/src/interactive/state.rs`（`ReplState`） | REPL 事件循环之前 | 状态构建属信任前安全 | 构建行式 REPL 状态；不存在可显示的引导信任界面。 | 保持信任前。 |
| 钩子执行器构建 | `crates/cli/src/interactive/state.rs:251`（`HookExecutor::new`） | REPL 启动之前 | 仅当全局配置是唯一来源时信任前安全 | 钩子由用户配置，但后续执行可在未受信任的工作区上下文中运行命令。 | 构建可保持信任前；执行应被门控。 |
| SessionStart 钩子执行 | 无生产触发点（事件定义于 `crates/agent-runtime/src/hooks.rs:18`；TUI `ui.rs` 触发点已于 2026-10-11 移除） | 不适用 | 仅信任后 / 显式绕过（当前不可达） | 当前树中没有任何代码触发 `SessionStart`，因此原"`TrustDirectory` 可见期间推迟"的守卫已无意义。若触发点回归需重新引入门控。 | 若 `SessionStart` 再次获得调用方，重新添加信任门。 |
| MessageSubmit 钩子 | `crates/cli/src/hooks.rs:328`（`execute_message_submit_transform`），经 `HookHost` trait 在用户消息分发期间到达 | REPL 中的用户消息分发期间 | 对工作区门控的会话属仅信任后 | REPL 仅在启动完成后分发消息；引导门移除后，REPL 内不存在信任界面期间提交消息的可能。 | 确认消息分发不会在启动完成前运行；若出现触发路径则添加守卫。 |
| 工具钩子 | `crates/agent-runtime/src/callback_bridge.rs:259` / `:306`（`ToolCallBefore` / `ToolCallAfter`） | 工具执行前后 | 信任后/工具策略控制 | 工具调用应仅在运行时信任和审批策略激活后发生。 | 保持在工具策略之下；记录与信任模式的关系。 |
| 用于计数/状态的 MCP 配置加载 | `crates/cli/src/mcp.rs`，由 `main_root.rs` 接线（`mcp_config_path` 消费点 `:1973`、`:2247`、`:2766`） | 启动 / REPL 构建期间 | 若来源于全局配置则信任前安全 | 项目配置当前拒绝 `mcp_config_path`；全局 MCP 配置是用户输入。 | 仅对全局配置保持信任前；不允许项目 MCP 配置在信任前加载。 |
| 工作区本地技能发现 | `crates/agent-runtime/src/skills/mod.rs`（注册表）、`crates/cli/src/skills/` | 工具目录构建期间 | 不确定 / 需要评审 | 工作区本地技能元数据可能是仓库控制的。加载文本比执行命令风险更低，但模型可见的指令可影响行为。 | 审计确切的发现和执行点。工作区本地技能倾向于信任后，或标记为受约束的信任前读取。 |
| 工具路径强制执行 | `crates/agent-runtime/src/tools/spec.rs`（`resolve_path` `:526`），经 `crates/cli/src/core/engine.rs` 构建桥接到达 | 工具执行期间 | 信任后/工具策略控制 | `trust_mode` 绕过工作区路径检查；允许列表在未受信任时收窄外部访问。 | 保持强制执行集中在 `ToolContext`；记录三个信任概念。 |

## 发现

### 发现 1：`.env` 加载发生在工作区信任之前 —— 已解决

原始发现（2026-06-20）：当时的 TUI 运行时在 CLI 解析之前、工作区解析之前
调用 `dotenv().ok()`。这意味着 `.env` 来源是进程 cwd，不一定是
`--workspace` 路径，并且它在 CodeSmith 能够决定工作区是否受信任之前
就被应用。

解决情况：树中已不存在任何 `dotenv().ok()` 调用。建议已被实现——
`load_workspace_dotenv_if_allowed`
（`crates/cli/src/main_root.rs:4072`–`4094`）仅在启动边界允许工作区
初始化时加载 `dotenvy::from_path(workspace.join(".env"))`，且在
`:5407` 的 `resolve_workspace_init_boundary(&workspace, cli.skip_onboarding,
startup_yolo)` 之后于 `:5409` 调用。项目配置门控使用同一边界
（`should_load_project_config` `:4065`–`4069` / `merge_project_config`
在 `:5421`–`5423` 应用）。非交互式 dotenv 行为（exec/serve）仍是该
发现的未决尾巴。

### 发现 2：项目配置是受约束的信任前读取 —— 门控层面已基本解决

原始发现（2026-06-20）：交互式启动在引导信任提示之前合并项目配置，
仅由字段拒绝列表部分约束。

当前状态：合并现在被门控在启动边界之后——未受信任的交互式工作区在
信任决策之前完全不读取 `$WORKSPACE/.codesmith/config.toml`
（`should_load_project_config` / `merge_project_config`，
`crates/cli/src/main_root.rs:5421`–`5423`）。对已受信任或显式绕过的
工作区，拒绝列表仍拒绝项目作用域的 `api_key` / `base_url` /
`provider` / `mcp_config_path` 并约束策略字段（`:5297`–`5350`，理由
记录为 #417），作为纵深防御。

建议的跟进：

- 决定项目配置对非交互式路径（`exec`、`serve`）是否也应仅信任后——
  这些路径的边界目前由 flag 决定。
- 保持拒绝列表有文档并与配置文档同步。
- 决定会话中途做出信任决策后项目配置是否重新加载（当前：仅下次启动）。

### 发现 3：`SessionStart` 钩子可在信任接受之前运行 —— TUI 移除后已无意义

原始发现（2026-06-20）：钩子执行器在 app 构建期间创建，`SessionStart`
可能在用户接受工作区信任提示之前执行。

当前状态：TUI 引导门及其 `TrustDirectory` 状态机已于 2026-10-11 移除。
`HookEvent::SessionStart` 仍在枚举中
（`crates/agent-runtime/src/hooks.rs:18`），但当前树中没有生产路径触发
它（已验证：只有枚举、其 `as_str` 映射与测试引用它），因此已无从推迟。
真正会触发的工具路径钩子（`ToolCallBefore`/`ToolCallAfter`，
`crates/agent-runtime/src/callback_bridge.rs:259`/`:306`）在工具执行
前后运行，只发生在启动之后并处于工具策略之下。

建议的跟进：

- 若 `SessionStart` 再次获得生产触发点，仅在启动边界允许工作区初始化
  之后执行它（`WorkspaceInitBoundary` 检查是天然的门）。

### 发现 4：信任提示尚不是完整的启动边界 —— 部分解决

原始发现（2026-06-20）：工作区信任提示控制引导 UI 与当前会话的信任
模式，但并不位于所有项目敏感读取与所有运行时初始化之间。

当前状态：显式启动边界已经存在——
`WorkspaceInitBoundary`（`crates/cli/src/main_root.rs:4042`–`4063`）
由持久化信任 / `--skip-onboarding` / YOLO 解析，并门控 `.env` 加载、
项目配置合并与遥测落盘（`attach_telemetry_if_trusted`）。交互式信任
提示移除后，边界是计算出来的，而不是提示出来的。剩余缺口：非交互式
入口（`exec --auto`、`serve --mcp`/`--http`/`--acp`）从 flag/持久化
解析信任，没有显式文档化的策略；`apply_process_hardening()` 不再被
任何启动路径调用（见启动操作审计表）。

建议的跟进：

- 保持该边界作为唯一缝隙，并记录它实现的启动阶段：
  1. 进程前置（process prelude）
  2. CLI 解析与全局配置
  3. 工作区解析与信任边界
  4. 受信任的项目初始化
  5. 运行时分发
- 定义非交互式信任策略（摘要跟进事项 2）。

## 后续实现候选

这些是实现候选，并非本审计所做的变更。

1. **信任感知的 `.env` 加载** —— 已实现
   - `load_workspace_dotenv_if_allowed` 在启动边界之后加载
     `dotenvy::from_path(workspace.join(".env"))`
     （`crates/cli/src/main_root.rs:4072`–`4094`，调用点 `:5409`）。
   - 仍未决：非交互式 `exec`、`serve` 与命令模式的行为。

2. **项目配置拆分**
   - 将项目配置拆分为 `pre_trust_project_config_subset` 和
     `post_trust_project_config`。
   - 信任前仅允许收紧能力的字段。
   - 将指令、备注路径和其他行为塑造字段移至
     信任后，除非获得显式批准。
   - 当前状态注记：交互式路径已完全不在信任前读取项目配置；该候选
     现在主要关乎非交互式路径。

3. **钩子执行门** —— 按原文已无意义（2026-10-12）
   - 原本要用其可见性门控 `SessionStart` 的 TUI 引导界面已不存在，
     且当前树中没有生产路径触发 `SessionStart`。若 `SessionStart`
     重获触发点，重新打开该候选。

4. **启动阶段辅助函数**
   - 考虑提取具名辅助函数，例如：
     - `init_process_pre_trust()`
     - `parse_cli_and_load_user_config()`
     - `resolve_workspace_trust()`
     - `init_project_post_trust()`
     - `dispatch_runtime()`
   - 当前状态注记：`resolve_workspace_init_boundary` 已在
     `main_root.rs` 中扮演 `resolve_workspace_trust()` 的角色。

5. **文档清理**
   - 更新 `docs/CONFIGURATION.md`，描述实际的 `.env` 时机/来源以及
     项目配置拒绝列表。
   - 更新 `docs/PRESETS.md`，说明 `/trust` 无参数为状态查询、`/trust on` 启用
     运行时信任模式、`/trust add` 是更窄的外部路径选项。

## 验证清单

在更改启动行为之前：

1. 添加或更新测试，证明未受信任工作区的 `.env` 在信任之前
   不会被加载（既有守卫：`load_workspace_dotenv_if_allowed` 测试见
   `crates/cli/src/main_root.rs:7421`–`7429`；
   `should_load_project_config` 测试见 `:7374`–`7390`）。
2. 添加或更新测试，证明项目配置在信任之前不能放宽审批、沙箱、
   shell、提供商、密钥或端点行为。
3. 若 `SessionStart` 获得生产触发点，添加测试证明它在启动边界阻止
   工作区初始化期间不运行。
4. 验证受信任工作区的持久化仍使启动直接越过边界而不再提示（行式
   REPL 中没有交互式提示；`~/.codesmith/.onboarded` 标记仅是簿记）。
5. 验证 `/trust on`、`/trust off`、`/trust add`、`/trust remove` 和
   `/trust list` 仍然映射到其预期的运行时/文件工具语义
   （`crates/cli/src/interactive/commands/config_cmds.rs:255`–`281`）。
6. 在任何启动重构之后，对交互式启动、`exec`（`--auto` flag 见
   `crates/cli/src/main_root.rs:340`）、`serve --mcp`、`serve --http` 和
   `serve --acp`（flag 见 `:684`–`698`，单模式检查见 `:761`）进行
   冒烟测试。

## 状态

当本审计经过评审并从架构对齐 RFC 链接引用时，P0 的第一阶段交付物即告完成。
运行时行为变更应作为衍生自上述候选的独立后续工作项
进行跟踪。
