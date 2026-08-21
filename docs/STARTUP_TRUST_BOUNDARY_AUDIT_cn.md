# 启动信任边界审计

状态日期：2026-06-20

本审计刻意保持非破坏性。在当前的信任前/信任后分类经过评审并被拆分为
独立的实现变更之前，不移动任何启动行为。

## 范围

本审计覆盖活跃 Rust 运行时的启动和工作区信任边界：

- `crates/cli/src/lib.rs`
- `crates/tui/src/main.rs`
- `crates/tui/src/config.rs`
- `crates/tui/src/commands/config.rs`
- `crates/tui/src/tui/app.rs`
- `crates/tui/src/tui/onboarding/mod.rs`
- `crates/tui/src/tui/onboarding/trust_directory.rs`
- `crates/tui/src/tui/ui.rs`
- `crates/tui/src/hooks.rs`
- `crates/tui/src/tools/spec.rs`
- `crates/tui/src/workspace_trust.rs`

本审计关注 CodeSmith 首次读取或执行工作区敏感输入的时机，
以及这些操作发生在用户信任该工作区之前还是之后。

## 摘要

CodeSmith 已经具备工作区信任概念，但当前的信任提示
主要是一个 TUI 引导（onboarding）门。它尚不是一个硬性的启动流水线边界，
无法将所有安全的信任前初始化与所有项目敏感的
信任后初始化分离开来。

首个实现切片的状态：

- 早期的 `dotenv().ok()` 调用已被移除；交互式启动现在
  仅在允许启动工作区初始化时才显式加载 `workspace/.env`。
- 项目配置覆盖层被门控在同一启动边界之后；未受信任的
  交互式工作区在信任提示之前不会读取 `$WORKSPACE/.codesmith/config.toml`。
- `SessionStart` 钩子在 `OnboardingState::TrustDirectory` 可见期间被推迟，
  并在信任门通过后触发一次。

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
- 通过 `is_workspace_trusted(workspace)` 读取。
- 通过 `save_workspace_trust(workspace)` 和引导信任
  提示写入。
- `.deepseek/` 下的旧版工作区本地标记仍被
  `needs_trust(workspace)` 接受。

这是一个启动/引导决策。它与运行时 `trust_mode` 相关，但不等同于
运行时 `trust_mode`。

### 运行时 `trust_mode`

运行时 `trust_mode` 回答的问题是："在此会话期间，文件工具是否应绕过
常规的工作区路径边界？"

- 通过 `App`、会话状态和 `ToolContext` 传递。
- 由 YOLO 模式和 `/trust on` 启用。
- 用户接受引导信任提示时，也会为当前会话
  设置该模式。
- 在 `ToolContext::resolve_path()` 中，`trust_mode == true` 绕过常规的
  工作区路径检查。

这是一个宽泛的能力开关，不应与仅仅将工作区标记为
未来引导受信任相混淆。

### 按工作区外部路径允许列表

外部路径允许列表回答的问题是："当 `trust_mode` 为 false 时，
CodeSmith 文件工具可以访问该工作区之外的哪些具体路径？"

- 由 `/trust add <path>`、`/trust remove <path>` 和 `/trust list` 管理。
- 通过 `WorkspaceTrust::load_for(workspace)` 加载。
- 通过 `ToolContext::trusted_external_paths` 应用。
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
| CLI 分发器直接命令与 TUI 委托 | `crates/cli/src/lib.rs` | TUI 运行时之前 | 纯分发改动属信任前安全；命令特定行为各异 | 某些被委托的命令在没有共享启动边界文档的情况下进入 TUI。 | 保持分发器行为，但让 TUI 启动边界成为运行时事实来源。 |
| 进程加固 | `crates/tui/src/main.rs`、`crates/tui/src/sandbox/process_hardening.rs` | TUI `main()` 中极早期 | 信任前安全 | 防御性进程设置不应依赖工作区信任。 | 保持信任前。 |
| Panic 钩子 / 崩溃转储设置 | `crates/tui/src/main.rs` | TUI `main()` 中极早期 | 信任前安全 | 可能写入诊断状态，但不读取工作区控制的启动输入。 | 保持信任前；确保崩溃转储不泄露机密。 |
| 信号清理任务 | `crates/tui/src/main.rs` | TUI `main()` 中极早期 | 信任前安全 | 清理注册是进程作用域的。 | 保持信任前。 |
| 工作区 `.env` 加载 | `crates/tui/src/main.rs` | 交互式启动中，位于工作区解析和启动边界计算之后 | 仅信任后 / 显式绕过 | 旧的 `dotenvy::dotenv()` cwd 搜索已被移除。交互式启动现在仅加载 `workspace/.env`，且仅当工作区已被信任或被 YOLO/skip-onboarding 显式绕过时。非交互式 dotenv 策略仍待定。 | 保持显式路径加载。在后续切片中定义非交互式 dotenv 行为。 |
| CLI 参数解析 | `crates/tui/src/main.rs` | TUI `main()` 早期 | 信任前安全 | CLI 参数是用户提供的进程输入，不是仓库控制的文件。 | 保持信任前。 |
| 全局配置加载 | `crates/tui/src/main.rs`、`crates/tui/src/config.rs` | 运行时分发之前 | 信任前安全 | 用户拥有的全局配置可以启用稍后执行的钩子或路径。但它仍不是工作区控制的输入。 | 保持信任前；记录全局配置是受信任的用户输入。 |
| 日志设置 | `crates/tui/src/main.rs` | 命令分发之前 | 信任前安全 | 日志目的地可能包含来自用户配置的路径。 | 若仅来源于用户/CLI 配置则保持信任前。 |
| 工作区解析 | `crates/tui/src/main.rs` | `run_interactive()` 早期 / 命令特定路径 | 信任前安全 | 决定信任状态所需。 | 保持信任前。 |
| 工作区信任检查 | `crates/tui/src/config.rs`、`crates/tui/src/tui/onboarding/mod.rs` | App/引导状态构建期间 | 信任前安全 | 读取全局受信任工作区列表和旧版标记路径。旧版工作区标记是工作区本地输入。 | 为兼容性保留，但评审旧版标记是否应被视为充分的信任。 |
| 来自 `$WORKSPACE/.codesmith/config.toml` 或旧版 `.deepseek/config.toml` 的项目配置覆盖 | `crates/tui/src/main.rs` | 仅当允许启动工作区初始化时在 `run_interactive()` 中 | 仅信任后 / 显式绕过 | 未受信任的交互式工作区不再在信任提示之前读取项目配置。现有的拒绝列表仍作为针对已受信任/已绕过项目配置的纵深防御检查。接受信任后的运行时重新加载在本切片中未实现。 | 决定在信任接受后重新加载项目配置，还是仅在下次启动时生效。更新文档以匹配拒绝列表。 |
| 配置文件创建/迁移 | `crates/tui/src/main.rs` | TUI 启动之前 | 若仅涉及用户状态则信任前安全 | 写入用户配置/状态并可能迁移旧版配置。 | 若不应用工作区控制的输入则保持信任前。 |
| 系统技能安装 | `crates/tui/src/main.rs` | TUI 启动之前 | 若仅限捆绑/全局则信任前安全 | 将捆绑技能安装到用户状态不是工作区控制的，但工作区技能发现是独立的。 | 捆绑技能保持信任前；单独审计工作区本地技能发现。 |
| 工作区快照清理 | `crates/tui/src/main.rs` | TUI 启动之前 | 不确定 / 需要评审 | 使用工作区路径并删除旧的快照元数据。它很可能影响 CodeSmith 管理的状态，但工作区信任影响应当被记录。 | 对确切的存储目标分类，并仅将 CodeSmith 拥有的缓存清理保留在信任前。 |
| 溢出/截断缓存清理 | `crates/tui/src/main.rs` | TUI 启动之前 | 若仅限缓存则信任前安全 | CodeSmith 拥有的缓存维护。 | 若缓存路径是用户状态路径则保持信任前。 |
| 旧会话清理 | `crates/tui/src/main.rs` | TUI 启动之前 | 若仅限状态则信任前安全 | CodeSmith 拥有的会话/状态维护。 | 若不读取工作区控制的会话钩子/配置则保持信任前。 |
| App 构建与引导状态计算 | `crates/tui/src/tui/app.rs` | 事件循环之前 | 状态构建属信任前安全 | 构建 TUI 状态并决定是否显示 `TrustDirectory`。 | 保持信任前。 |
| 钩子执行器构建 | `crates/tui/src/tui/app.rs` | 引导信任提示被接受之前 | 仅当全局配置是唯一来源时信任前安全 | 钩子由用户配置，但后续执行可在未受信任的工作区上下文中运行命令。 | 构建可保持信任前；执行应被门控。 |
| SessionStart 钩子执行 | `crates/tui/src/tui/ui.rs` | 对已受信任/已绕过的启动在事件循环之前；`TrustDirectory` 可见期间被推迟 | 仅信任后 / 显式绕过 | 钩子执行器仍可在信任前由用户配置构建，但在工作区信任提示处于活动状态时 `SessionStart` 执行被抑制，并在门通过后触发一次。 | 保持一次性守卫。仅当某条路径可在引导期间到达消息/工具钩子时，才为其添加更宽泛的守卫。 |
| MessageSubmit 钩子 | `crates/tui/src/tui/ui.rs` | 用户消息分发期间 | 对工作区门控的会话属仅信任后 | 不应在工作区信任门处于活动状态时运行。 | 确认引导期间消息分发被阻塞；若尚未保证则添加守卫。 |
| 工具钩子 | `crates/tui/src/tui/tool_routing.rs` | 工具执行前后 | 信任后/工具策略控制 | 工具调用应仅在运行时信任和审批策略激活后发生。 | 保持在工具策略之下；记录与信任模式的关系。 |
| 用于计数/状态的 MCP 配置加载 | `crates/tui/src/tui/app.rs`、`crates/tui/src/mcp.rs` | App 构建期间 | 若来源于全局配置则信任前安全 | 项目配置当前拒绝 `mcp_config_path`；全局 MCP 配置是用户输入。 | 仅对全局配置保持信任前；不允许项目 MCP 配置在信任前加载。 |
| 工作区本地技能发现 | `crates/tui/src/tui/app.rs`、技能模块 | App 构建 / 工具目录构建期间 | 不确定 / 需要评审 | 工作区本地技能元数据可能是仓库控制的。加载文本比执行命令风险更低，但模型可见的指令可影响行为。 | 审计确切的发现和执行点。工作区本地技能倾向于信任后，或标记为受约束的信任前读取。 |
| 工具路径强制执行 | `crates/tui/src/tools/spec.rs`、`crates/tui/src/core/engine.rs` | 工具执行期间 | 信任后/工具策略控制 | `trust_mode` 绕过工作区路径检查；允许列表在未受信任时收窄外部访问。 | 保持强制执行集中在 `ToolContext`；记录三个信任概念。 |

## 发现

### 发现 1：`.env` 加载发生在工作区信任之前

当前 TUI 运行时在 CLI 解析之前、工作区解析之前调用 `dotenv().ok()`。
这意味着 `.env` 来源是进程 cwd，不一定是 `--workspace` 路径，
并且它在 CodeSmith 能够决定工作区是否受信任之前就被应用。

这应被视为信任后初始化，除非 CodeSmith 显式为 `.env`
选择不同的产品规则。

建议的跟进：

- 移除全局的早期 `dotenv().ok()` 调用。
- 先解析工作区。
- 仅在信任之后加载 `workspace.join(".env")`，或对非交互式
  命令采用显式选择加入。

### 发现 2：项目配置当前是受约束的信任前读取

交互式启动当前在引导信任提示之前合并项目配置。该合并是部分
受约束的：敏感字段（如 `api_key`、
`base_url`、`provider` 和 `mcp_config_path`）在项目作用域被拒绝，
某些策略字段也被约束。

该边界仍不是显式的。项目配置仍是仓库控制的输入，
并可通过被允许的字段影响运行时行为。

建议的跟进：

- 决定项目配置是否应为仅信任后。
- 若保持信任前，记录确切的被允许安全子集，并确保每个
  字段在信任之前只能降低能力。
- 更新配置文档以匹配已实现的拒绝列表。

### 发现 3：`SessionStart` 钩子可在信任接受之前运行

钩子执行器在 app 构建期间创建。基于全局用户拥有的配置构建它在信任前
是可以接受的。在用户接受工作区信任提示之前执行 `SessionStart`
更为敏感，因为钩子以
工作区上下文运行。

建议的跟进：

- 引导状态为 `TrustDirectory` 时不要执行 `SessionStart`。
- 在信任接受之后触发它，或者在需要该行为时定义单独的受限信任前
  钩子事件。

### 发现 4：信任提示尚不是完整的启动边界

当前的工作区信任提示控制引导 UI 和当前会话的
信任模式。它目前并不位于所有项目敏感读取与所有
运行时初始化之间。若干项目敏感的读取或操作可能发生在
提示之前。

建议的跟进：

- 在代码或文档中引入显式的启动阶段：
  1. 进程前置（process prelude）
  2. CLI 解析与全局配置
  3. 工作区解析与信任检查
  4. 受信任的项目初始化
  5. 运行时分发

## 后续实现候选

这些是实现候选，并非本审计所做的变更。

1. **信任感知的 `.env` 加载**
   - 将 `.env` 加载移至工作区解析和信任接受之后。
   - 相比 cwd 搜索，优先使用 `dotenvy::from_path(workspace.join(".env"))`。
   - 为非交互式 `exec`、`serve` 和命令模式定义行为。

2. **项目配置拆分**
   - 将项目配置拆分为 `pre_trust_project_config_subset` 和
     `post_trust_project_config`。
   - 信任前仅允许收紧能力的字段。
   - 将指令、备注路径和其他行为塑造字段移至
     信任后，除非获得显式批准。

3. **钩子执行门**
   - 将 `SessionStart` 推迟到引导完成。
   - 若信任提示处于活动状态时任何消息/工具钩子路径可达，
     则为其添加守卫。

4. **启动阶段辅助函数**
   - 考虑提取具名辅助函数，例如：
     - `init_process_pre_trust()`
     - `parse_cli_and_load_user_config()`
     - `resolve_workspace_trust()`
     - `init_project_post_trust()`
     - `dispatch_runtime()`

5. **文档清理**
   - 更新 `docs/CONFIGURATION.md`，描述实际的 `.env` 时机/来源以及
     项目配置拒绝列表。
   - 更新 `docs/MODES.md`，说明 `/trust` 无参数为状态查询、`/trust on` 启用
     运行时信任模式、`/trust add` 是更窄的外部路径选项。

## 验证清单

在更改启动行为之前：

1. 添加或更新测试，证明未受信任工作区的 `.env` 在信任之前
   不会被加载。
2. 添加或更新测试，证明项目配置在信任之前不能放宽审批、沙箱、
   shell、提供商、密钥或端点行为。
3. 添加或更新测试，证明 `SessionStart` 钩子在
   工作区信任提示处于活动状态时不运行。
4. 验证受信任工作区的持久化仍按产品预期抑制引导提示。
5. 验证 `/trust on`、`/trust off`、`/trust add`、`/trust remove` 和
   `/trust list` 仍然映射到其预期的运行时/文件工具语义。
6. 在任何启动重构之后，对交互式启动、`exec`、`serve --mcp`、`serve --http` 和
   `serve --acp` 进行冒烟测试。

## 状态

当本审计经过评审并从架构对齐 RFC 链接引用时，P0 的第一阶段交付物即告完成。
运行时行为变更应作为衍生自上述候选的独立后续工作项
进行跟踪。
