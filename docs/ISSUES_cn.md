# 问题台账（Issue Ledger）

本文件是代码与文档中所有 issue 引用的统一归档记录（整理于 2026-08-24）。
英文版见 [ISSUES.md](ISSUES.md)。

**收录规则：**

- 指向**本仓库 GitHub issue** 的引用（`#N` 编号与
  `github.com/camilesing/CodeSmith/issues` 链接）视为不存在，**不予收录**。
  原文段落若描述了实际问题但带有此类编号，则收录描述、去掉编号。
- 其余全部收录：内部 `CX#N` 编号、安全报告（HackerOne、GHSA 通告）、
  跨仓库 `Whalescale#N` 引用、以及无编号的已知问题 / TODO / 限制。
- `.zcode/plans/`（本地工具会话产物）不在范围内；
  `docs/superpowers/plans/`（未跟踪的工作产物）亦不逐条收录。
- `crates/tui/CHANGELOG.md` 是根 `CHANGELOG.md` 的逐行相同副本，
  以下引用一律使用根文件。

后续在代码 / 文档中新增 issue 引用时，请记录到本文件，不再指向已废弃的
GitHub tracker。

## 1. 内部 `CX#N` 编号

TUI 注释中出现四个内部设计问题编号。代码树中不存在 `CX#1`–`CX#4` 的引用。

### CX#5 — 跨 delta 拆分的代码围栏起始符的流式门控

围栏代码块的起始符若在多个流式 delta 之间被拆分抵达，绝不能向渲染层暴露
不完整的围栏（例如没有闭合围栏的 `foo```rust`）。流式行缓冲会门控输出，
直到起始符可判定完整。

- `crates/tui/src/tui/streaming/line_buffer.rs:169`（验收场景）

### CX#6 — 宽度无关解析 vs 宽度相关渲染

旧渲染器是单趟的 `render_markdown(content, width)`，每次终端 resize 都要
重新解析源文本。修复后拆分为 `parse`（宽度无关的块级 AST，按转录单元格
缓存）与 `render_parsed`（宽度相关的折行 + span 样式），使 resize 只需
re-flow 而无需 re-parse + re-flow。性能不变量由测试钉住。

- `crates/tui/src/tui/markdown_render.rs:3`（模块文档）
- `crates/tui/src/tui/markdown_render.rs:1378`（性能不变量）

### CX#7 — 并行工具调用聚合为单一单元格渲染

同一回合内并行执行的工具调用必须原地更新单一活跃单元格，聚合为一个块
渲染（tool_routing），而不是每次调用一个单元格。测试为最常见的并行场景
锁定了该契约。

- `crates/tui/src/tui/tool_routing.rs:63`
- `crates/tui/src/tui/ui/tests.rs:4743`
- `crates/tui/src/tui/ui/tests.rs:5061`
- `crates/tui/src/tui/ui/tests.rs:5151`

### CX#8 — 实时视图紧凑 vs 转录视图完整

契约：实时（底部）视图保持 reasoning 紧凑并对工具输出截断；转录（回滚）
视图展示完整正文。由测试锁定。

- `crates/tui/src/tui/history.rs:4571`

## 2. 安全报告

### HackerOne 报告 #3086545 — 不可见 Unicode 提示注入

Unicode Tag 字符（U+E0000–U+E007F 块，尤以 U+E0001 LANGUAGE TAG 为例）与
零宽字符（如 U+200B）对用户不可见但会被模型处理——即报告中演示的隐藏
提示注入向量。防御措施：`partially_sanitize_unicode` /
`recursively_sanitize_unicode` 对 MCP 工具调用输入、工具结果、模型上下文
与转录做 NFKC 归一化并剥离危险 Unicode（零宽、bidi 格式符、BOM、私用区、
Tag 块）。设计参考：
<https://embracethered.com/blog/posts/2024/hiding-and-finding-text-with-unicode-tags/>

- `crates/agent-runtime/src/sanitization.rs:6`
- `crates/agent-runtime/src/sanitization.rs:143`（U+E0001 向量）
- `crates/agent-runtime/src/engine/context.rs:308`
- `crates/agent-runtime/src/engine/host_executor.rs:373`
- `crates/agent-runtime/src/engine/host_executor.rs:1569`
- `crates/agent-runtime/src/engine/host_executor.rs:8204`
- `crates/agent-runtime/src/mcp.rs:3010`
- `docs/rfcs/extra-findings-01-unicode-sanitization.md:12`
- `CHANGELOG.md:64`（Unreleased 清理条目）
- `ROADMAP.md:1580`（read-file observe 路径）

### GHSA-72w5-pf8h-xfp4 — 子代理默认权限必须显式开启

收紧经 `task_create` 创建的子代理的默认权限（v0.8.26 修复）。回归守卫钉住
契约：`allow_shell` / shell 访问必须显式 opt-in，省略可选字段不得静默开启
shell。

- `crates/tui/src/config.rs:2526`
- `crates/tui/src/config.rs:5023`（回归测试）
- `crates/tui/src/task_manager.rs:582`
- `crates/tui/src/task_manager.rs:1615`（回归测试）
- `CHANGELOG.md:2690`

### GHSA-88gh-2526-gfrr — `fetch_url` 网络目标校验

加固 `fetch_url` 工具的网络目标校验（v0.8.26 修复）。回归覆盖钉住方括号
IPv6 字面量的解析。

- `crates/tool-impls/src/tools/fetch_url.rs:338`
- `crates/tool-impls/src/tools/fetch_url.rs:687`（回归测试）
- `CHANGELOG.md:2688`

### 依赖侧通告（为完整起见记录）

Changelog 中记录的第三方依赖升级，非本代码库缺陷：

- `CHANGELOG.md:1985` — `next` 15.5.16 → 15.5.18（GHSA-26hh-7cqf-hhc6，
  App Router middleware/proxy 经 segment-prefetch 路由绕过）及
  `mermaid` 的 GHSA 家族升级。

## 3. 跨仓库 `Whalescale#N` 引用

Whalescale 桌面项目在它自己的 issue 编号里跟踪集成需求；runtime API 与
TUI 在实现处引用这些编号。（Whalescale 编号与 CodeSmith tracker 编号成对
出现时，按收录规则只保留 Whalescale 编号。）

### Whalescale#420 — MCP 优雅关停

`StdioTransport::shutdown` 先发 SIGTERM，给 stdio 服务器一个短暂的宽限
窗口，之后 tokio 的 `kill_on_drop` 才发 SIGKILL；drop 兜底覆盖从未显式
shutdown 的连接池与没有子进程的 transport。

- `crates/agent-runtime/src/mcp.rs:677`
- `crates/agent-runtime/src/mcp.rs:818`（以 `#420` 引用）
- `crates/agent-runtime/src/mcp.rs:3149`
- `crates/agent-runtime/src/mcp.rs:5008`（以 `#420` 引用）
- `crates/agent-runtime/src/engine/mod.rs:911`（以 `#420` 引用）

### Whalescale#439 — toast 栈队列一致性

多个状态 toast 排队时，在 footer 上方以 1–2 行的条带浮出较早的 toast，
避免一串事件被折叠为单条可见消息（`TOAST_STACK_MAX_VISIBLE = 3`；footer
行保持最新一条）。

- `crates/tui/src/tui/app.rs:3097`
- `crates/tui/src/tui/ui.rs:6540`（以 `#439` 引用）
- `crates/tui/src/tui/ui.rs:7779`（以 `#439` 引用）

### whalescale#255 — runtime API CORS 白名单

runtime API 为 Whalescale 桌面桥接增加可配置的 CORS 来源：`config.toml`
的 `[runtime_api] cors_origins` 加 `CODESMITH_CORS_ORIGINS` 环境变量，
在内置 dev origin 之上扩展，并保持首次出现的顺序。

- `crates/tui/src/runtime_api.rs:78`
- `crates/tui/src/runtime_api.rs:2044`
- `crates/tui/src/runtime_api.rs:3684`
- `crates/tui/src/config.rs:1187`
- `crates/tui/src/config.rs:1218`
- `crates/tui/src/main.rs:668`
- `crates/tui/src/main.rs:1699`
- `CHANGELOG.md:3766`（runtime API 四件套条目）

### whalescale#260 — `archived_only` 线程过滤

线程列表接受 `archived` / `status` / `archived_only` 查询参数，桌面 UI
可请求"仅已归档"视图。

- `crates/tui/src/runtime_api.rs:216`
- `crates/tui/src/runtime_api.rs:226`
- `crates/tui/src/runtime_api.rs:3857`
- `crates/tui/src/runtime_threads.rs:553`

### whalescale#256 — `PATCH /v1/threads/{id}`

runtime API 提供 PATCH 端点，UI 可翻转持久线程状态（标题、归档标志）而
无需整体重写，并伴随 `schema_version` 升版。

- `crates/tui/src/runtime_api.rs:3770`
- `crates/tui/src/runtime_threads.rs:584`

### whalescale#261 — `GET /v1/usage` 聚合

usage 端点按天/模型聚合计费回合的 token 与成本，供桌面 UI 使用。

- `crates/tui/src/runtime_api.rs:3968`
- `crates/tui/src/runtime_threads.rs:629`
- `crates/tui/src/runtime_threads.rs:993`

## 4. 源码中无编号的已知问题与待办

### TODO — 子代理权限请求接入审批对话框

全仓库唯一的 `TODO`。子代理的权限请求目前只发一条状态消息（"X needs
permission for Y"）；路由到真正的审批对话框待 UI 支持。

- `crates/agent-runtime/src/engine/team_inbox.rs:82`

### OSC 8 渲染损坏（issue 未创建）

一次 Windows 会话报告：滚动时残留字节吃掉下一行首列、composer 面板重复
（截图显示 `"eepseek-v4-flash"` 的开头 `d` 被吞、三个重叠的 composer 面板）。
v0.8.8 还暴露了 macOS 损坏（`"526sOPEN"` 而非 `"526   OPEN"`）：OSC 8 包装字节被发进 ratatui
`Span` 内容内部时会被逐字节处理——grapheme 过滤丢弃裸
ESC 字节但把包装的其余字节逐个画进缓冲单元格，导致列漂移。缓解措施：在
OSC 8 改为在缓冲管线之外发出之前，所有平台默认关闭；经
`[ui] osc8_links = true` 重新开启。从未创建跟踪 issue。

- `crates/tui/src/tui/ui.rs:287`–`303`

### 超长 token 的折行溢出（已修复，测试钉住）

段落折行（`render_line_with_links`）与代码块折行（`wrap_text`）都是基于
词的：比可用宽度更宽的单个词被单独放在一行并静默溢出右缘——长 URL、路径、
哈希、无空格 CJK 连排都会命中。修复对超长词硬折断；回归套件在宽度 40 与
80 钉住。

- `crates/tui/src/tui/markdown_render.rs:1761`（缺陷描述）
- `crates/tui/src/tui/markdown_render.rs:1866`（代码块折行钉住）

### 粘贴绝不自动提交（QA 守卫）

自动提交会把 composer 替换为 "working / thinking" 状态芯片并清空
composer 文本；PTY dump 中出现任一信号即说明 bug 触发。

- `crates/tui/tests/qa_pty.rs:241`

### 技能发现忽略 vendor 嵌套子目录（已修复，测试钉住）

把技能组织在 vendor/类别子目录下的用户（克隆下来捆绑多个技能的技能仓库）
被旧的单层 `read_dir` 静默丢弃——它只会发现 `<root>/<skill>/SKILL.md`，
忽略 `<root>/<vendor>/<skill>/SKILL.md`。

- `crates/agent-runtime/src/skills/mod.rs:1524`（回归测试）

### 待输入预览中超长 token 的行溢出

比折行预算更长的 token 会被冲出为独自一行、允许溢出——刻意为之，以避免
长 URL 扇出成 N 行垃圾省略号行（此处规避的 codex TUI 已知行为）。

- `crates/tui/src/tui/widgets/pending_input_preview.rs:278`

## 5. 文档中的已知问题、限制与延期项

按收录规则去掉原文中的 GitHub tracker 编号，保留描述与版本上下文。

### CHANGELOG 的 "Known issues" 章节

- **v0.8.32**（`CHANGELOG.md:1869`）— agent 思考或流式输出期间，终端原生
  文本选择仍可能被阻断。v0.8.32 移除了吵闹的 Shift 绕过鼠标捕获路径
  （"scroll demon"），但替代的选择路径当时尚未完成；文本选择修复计划在
  v0.8.33。
- **v0.8.25**（`CHANGELOG.md:2862`）— Windows 10 conhost 闪烁回归：
  v0.8.22 引入的 viewport 重置转义序列需要 Windows 平台守卫（延期到
  v0.8.26）。快照系统仍每回合快照、不区分工作区是否变化（写感知跳过计划
  在 v0.8.26）。代码块中 `▏` 字形泄漏、鼠标选区跨越侧栏、拖选边缘自动
  滚动、运行中 MCP 服务器 stderr 捕获——全部延期到 v0.8.26。后续条目表明
  拖选自动滚动、字形与 MCP stderr 修复已在 v0.8.26 落地
  （`CHANGELOG.md:2736`–`2753`），跨终端闪烁修复见于 v0.8.27–v0.8.29 区段
  （`CHANGELOG.md:2457`、`:2525`）。
- **v0.8.24**（`CHANGELOG.md:2957`）— Windows 闪烁/抖动根因：viewport
  重置序列（`\x1b[r\x1b[?6l\x1b[H\x1b[2J\x1b[3J`）在 conhost 下可能每次
  重绘触发全屏清除；需要平台守卫或更温和的序列。
- **v0.8.23**（`CHANGELOG.md:3039`）— 运行中 MCP 服务器 stderr 被抑制：
  stdio 服务器成功启动但随后崩溃（如在 `initialize` 期间）时没有 stderr
  捕获；计划在 v0.8.24，实际于 v0.8.26 落地（`CHANGELOG.md:2744`）。

### docs/INDEX.md — 代码索引 v1 限制

- `docs/INDEX.md:94` — 引用是基于名字（词法）的，非作用域解析；索引绑定
  工作区根（v1 不重索引 worktree 下的文件）；后台 runtime 线程运行时没有
  索引；语义搜索（`[index.semantic]`）是预留缝隙，尚未编译任何后端。

### docs/SANDBOX.md — 沙箱不防护面

- `docs/SANDBOX.md:268` — 网络攻击（Linux 与 Windows v1 保持网络开放）、
  git hook / fsmonitor 执行、内存攻击、时序侧信道、资源耗尽（不限制 CPU、
  文件描述符、磁盘 I/O）、内核漏洞、供应链。平台差异缺口：Linux seccomp
  白名单可能需要为新 syscall 更新；macOS 运行时生成的 Seatbelt profile
  若配置不当可能过于宽松。

### docs/KEYBINDINGS.md — 可配置键位延期

- `docs/KEYBINDINGS.md:129` — 可配置键位映射与 `tui.toml` 仍然延期：
  `TuiPrefs` 结构体与加载器已存在于 `settings.rs`，但未在启动时接线；
  让 `~/.codesmith/tui.toml` 覆盖单个条目的命名绑定注册表仍在待办中。
  （中文镜像：`docs/KEYBINDINGS_cn.md:129`。）

### docs/EXTENSIONS.md — 禁用在 reload 时生效

- `docs/EXTENSIONS.md:78` — `/extension disable <id>` 会标记扩展为禁用，
  但效果在下一次 `/extension reload` 才落地（同一 reload 注意事项）。

### docs/superpowers/todo.md — §F 扩展系统交接

- `docs/superpowers/todo.md` — §F5（dylib 加载）与 §F2（事件、handler
  链、热重载）已完成。其余阶段按需启动（尚无 spec/plan）：**§F3**
  EventBus 真实现（`crates/extensions/src/bus.rs` 的 `subscribe`/`publish`
  目前返回 `ExtensionError::Unimplemented`）、**§F4** registerProvider、
  **§F6** Renderers、**§F7** Shortcut + Flag、**§F8** Embedding API。
  热加载永久移除（spec §2.4 "never"）。该文件记录的 flaky 测试基线：
  `streamable_http`（agent-runtime）与 `runtime_api`（tui）——均为既有
  状态，触发时隔离重跑。

### docs/plans/codebase-health.md — 清理待办

- `docs/plans/codebase-health.md:34` — 把 `crates/agent-runtime/src/engine/`
  中的 `allow(dead_code)` 归零（或每个幸存者附 migration-issue 链接）；
  清理指向已删代码的注释；确认纯 re-export 后合并/删除 TUI 镜像模块
  （`tui/src/compaction/`、`tui/src/prompts.rs`、`tui/src/mcp.rs`、
  `tui/src/sandbox/`、`tui/src/execpolicy/`）。
- `docs/plans/codebase-health.md:37` — 自 v0.8.33 起废弃的约 12 个子代理
  工具（`agent_spawn`、`agent_result`、`agent_wait`、`delegate_to_agent`
  等）仍注册在目录中，占用工具面与提示词预算。

### ROADMAP.md — 已知缺口与延期 re-wire

- **thinking-only 处理的 by-design 缺口**（`ROADMAP.md:1922`–`1940`）—
  goal-continuation 与 inline-REPL resume 分支延期（基础设施仍在但未接线：
  `tool_state/goal.rs`、`repl/`）；tool-call 回合缺 reasoning 时的
  `"(reasoning omitted)"` 占位 Thinking 块未被执行器注入（DeepSeek
  thinking-mode 要求 tool-call assistant 消息携带 `reasoning_content`）。
  该处列为最后一项 "still to come" 的 seam-3 parallel dispatch 缺口此后
  已闭合（slice 40；`crates/agent-runtime/src/engine/host_executor.rs:251`）。
- **compaction 收尾**（`ROADMAP.md:1536`–`1565`）— 25a（summary-prompt
  合并）与 25b（附件重注入）已落地；**25c** `post_compact_cleanup` 仍延期
  （merge 与 cleanup 互斥 + 分离的 `CompactionProbe` 槽位）；read-file
  observe 站点尚无生产调用方，是独立的后续切片。
- **`#[allow(dead_code)]` 下保留的被取代成员**（`ROADMAP.md:1716`–`1722`）
  — `layered_context_checkpoint`（零调用方；为 nav-aids re-wire 参考而
  保留）、`Engine::recover_context_overflow`（容量级联参考）、KoD 集群
  （Knowledge-on-Demand，已规划）、`rx_user_input`（与 tui sender 成对
  生命周期）、`tool_exec_lock`（耦合到延期的 Gate-A CapacityController）、
  `EarlyToolResult` / `EarlyToolTask`（投机派发）、预留的 `CancelReason`
  枚举变体。

### docs/rfcs/2189-persistence-sqlite.md — 持久化痛点

- `docs/rfcs/2189-persistence-sqlite.md:68` — 驱动 SQLite 持久化 RFC 的
  五大痛点：列出线程/会话/任务需扫描并反序列化所有文件；过滤需全量扫描；
  无事务一致性（回合与其条目保存之间崩溃会产生孤儿）；JSONL 事件回放
  O(n) 且无索引；四个模块中存在六个不同的 schema 版本常量。
