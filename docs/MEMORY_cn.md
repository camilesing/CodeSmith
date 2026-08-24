# 用户记忆

用户记忆（user memory）功能为模型提供一个小型持久笔记文件，每一轮都会
注入到系统提示中。它是存放应跨会话留存的偏好与约定的地方——"我偏好
pytest 而不是 unittest"、"这个代码库使用 4 空格缩进"、"提交前总是运行
`cargo fmt`"——无需在每次对话中重复它们。

记忆**默认开启**（与 Claude Code 的自动记忆保持一致）。启用时，
记忆文件会被加载、`# ` 快速添加会追加到该文件、`remember` 工具会
呈现给模型。bare/simple 会话和无持久存储的远程会话会自动禁用该功能；
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

切换后重启 TUI。禁用方式与之相反。

记忆文件默认位于 `~/.codesmith/memory.md`；可通过 `config.toml` 中的
`memory_path` 或环境中的 `CODESMITH_MEMORY_PATH` 覆盖。两者都设置时
`CODESMITH_MEMORY_PATH` 优先于配置文件。当不存在 `.codesmith` 记忆
文件时，已有的 `~/.codesmith/memory.md` 文件仍作为旧版回退被支持。

## 快速示例

```text
# remember that this repo prefers cargo fmt before commits
/memory
/memory path
/memory edit
/memory help
```

- 在输入框中键入 `# remember that this repo prefers cargo fmt before commits`，即可追加一条带时间戳的条目，而不触发回合。
- 运行 `/memory` 确认该功能正在写入哪里以及当前存了什么。
- 想在编辑器中手动整理文件时，运行 `/memory edit`。

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
缓存之内。文件在每次构建提示时都会读取——通过 `/memory` 或外部编辑器
的修改会在下一轮生效，无需重启。

超过 100 KiB 的文件会被加载但截断，并附加一个标记，以便你看到切割
位置。

## 添加记忆的三种方式

### 1. `# ` 输入框前缀（#492）

在输入框中键入一行以 `#` 开头（但不是 `##` 或 `#!`）的内容：

```
# remember to use 4-space indentation in this repo
```

TUI 会拦截该输入，并向你的记忆文件追加一条带时间戳的条目。**不触发
回合**——你的输入被消费，状态行确认写入路径，你可以继续键入真正的
问题。

多个 `#` 的前缀会特意放行为正常的回合提交，这样你可以放心粘贴
Markdown 标题。

### 2. `/memory` 斜杠命令（#491）

查看、清空文件或获取编辑提示：

| 子命令             | 效果                                                  |
|--------------------|--------------------------------------------------------|
| `/memory`          | 内联显示解析后的路径和当前内容                         |
| `/memory show`     | 无参数形式的别名                                       |
| `/memory path`     | 只打印解析后的路径                                     |
| `/memory clear`    | 将文件替换为空标记                                     |
| `/memory edit`     | 打印 `${VISUAL:-${EDITOR:-vi}} <path>` 这条 shell 命令 |
| `/memory help`     | 显示命令专属帮助和当前路径                             |

`/memory edit` 形式刻意只打印命令而不在进程内启动编辑器——这使斜杠
命令处理程序保持简单且一致，无论你使用哪种编辑器。

你也可以从常规帮助入口发现该功能：

- `/help memory` 显示斜杠命令摘要和用法行。
- `/memory help` 打印记忆专属子命令以及解析后的路径。

### 3. `remember` 工具（自动更新，#489）

当记忆启用时，模型会获得一个形状如下的 `remember` 工具：

```json
{
  "name": "remember",
  "description": "Append a durable note to the user memory file...",
  "input_schema": {
    "type": "object",
    "properties": {
      "note": { "type": "string", ... }
    },
    "required": ["note"]
  }
}
```

模型在注意到值得跨会话留存的持久偏好、约定或事实时使用它。该工具自动
获批，因为写入范围仅限于用户自己的记忆文件——若将其置于标准写入审批
流程之后，将有悖于自动记忆捕获的意义。

如果模型将 `remember` 用于临时任务状态（"我正在编辑 foo.rs"），结果
无害但浪费上下文。工具描述明确告诉模型**不要**这样做——只要持久、
单句的笔记。

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

- **用户记忆**（`~/.codesmith/memory.md`，即本功能）——你通过 `#`
  前缀行、`/memory` 或 `remember` 工具逐步积累的唯一持久笔记文件。
- **指令层级（Instruction tiers）**——从四个信任层级收集并合并进
  同一个块的 `CLAUDE.md` / `AGENTS.md` / `WHALE.md` 文件。

- 将**用户记忆**用于应跟随你跨仓库、跨会话的持久个人偏好。
- 将**指令层级**用于应随机器或代码库移动的组织级或仓库级约定。

### 指令信任层级

指令文件按特异性升序从四个层级收集。发生冲突时后面的层级覆盖前面的，
因此项目规则比托管层或用户层规则拥有最终话语权。每个层级的内容前都
有 `<!-- tier: … -->` 标签，以便当两条规则不一致时，模型能辨别一条
规则来自哪一层。

| 层级   | 来源                                                                                                | 标签      |
|--------|-----------------------------------------------------------------------------------------------------|-----------|
| Managed | `/etc/codesmith/CLAUDE.md`，然后 `/etc/codesmith/CLAUDE.md`（组织策略）                            | `managed` |
| User    | `~/.codesmith/{WHALE,AGENTS}.md`，然后 `.agents/`，然后旧版 `.codesmith/`                          | `user`    |
| Project | `{cwd}` 中 `WHALE.md`、`AGENTS.md`、`.claude/instructions.md`、`CLAUDE.md`、`.codesmith/instructions.md`、`.deepseek/instructions.md` 的第一个命中者，然后向父目录遍历 | `project` |
| Local   | `.claude/rules/` 和 `.codesmith/rules/` 中的 `*.md` 片段（已排序）                                 | `local`   |

CodeSmith 此前只加载 Project 和 User 层级；Managed 和 Local 层级以及
父目录遍历是新加入的。

### `@include <path>` 指令

任何层级中的任何指令文件都可以将另一个文件内联引入：

```markdown
@include ../shared/coding-style.md
  @include ~/notes/security.md
```

该指令必须独占一行（允许前导空白），其后跟路径前的空白，因此像
"see @include" 这样的正文不会被误认为指令。目标会做 `~` 展开并相对
于包含该指令的文件解析。展开有界且去重：

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
  [技能](../crates/tui/src/skills/mod.rs)（可复用的指令包）中。

## 隐私与范围

记忆文件完全保存在你机器上的 `~/.codesmith/` 中。它从不会被上传到
任何云服务——TUI 只是在记忆启用时把它内联进 LLM 提供方收到的系统
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
级的 `AGENTS.md` 或 `.codesmith/instructions.md` 文件。旧版
`.deepseek/instructions.md` 文件出于兼容仍会被加载。这些文件由
`project_context` 加载，位于仓库中（或你提交它们的任何地方）。

## 配置参考

```toml
# ~/.codesmith/config.toml
[memory]
enabled = true                    # 默认 true（开启）；也可用 CODESMITH_MEMORY=on
excludes = ["~/work/secret/CLAUDE.md"]  # skip these paths in the tier merge
# Path is configured at the top-level (next to skills_dir, notes_path):
memory_path = "~/.codesmith/memory.md"
```

| 设置          | 默认值                        | 覆盖方式                                           |
|---------------|-------------------------------|----------------------------------------------------|
| 启用记忆      | `true`                        | `[memory] enabled = false` 或 `CODESMITH_DISABLE_AUTO_MEMORY=1` |
| 记忆文件路径  | `~/.codesmith/memory.md`      | `memory_path = "..."` 或 `CODESMITH_MEMORY_PATH=`  |
| 记忆排除项    | （无）                        | `[memory] excludes = ["..."]` 或 `CODESMITH_MEMORY_EXCLUDES=`（冒号分隔） |
| 文件大小上限  | 100 KiB                       | （目前无；截断标记会显示切割位置）                 |

## 相关

- `docs/SUBAGENTS.md` — 子代理会继承记忆，也可以使用 `remember`
  工具。
- `docs/CONFIGURATION.md` — 完整配置参考。
- Issue [#489](https://github.com/Hmbown/CodeSmith/issues/489)
  — 跟踪这项工作的第一阶段 EPIC。
