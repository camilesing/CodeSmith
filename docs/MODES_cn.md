# 模式与审批

codesmith 有两个相关概念：

- **TUI 模式**：你当前所处的可见交互类型（Plan/Agent/YOLO）。
- **审批模式**：UI 在执行工具前要求确认的严格程度。

模型选择是独立的。`--model auto` 和 `/model auto` 会把每个对话轮路由到具体的模型和思考级别；它们不是 TUI 模式，也不属于 `Tab` 循环。

## TUI 模式

按 `Tab` 可确认 composer 菜单选择、在对话轮运行期间把草稿排入下一轮跟进，或在
composer 空闲时循环切换可见模式：**Plan → Agent → YOLO → Plan**。
按 `Shift+Tab` 循环切换推理力度。
运行 `/mode` 打开模式选择器，或使用 `/mode agent`、
`/mode plan`、`/mode yolo`、`/mode 1`、`/mode 2` 或 `/mode 3` 直接切换。

- **Plan**：设计优先的提示模式。只读调查工具保持可用；shell 与补丁执行保持关闭。当你想边思考边表达、产出一份交给真人（未来的自己或评审者）的计划时使用它。
- **Agent**：多步工具使用。Shell 执行（`exec_shell`、`task_shell_start`、`task_shell_wait`）要求配置中 `allow_shell = true`；每次调用都由审批提示把关。文件写入无需提示即可进行。
- **YOLO**：启用 shell + 信任模式并自动批准所有工具。仅在受信任的仓库中使用。

所有具备执行能力的模式都可以通过 `rlm_open`、`rlm_eval`、`rlm_configure` 和 `rlm_close` 访问持久的 RLM 会话。在 RLM Python REPL 中，`sub_query_batch` 可以并发发出 1-16 个固定使用 `deepseek-v4-flash` 的廉价并行子调用。当工作对于父对话来说过于庞大或重复时，模型会主动使用它。

快速的 `deepseek-v4-flash` / 关闭思考路径在产品语言中称为 Fin。Fin 是路由、
摘要、廉价子调用和协调工作的接缝；它不会改变审批行为。

`/goal` 设置一个带可选 token 预算的会话目标，并将该目标作为 Work 上下文保持
可见。它不会改变当前活跃的 TUI 模式、审批模式或模型路由。它与 `--model auto`
保持区分，后者只控制模型与思考选择。

## 兼容性说明

- 带有 `default_mode = "normal"` 的旧设置文件仍按 `agent` 加载；保存时会写入规范化后的值。

## Esc 键行为

`Esc` 是一个取消栈，而不是模式切换。

- 先关闭斜杠菜单或临时 UI。
- 如果有对话轮正在运行，取消当前请求。
- 如果 composer 为空，丢弃已排队的草稿。
- 如果存在输入文本，清空当前输入。
- 否则不做任何操作。

## 审批模式

你可以在运行时覆盖审批行为：

```text
/config
# edit the approval_mode row to: suggest | auto | never
```

旧版说明：`/set approval_mode ...` 已被弃用，由 `/config` 取代。

- `suggest`（默认）：使用上述各模式的规则。
- `auto`：自动批准所有工具（类似 YOLO 的审批行为，但不强制进入 YOLO 模式）。
- `never`：阻止任何不被视为安全/只读的工具。

## 小屏状态栏行为

当终端高度受限时，状态区域会首先压缩，让头部/聊天/composer/底部栏保持可见：

- 加载与排队状态行按可用高度分配。
- 完整预览放不下时，排队预览折叠为紧凑摘要。
- `/queue` 工作流仍可用；紧凑状态只影响渲染密度。

## 工作区边界与信任模式

默认情况下，文件工具被限制在 `--workspace` 目录内。启用信任模式可允许访问工作区之外的文件：

```text
/trust
```

YOLO 模式会自动启用信任模式。

## MCP 行为

MCP 工具以 `mcp__<server>__<tool>` 的形式暴露（双下划线；旧的单下划线写法 `mcp_<server>_<tool>` 仍作为兼容别名被接受），并使用与内置工具相同的审批流程。在建议型审批模式下，只读 MCP 助手可以自动运行；可能产生副作用的 MCP 工具需要审批。

参见 `MCP.md`。

## 相关 CLI 参数

运行 `codesmith --help` 获取权威列表。常见参数：

- `-p, --prompt <TEXT>`：一次性提示模式（打印后退出）
- `codesmith exec --auto --output-format stream-json <PROMPT>`：运行带工具的非交互智能体，并为 harness 和后端封装逐行输出一个 JSON 对象
- `codesmith exec --resume <ID|PREFIX> <PROMPT>` / `--session-id <ID|PREFIX>`：以非交互方式继续一个已保存的会话
- `codesmith exec --continue <PROMPT>`：以非交互方式继续该工作区最近保存的会话
- `codesmith swebench run --instance-id <ID> --issue-file <PATH>`：在一个 SWE-bench 任务上运行带工具的智能体，并写入/更新一行预测 JSONL
- `codesmith fork <ID|PREFIX>` / `codesmith fork --last`：把已保存的会话复制为新的同级会话；fork 出的会话保留可叠加的父会话元数据，并在会话列表中显示该谱系
- `--model <MODEL>`：使用 `codesmith` 门面时，将模型覆盖转发给 TUI
- `--workspace <DIR>`：文件工具的工作区根目录
- `--yolo`：以 YOLO 模式启动
- `-r, --resume <ID|PREFIX|latest>`：恢复一个已保存的会话
- `-c, --continue`：恢复该工作区最近的会话
- `--max-subagents <N>`：限制为 `1..=20`
- `--mouse-capture` / `--no-mouse-capture`：选择加入或退出内置鼠标滚动、对话记录选择、右键上下文操作以及对话记录滚动条拖拽。在非 Windows 终端以及 Windows Terminal/ConEmu/Cmder 上，鼠标捕获默认开启，因此拖拽选择只会复制对话记录文本、去掉段落的视觉折行列换行，并限定在对话记录面板内；按住 Shift 拖拽或使用 `--no-mouse-capture` 可使用终端原生选择。在旧版 Windows 控制台（没有 `WT_SESSION` / `ConEmuPID` 的 CMD）以及 JetBrains JediTerm——PyCharm/IDEA/CLion 等——内部默认关闭，因为这些终端声称支持鼠标却把 SGR 鼠标事件当作原始文本转发（#878、#898）。在任何默认关闭的地方都可以用 `--mouse-capture` 选择加入。终端原生选择可能越过右侧栏并包含视觉折行，因为选择由终端而非 TUI 拥有。
- `--profile <NAME>`：选择配置 profile
- `--config <PATH>`：配置文件路径
- `-v, --verbose`：详细日志

## 分支与回滚

CodeSmith 有三条相关但刻意分开的恢复路径：

- `codesmith fork <ID>` 从一个已保存的对话创建新的已保存会话，并记录源会话 id。
  这是在不覆盖原会话的情况下探索另一条答案路径的安全方式。
- Esc-Esc 回退会把实时对话记录回退到上一个用户提示，并将该提示恢复到
  composer 中以便编辑。
- `/restore` 和 `revert_turn` 工具从 side-git 快照恢复工作区文件。它们不会改写
  对话历史。

Pi 风格的文件内树状浏览器是一个更大的 UI/数据模型项目。v0.8.40 提供的是
有边界的 fork/回退原语和显式谱系元数据。
