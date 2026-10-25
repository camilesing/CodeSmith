# 用户记忆

用户记忆（user memory）功能为模型提供一个小型持久笔记文件，每一轮都会
注入到系统提示中。它是存放应跨会话留存的偏好与约定的地方——"我偏好
pytest 而不是 unittest"、"这个代码库使用 4 空格缩进"、"提交前总是运行
`cargo fmt`"——无需在每次对话中重复它们。

记忆**默认开启**（与 Claude Code 的自动记忆保持一致）。启用时，
记忆文件会被加载，`remember` 工具会呈现给模型——捕获由模型通过
该工具驱动。bare/simple 会话和无持久存储的远程会话会自动禁用该功能；
通过 `enabled = false` 或 `CODESMITH_DISABLE_AUTO_MEMORY=1` 可完全退出。

## 启用记忆

等价的环境变量也可以切换该功能：

```bash
export CODESMITH_MEMORY=on
```

被接受为真的值有 `1`、`on`、`true`、`yes`、`y` 和 `enabled`。

……或者向 `~/.codesmith/config.toml` 添加：

```toml
[memory]
enabled = true
```

切换后重启 REPL。禁用方式与之相反。

记忆文件默认位于 `~/.codesmith/memory.md`；可通过 `config.toml` 中的
`memory_path` 或环境中的 `CODESMITH_MEMORY_PATH` 覆盖。两者都设置时
`CODESMITH_MEMORY_PATH` 优先于配置文件。

## 快速示例

- 敲一个以 `#` 开头的行来自己快速追加笔记：
  `# 这个仓库提交前要先跑 cargo fmt`——不会发起任何回合。
- 让模型记住某件事（"记住这个仓库提交前要先跑 `cargo fmt`"）；
  记忆启用时，模型可以调用 `remember` 工具保存这条笔记。
- 记忆文件（默认 `~/.codesmith/memory.md`）是纯 Markdown——
  `/memory show` 直接打印，`/memory edit` 打开它的编辑器命令，任意
  外部编辑器也都可用于查看或整理已存储的内容。

## 注入的内容

当记忆已启用且文件存在时，每一轮的系统提示都会携带一个额外的块：

```xml
<user_memory source="/Users/you/.codesmith/memory.md">
- (2026-05-03 22:14 UTC) prefer pytest over unittest
- (2026-05-03 22:31 UTC) this codebase uses 4-space indentation
…
</user_memory>
```

该块位于提示组装中易变内容的边界之上，因此逐轮保持在 DeepSeek 的前缀
缓存之内。该块随系统提示逐轮重新组装（每次 prompt build 都会重读记忆
文件），外部编辑器中的修改在下一轮即对模型生效。

启用 Knowledge On Demand（`kod_enabled = true`）时，来自记忆目录
`MEMORY.md` 入口的 KoD 知识块会取代 `<user_memory>` 块；若入口缺失
或为空，则按原样使用旧版 `<user_memory>` 块。

超过 100 KiB 的文件会被加载但截断，并附加一个标记，以便你看到切割
位置。

## 添加记忆：`remember` 工具

捕获由模型驱动。记忆启用时，模型会获得一个形状如下的 `remember`
工具：

```json
{
  "name": "remember",
  "description": "Save a durable note to memory so it surfaces in future sessions. When Knowledge On Demand is enabled, this creates a typed memory file in the memory directory. Use 'memory_type' to categorize: 'user' (profile/preferences), 'feedback' (behavioral guidance), 'project' (ongoing work/goals), or 'reference' (external pointers). Keep notes terse. Don't store secrets or transient tasks.",
  "input_schema": {
    "type": "object",
    "properties": {
      "note": { "type": "string" },
      "name": { "type": "string" },
      "description": { "type": "string" },
      "memory_type": { "type": "string", "enum": ["user", "feedback", "project", "reference"] }
    },
    "required": ["note"]
  }
}
```

`note` 是唯一必填参数；`memory_type` 默认为 `feedback`。该工具以两种
模式之一写入：

- **KoD 模式**（启用 Knowledge On Demand）：向记忆目录写入一个带
  frontmatter（`name` / `description` / `type`）的 `.md` 文件，并向
  `MEMORY.md` 入口追加一条指针行。完全重复的指针行会被跳过。
- **旧版模式**（未启用 KoD）：无条件地向 `memory.md` 追加一条带
  时间戳的条目。

模型在注意到值得跨会话留存的持久偏好、约定或事实时使用它。该工具自动
获批，因为写入范围仅限于用户自己的记忆位置——若将其置于标准写入审批
流程之后，将有悖于自动记忆捕获的意义。

如果模型将 `remember` 用于临时任务状态（"我正在编辑 foo.rs"），结果
无害但浪费上下文。工具描述明确告诉模型**不要**这样做——只要持久、
简洁的笔记。

## 自己添加记忆：`#` 与 `/memory`

REPL 有一条不经过模型的直接捕获路径：

- **`# <文本>`**——敲一个恰好以单个 `#` 开头的行（但不能是 `##` 或
  `#!`，因此 Markdown 标题和 shebang 会照常作为普通回合提交）。REPL
  会拦截该输入并向记忆文件追加一条带时间戳的条目——格式与
  `remember` 写入的相同。**不发起回合**：输入被消费，确认行显示写入
  的路径。KoD 模式下追加落在记忆目录的 `MEMORY.md` 入口（知识块因此
  可见）；否则落在平面记忆文件。
- **`/memory [show|path|clear|edit]`**——查看与管理该文件：

| 子命令 | 效果 |
|---|---|
| `/memory` / `/memory show` | 打印解析后的路径并内联显示当前内容 |
| `/memory path` | 只打印解析后的路径 |
| `/memory clear` | 将文件替换为空文件 |
| `/memory edit` | 打印 `${VISUAL:-${EDITOR:-vi}} <path>` shell 命令（有意不在进程内启动编辑器） |
| `/memory help` | 命令帮助及当前路径 |

提示中的记忆块在每次 prompt build 时重读（见上文），因此 `#` 追加与
`/memory` 的修改在下一轮就进入模型视图——文件是纯 Markdown，
你随时可以直接阅读。

## 文件格式

记忆是带时间戳条目的普通 Markdown：

```markdown
- (2026-05-03 22:14 UTC) prefer pytest over unittest
- (2026-05-03 22:31 UTC) this codebase uses 4-space indentation
- (2026-05-04 09:02 UTC) all PRs need 2 reviewers before merge
```

你可以在任何编辑器中手动编辑该文件——加载器不关心时间戳格式；它只是
把整个文件读作记忆块。时间戳是一种约定，便于你在整理文件时知道每条
笔记是何时添加的。

## 层级与导入

有两种不同的东西会进入系统提示的指令块，**用户记忆**只是其中之一：

- **用户记忆**（`~/.codesmith/memory.md`，即本功能）——你通过
  `remember` 工具或手动编辑文件逐步积累的唯一持久笔记文件。
- **指令层级（Instruction tiers）**——从四个信任层级收集并合并进
  同一个块的 `AGENTS.md` / `CLAUDE.md` 文件。

- 将**用户记忆**用于应跟随你跨仓库、跨会话的持久个人偏好。
- 将**指令层级**用于应随机器或代码库移动的组织级或仓库级约定。

### 指令信任层级

指令文件按特异性升序从四个层级收集。发生冲突时后面的层级覆盖前面的，
因此项目规则比托管层或用户层规则拥有最终话语权。每个层级的内容前都
有 `<!-- tier: … -->` 标签，以便当两条规则不一致时，模型能辨别一条
规则来自哪一层。

| 层级   | 来源                                                                                                | 标签      |
|--------|-----------------------------------------------------------------------------------------------------|-----------|
| Managed | `/etc/codesmith/CLAUDE.md`（组织策略）                                                              | `managed` |
| User    | `~/.codesmith/AGENTS.md`，然后 `~/.agents/AGENTS.md`                                                 | `user`    |
| Project | `{cwd}` 中 `AGENTS.md`、`.claude/instructions.md`、`CLAUDE.md`、`.codesmith/instructions.md` 的第一个命中者，然后向父目录遍历 | `project` |
| Local   | `.claude/rules/` 和 `.codesmith/rules/` 中的 `*.md` 片段（已排序）                                 | `local`   |

### `@include <path>` 指令

任何层级中的任何指令文件都可以将另一个文件内联引入：

```markdown
@include ../shared/coding-style.md
```

以 `~` 锚定的目标（如 `@include ~/notes/security.md`）解析到 home 目录
内，因此从 User 层文件（层级根为 home）可以加载，从 Project/Local
文件则会被丢弃。

该指令必须独占一行（允许前导空白），其后跟路径前的空白，因此像
"see @include" 这样的正文不会被误认为指令。目标会做 `~` 展开并相对
于包含该指令的文件解析。Markdown 代码围栏（\`\`\` 或 `~~~` 块）
内的指令是无效正文。展开有界且去重：

- **层级根边界** — 目标可解析到其层级根内的任意位置（Project/Local
  文件为工作区、User 层为 home 目录、Managed 为 `/etc/codesmith`），
  按规范路径比较，符号链接无法绕过。解析结果越出该边界的——`..`
  逃逸、绝对路径、工作区文件引用 `~` 目标——会被丢弃并告警：不可信
  的工作区记忆文件不得把任意文件（`~/.ssh/…`、`/etc/…`）拉进系统
  提示词。
- **深度上限** — `MAX_INCLUDE_DEPTH = 5`。根文件加上最多五层 include
  会被加载；第六层被静默丢弃。这限制了递归并让提示组装保持缓存友好。
- **符号链接稳定的去重** — 已加载过的文件（按规范路径判断）不会再次
  加载，即使经由符号链接循环。
- **继承标签** — 被引入的内容携带拉取它的文件所在的层级标签。

### 排除项

排除列表中的路径无论本会以何种方式被加载——作为层级来源或
`@include` 目标——都会被跳过。在 `config.toml` 中设置：

```toml
[memory]
excludes = ["~/work/secret/CLAUDE.md", "/etc/sandbox-override.md"]
```

……或者在不修改配置的情况下在 shell 中覆盖：

```bash
export CODESMITH_MEMORY_EXCLUDES=~/work/secret/CLAUDE.md:/etc/sandbox-override.md
```

该环境变量与配置值合并（而非替换）；`~` 会被展开，路径按规范形式
匹配，因此指向被排除文件的符号链接也会被跳过。

## 不应进入记忆的内容

记忆用于**持久**信号。不应存放在这里的东西：

- **秘密** — 不要放 API 密钥、令牌、密码。该文件是磁盘上的纯文本，
  并被原样注入系统提示。
- **临时任务状态** — "我目前正在做解析器"每个会话都会变化；它不属于
  跨会话记忆。
- **对话片段** — 引用式的笔记属于笔记工具（`note`），不属于记忆。
- **长指令** — 超过几句话的内容应放在 `AGENTS.md`（项目级）或
  [技能](../crates/cli/src/skills/mod.rs)（可复用的指令包）中。

## 隐私与范围

记忆文件完全保存在你机器上的 `~/.codesmith/` 中。它从不会被上传到
任何云服务——CodeSmith 只是在记忆启用时把它内联进 LLM 提供方收到的系统
提示中。如果你切换提供方（DeepSeek / NVIDIA NIM / Fireworks 等），
使用的仍是同一个记忆文件；该文件与提供方无关。

一个独立的、可选启用的**遥测**（telemetry）汇（`telemetry = true`）
将容量决策分析写入仅限本地的 jsonl 文件
（`~/.codesmith/telemetry/events.jsonl`）——同样从不联网。它与这个
用户记忆文件无关：遥测事件携带临时的每会话 id，而不是你记忆文件的
内容。磁盘上的会话/容量记忆文件以持久线程 id 为键，因此遥测无法跨
重启关联回你的记忆。参见
`docs/OPERATIONS_RUNBOOK.md` 和 `docs/CONFIGURATION.md`。

该文件按用户划分，而非按项目。如果你想要项目特定的记忆，请改用项目
级的 `AGENTS.md` 或 `.codesmith/instructions.md` 文件。这些文件由
`project_context` 加载，位于仓库中（或你提交它们的任何地方）。

## 配置参考

```toml
# ~/.codesmith/config.toml
[memory]
enabled = true                    # 默认 true（开启）；也可用 CODESMITH_MEMORY=on
kod_enabled = false               # Knowledge On Demand：类型化记忆目录 + MEMORY.md 入口
directory = "~/.codesmith/memory" # KoD 记忆目录（默认：memory_path 父目录 + /memory/）
excludes = ["~/work/secret/CLAUDE.md"]  # skip these paths in the tier merge
# Path is configured at the top-level (next to skills_dir, notes_path):
memory_path = "~/.codesmith/memory.md"
```

| 设置          | 默认值                        | 覆盖方式                                           |
|---------------|-------------------------------|----------------------------------------------------|
| 启用记忆      | `true`                        | `[memory] enabled = false` 或 `CODESMITH_DISABLE_AUTO_MEMORY=1` |
| 记忆文件路径  | `~/.codesmith/memory.md`      | `memory_path = "..."` 或 `CODESMITH_MEMORY_PATH=`  |
| 启用 KoD      | `false`                       | `[memory] kod_enabled = true`（要求 `enabled = true` 且开启 `knowledge_on_demand` 特性开关） |
| 记忆目录      | `memory_path` 父目录 + `/memory/` | `[memory] directory = "..."`                  |
| 记忆排除项    | （无）                        | `[memory] excludes = ["..."]` 或 `CODESMITH_MEMORY_EXCLUDES=`（冒号分隔） |
| 文件大小上限  | 100 KiB                       | （目前无；截断标记会显示切割位置）                 |

## 记忆整理（`codesmith memory consolidate`）

在线循环对记忆只会**追加**（KoD 模式下 `remember` 会跳过完全重复的
`MEMORY.md` 指针行；旧版模式的追加不去重；也没有任何修剪）。
`codesmith memory consolidate` 是离线的对应面——持续进化
计划（P3-8）的"睡眠学习"一步——并且只改写 Knowledge On Demand 的
`MEMORY.md` 索引，绝不改动主题文件内容：

- **确定性整理**永远先跑：删除完全重复的指针行、丢弃指向已删除
  主题文件的指针；孤立主题文件（在盘但未被索引）只**报告**、绝不
  删除；并对照入口预算（200 行 / 25 KB，与注入器截断用的同一组
  上限）检查索引。
- **LLM 合并提案**（可选，`--deterministic-only` 跳过）：把清理后的
  索引加全部主题文件的 frontmatter 交给一次咨询式调用（配置了
  `[utility_model]` 时用它，否则用主客户端）重写索引——描述更
  精炼、相关指针分组、每文件恰好一条。提案只有在校验器确认每个
  现存主题文件仍被恰好引用一次、没有引入未知文件、预算未超时才会
  被接受；任何违例都回退到确定性结果。
- **写入门控**：默认是干跑，只打印统一 diff；`--apply` 才写盘，且
  先做带时间戳的 `MEMORY.md.bak.<unix_ts>` 备份（每次运行一个新文件，
  首次合并前的原件也不会被后续合并覆盖）。下一个会话的提示词刷新会
  自动拿到新索引。

```bash
codesmith memory consolidate                      # 干跑：报告 + diff
codesmith memory consolidate --apply              # 写盘（先备份）
codesmith memory consolidate --deterministic-only # 完全不调 LLM
```

## 相关

- `docs/SUBAGENTS.md` — 子代理不继承该文件，也没有 `remember`
  工具；它们获得可选启用的作用域代理记忆（`agent_open` 的 `memory`
  参数，以及 `agent_memory_read` / `agent_memory_write` /
  `agent_memory_edit` 工具）。
- `docs/CONFIGURATION.md` — 完整配置参考。
