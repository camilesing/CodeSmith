# CodeSmith 用户指南

本指南面向你使用 CodeSmith 的第一个小时。它介绍主要工作流、重要的安全控制，以及当你需要完整参考时下一步该去哪里。

CodeSmith 为安装、配置、Provider、模式、工具和运维提供了更深入的参考文档。请将本页面作为引导式入门，然后在需要了解每个选项时跟随"下一步"链接。

## 1. 欢迎使用 CodeSmith

CodeSmith 是一个终端编码 agent。你从某个工作区启动它，交给它一个任务，它就可以使用结构化工具检查文件、运行命令、编辑代码，并带回证据汇报。

与普通聊天模型的重要区别在于，CodeSmith 是围绕一个 harness 构建的：

- 它让当前工作区和会话保持可见。
- 它通过显式的模式和审批规则处理每一轮。
- 它在会话记录中显示工具调用，而不是隐藏工作过程。
- 它可以保留会话、分叉对话并稍后继续。
- 它可以运行子代理来完成聚焦的后台工作。

你可以用 CodeSmith 处理小问题：

```text
Explain the authentication flow in this repository.
```

也可以用它做多步骤工作：

```text
Find the failing validation path, propose a fix, and wait for my approval
before editing files.
```

对于新仓库，请从保守开始。先让 CodeSmith 探索和规划，再让它修改文件。这会给你一条可审查的路径，也更容易及早发现错误假设。

下一步：[ARCHITECTURE.md](ARCHITECTURE.md) 解释内部 harness 和运行时模型。

## 2. 首次启动

选择适合你机器的方式安装 CodeSmith。每个受支持的安装路径都提供同一个 `codesmith` 二进制文件，它同时承载无头（headless）子命令和基于行的交互式 REPL。

```bash
# npm
npm install -g codesmith

# Cargo
cargo install codesmith-cli --locked

# Homebrew
# The tap/formula name differs from the package name; it installs codesmith.
brew tap camilesing/codesmith
brew install codesmith
```

当你需要隔离的运行时时，也可以使用 Docker：

```bash
docker volume create codesmith-home
docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/camilesing/codesmith:latest
```

从你希望 CodeSmith 工作的仓库或目录启动它：

```bash
codesmith
```

首次启动没有向导——CodeSmith 会直接进入 REPL。请先设置 provider 及其 API key：未做任何配置时，DeepSeek 是默认的回退 provider；缺少 key 时，第一轮对话会直接以提供商认证错误失败。

在 REPL 之外，同样的配置入口是 `--provider`、`CODESMITH_PROVIDER`，或配置文件中的 `provider = "<id>"`。回退 provider 最直接的配置路径是：

```bash
codesmith auth set --provider deepseek
```

你也可以通过环境变量提供 key：

```bash
export DEEPSEEK_API_KEY="your-key"
codesmith
```

CodeSmith 的配置存储在 `~/.codesmith/config.toml`。

配置完成后，运行一次 doctor 检查：

```bash
codesmith doctor
```

当你需要用于 issue 的机器可读报告时，使用 JSON 形式：

```bash
codesmith doctor --json
```

如果 doctor 命令报告被拒绝的 key 来自环境变量，请先移除或替换该环境变量，再重新测试已保存的配置。

下一步：[INSTALL.md](INSTALL.md) 介绍各平台的安装路径，[CONFIGURATION.md](CONFIGURATION.md) 介绍配置解析，[PROVIDERS.md](PROVIDERS.md) 介绍 provider ID 和凭据。

## 3. 你的第一个任务

从真实工作区中的只读任务开始：

```text
Map the repository structure and tell me where the CLI entrypoint lives.
```

然后要求一个聚焦的计划：

```text
I want to add a small validation for empty config values. Inspect the relevant
code and propose the smallest safe change before editing anything.
```

当你准备好让它编辑时，明确说明验收标准：

```text
Implement the validation you proposed. Keep the change scoped to config
parsing, add or update the narrowest test, and run the relevant check.
```

好的初始提示词包含四个细节：

- 你想要的结果。
- 你关心的文件、特性或行为。
- 哪些内容不在范围内。
- 什么样的验证算作完成。

例如：

```text
Fix the broken provider error message in the config loader. Do not change the
provider registry. Add a regression test and run only the config crate tests.
```

如果你不确定 bug 在哪里，直接说明：

```text
Investigate why `codesmith doctor` reports the wrong provider. Do not edit
files yet. Return the likely cause, evidence, and a proposed patch plan.
```

对于不熟悉的代码，让调查和实现分成两个独立步骤时 CodeSmith 表现最好。对于小而明确的改动，单次实现请求即可。

下一步：[PRESETS.md](PRESETS.md) 说明何时使用 Plan、Agent 和 YOLO。

## 4. 理解界面

交互式 REPL 是一个基于行的循环（Claude Code / Codex 风格）：你输入一行，
引擎运行一轮，输出流式打印到终端。

- 状态行：提示符上方的一行，显示运行状态、预设档位、上下文窗口占用（`🧭`）、
  权限模式（`🚥`，见下文[模式](#5-模式)）、运行中子代理数量（`🤖`）、模型、
  思考档位（`🧠`，即 `reasoning_effort` 设置：`off` / `low` / `med` /
  `high` / `max`，或 `auto`）和版本。设置了 `[providers.<id>] display_model`
  时模型标签打印该名称（别名网关背后其实是别的模型时正是这个用途），否则
  显示模型 id。`🧭` 的分母同样优先采用 provider 的 `context_window` 覆盖，
  其次才回退到静态模型表。运行中的 live 行同样带有模式、子代理数量和上下文
  百分比。
- 提示符：你输入提示词和斜杠命令的地方（`/help` 列出命令）。
- 会话记录：对话、工具调用、命令输出摘要和模型回复，随发生流式输出。

会话记录就是审计轨迹。当 CodeSmith 读取文件、运行命令或编辑代码时，操作会显示在那里。如果命令失败，把可见的失败输出作为下一条指令的一部分，而不是从头再来。

工具调用默认每条只占一行折叠显示（调用名、参数和结果首行）。想查看已运行的某个调用的完整结果，用 `/expand`（上一轮的全部调用）、`/expand <n>`（第 n 条）或 `/expand all`（保留的全部记录）。一轮进行中按 `Ctrl+O` 可在折叠/展开渲染之间实时切换。终端会话记录是只增不删的——展开是在当前输出下方补印细节，而不是改写历史；另外一轮运行期间除 `Ctrl+O` 外的按键会被丢弃。

提示符接受普通提示词和斜杠命令；`/help` 列出全部已登记命令。输入 `/`
会显示命令数量，`Tab` 补全命令名（多个匹配时连按两次列出候选），光标后的暗色
文字预览匹配到的命令及其一行说明。`Shift+Tab` 循环运行模式（见第 5 节）。
使用 `@path` 引用为下一条消息附加
图像文件，或用 `/attach <path>` 附加本地媒体。`Ctrl+C` 取消运行中的
一轮；连续按两次退出。

下一步：[CLI.md](CLI.md) 是命令与子命令参考。

## 5. 模式

CodeSmith 有三个可见模式：

| 模式 | 用途 | 默认姿态 |
| --- | --- | --- |
| Plan | 变更前的探索、设计和审查 | 只读调查 |
| Agent | 常规多步骤编码工作 | 带审批门控的工具使用 |
| YOLO | 你希望自动执行的受信任仓库 | 自动审批与信任 |

在 REPL 中直接切换模式：

```text
/mode plan
/mode agent
/mode yolo
```

（`/plan`、`/agent`、`/yolo` 分别是对应 `/mode` 取值的快捷方式；不带参数的 `/mode` 打印当前模式。）在提示符下按 `Shift+Tab` 不必输入命令即可循环同样的模式——依次经过 `agent → plan → yolo`，实验性 `coordinator_mode` 功能开启时 `coordinator` 是第四站。切换对下一条发送的消息生效，提示符上方状态行的 `🚥` 段显示当前所在模式。

在不熟悉的仓库中，Plan 模式是最安全的起点。它用于检查和决策，而不是文件编辑。它可以派发只读子代理并行调查多个区域——或多个仓库；每个子代理都被限制在只读工具内，工作区之外的目录需要先用 `/trust add <path>` 授权（见 `docs/SUBAGENTS.md` 的 "Working across repositories"）。

Plan 模式以自己的审批提示收尾：`exit_plan_mode` 先打印计划正文，再问一个三选一的问题。`[y]` 批准并立刻开始实施（见下段）；`[n]`（或空行）拒绝——模型会得知计划被拒、留在 plan 模式里改一版。**你打的任何其他内容都算反馈**：它拒掉这份计划并把你这句意见交给正在运行的回合——模型据此改 plan 文件，然后再次询问；`[r] <反馈>` 是同一件事的显式写法，只敲 `r` 会请你在下一行补充内容。拒绝不会退出 plan 模式，计划文件也留在原处，所以"改一版"从来不是从头再来。这个门**故意不提供** `[a]`/`[e]`：计划工具没有可用于区分的参数，一个活得比它所回答的那份计划更久的批准，会把之后每一版修订计划的评审都静音掉。

批准计划会结束那一轮——然后 CodeSmith 自己接着走：会话回到你进入 Plan 之前的模式，实施启动消息由它替你提交，下一轮就带着写工具直接开工，不需要你再发一条消息。（Plan 模式的工具集里根本没有写工具——`apply_patch`、`write_file`、shell 都不注册——这正是交接要走一个新回合、而不是在原回合里继续的原因。）

模型也可以**自己**在任务中途开一个计划会话（`enter_plan_mode`）：你让它先规划一下，它可能就会这么做。这种形式不需要你切模式——会话开着的时候，写入和 shell 会被逐次调用拒绝（计划文件、清单、只读子代理会话仍然可用）；而 `exit_plan_mode`——和任何计划一样由你批准——立刻解除该限制，于是同一个回合就能接着实现。

Agent 模式是大多数贡献工作的默认模式。它允许 CodeSmith 读取、运行检查和编辑文件，同时将高风险操作保留在审批门控之后。

YOLO 模式适用于你有意让模型不停下来等待审批就行动的受信任工作区。不要在你不信任的仓库中使用它。

模式与模型路由是分开的。`/mode` 切换可见模式，而 `/model auto` 控制每轮的模型和思考级别选择。

你还可以在 `/config` 中通过编辑审批模式来更改审批行为。只有当你理解它如何改变工具执行时才使用。

下一步：[PRESETS.md](PRESETS.md) 包含完整的模式、审批和信任模式参考。

## 6. 斜杠命令

斜杠命令在提示符处输入。当你想直接更改 CodeSmith 状态而不是用自然语言请求模型时，它们很有用。

面向新用户的常用命令：

| 命令 | 用途 |
| --- | --- |
| `/mode` | 在 agent、plan、yolo、coordinator 模式间切换（`/plan`、`/agent`、`/yolo` 为快捷方式） |
| `/preset` | 查看、切换、导出或去掉配置档（`/preset simple`、`/preset list`、`/preset export <name>`、`/preset off`） |
| `/model` | 选择模型，或使用 `/model auto` |
| `/models` | 列出活动端点的实时模型 |
| `/provider` | 选择当前 API provider |
| `/config` | 编辑运行时和 provider 设置 |
| `/settings` | 查看持久化 UI 偏好 |
| `/skills` | 列出技能（`--remote` 浏览社区注册表；`/skill install <source>` 安装） |
| `/memory` | 查看记忆文件（提示符处 `# 文本` 快速追加笔记） |
| `/extension` / `/mods` | 管理扩展与脚本 mod（list、install、activate、reload） |
| `/compact` | 压缩长上下文以回收 token 预算 |
| `/review` | 请求结构化审查工作流 |
| `/mcp` | 配置或查看 MCP server 集成 |

实时模型发现也可以用 `codesmith models` 子命令。

使用 `/provider` 切换当前 provider。Provider ID、环境变量、模型默认值和能力说明保存在 provider 注册表文档中。

当你希望 CodeSmith 每轮自行选择模型和思考级别时，使用 `/model auto`。当你需要可重复的基准测试或严格的成本特征时，使用固定模型。

当会话变长且模型开始携带过多历史时，使用 `/compact`。压缩以原始会话记录的细节换取简洁的工作摘要。

本指南有意不列出每一条命令。命令面的变化比入门流程更频繁，而在会话中时，REPL 内的 `/help` 才是事实来源。

下一步：[CONFIGURATION.md](CONFIGURATION.md) 介绍运行时设置，[MCP.md](MCP.md) 介绍 Model Context Protocol 集成。

## 7. 使用工具

CodeSmith 工具是结构化动作。模型不是只能生成文字，而是可以调用工具来检查和修改工作区。

工具支撑的工作示例包括：

- 解释前先读取文件。
- 提出重构前先搜索调用点。
- 运行聚焦的测试命令。
- 应用小补丁。
- 打开子代理进行并行调查。

工具使用受模式、审批和沙箱策略约束。具体行为取决于当前模式和配置，但基本规则很简单：只读探索从 Plan 开始，常规修改用 Agent，YOLO 留给受信任的自动化。

工作区边界很重要。CodeSmith 应在你启动它的目录或你配置的工作区内工作。当任务应留在仓库内时，请明确说明：

```text
Only inspect and edit files under this repository. Do not touch parent
directories or global config.
```

当命令需要网络、写入工作区之外或执行有风险的 shell 操作时，除非你配置了更宽松的行为，否则应预期出现审批提示。每条审批提示都支持"带理由拒绝"：输入 `[r] <反馈>`——或者直接把你那句话打进去——它拒掉这一个调用，并把文字送到正在运行的回合，让模型知道该改什么，而不是猜你为什么说"不"。只有那四个键（`y`/`n`/`a`/`e`）才算回答问题本身。

`[a]` 记住的是一个**分组键**，而这个键覆盖什么，提示语本身就写明了：shell 命令是 `this command family`（`git status` 覆盖 `git status -s`，但不覆盖 `git push`）、链式命令是 `each of these command families`（`cargo build && cargo clippy`——之后某条命令的**每条**子命令族都被批准过才算覆盖）、被安全分析判定为危险的命令（`… | sh`、多行脚本）是 `this exact call`、补丁是 `patches to the same paths`、抓取是 `requests to the same host`，而没有这类族的工具是 `every <tool> call`——答一次 `write_file` 就覆盖它之后的调用，答一次 `agent_open` 就覆盖本次运行里之后的每次 spawn。所以对某个工具按 `[a]` 不会静音另一个工具的提示：每个你不想再被问到的工具都要各答一次。`[e]` 始终是精确的那一次调用：只拒掉这一个调用，别的都不受影响。`[a]` 和 `[e]` 都只活到进程退出；会写进 `~/.codesmith/approvals.toml`、重启后仍然有效的是 `/permissions allow <命令前缀>`。

每一条审批决定都会写进会话文件的 `approvals` 列表——工具名、两个键、你的回答、你输入的理由、工作区与时间——**没有真的问过你的那些也在内**（`session-grant`、`workspace-grant`、`session-deny`、`auto-mode`）。这是"你到底同意了什么"的记录，所以恢复一个会话后仍然能看出某次 patch 是人手批的、还是被早先某次"always"覆盖的。只记录：它不会到达模型，本身也不构成任何授权。

好的工具指令是具体的：

```text
Run the narrowest test that covers this parser change. If it fails, report the
failure and stop before broadening the test scope.
```

避免在聚焦修复期间要求大范围清理。更小的工具范围让会话记录更容易审查，最终 diff 更容易合并。

下一步：[TOOL_SURFACE.md](TOOL_SURFACE.md) 列出工具面，[SANDBOX.md](SANDBOX.md) 解释沙箱行为。

## 8. 子代理与并行工作

子代理是后台子 agent。父会话交给子代理一个聚焦的任务，收到一个 agent id，并可以在子代理运行时继续工作。

主要的编排工具包括：

- `agent_open`：以任务和角色启动子代理。
- `agent_eval`：等待并收集子代理的结果。
- `agent_close`：取消运行中的子代理。

你通常不需要直接调用这些工具。用自然语言请求并行工作：

```text
Open one read-only explorer for the config crate and another for the CLI
provider picker. Have both return file references and risks before we plan the
fix.
```

有用的角色包括：

| 角色 | 适用场景 |
| --- | --- |
| `general` | 多步骤任务；未指定角色时的默认值 |
| `explore` | 只读代码梳理 |
| `plan` | 设计与迁移规划 |
| `review` | 针对现有改动的聚焦 bug 审查 |
| `implementer` | 规格明确的编辑 |
| `verifier` | 运行检查并报告通过/失败证据 |

当工作可以被干净地拆分时，子代理最有用。不要把它们用于微小的编辑，也不要让多个代理同时写同一批文件。

下一步：[SUBAGENTS.md](SUBAGENTS.md) 介绍角色、生命周期、并发和输出契约。

## 9. 技能

技能（skill）是可复用的指令包。一个技能通常是一个 `SKILL.md` 文件，教会 CodeSmith 如何执行某个反复出现的工作流、使用某个工具家族或遵循某个项目约定。

当任务具有可重复的流程时使用技能：

- 审查特定类型的 PR。
- 处理文档或电子表格格式。
- 遵循团队发布清单。
- 使用项目专属的记忆或 wiki 工作流。

在 REPL 中，`/skill` 在有可用技能时激活技能，`/skills` 列出已安装的技能。

好的技能是聚焦的。它们应告诉模型遵循什么工作流、收集什么证据以及避免什么。它们不应隐藏凭据或取代常规的仓库文档。

如果仓库有自己的说明，请将其视为当前工作的一部分。编辑前先阅读本地指引，并让任何贡献符合仓库的约定。

下一步：完整的技能参考 —— 编写、发现目录、条件激活和社区安装 —— 见 [SKILLS.md](SKILLS.md)。[README.md](../README.md) 中的"Publishing Your Own Skill"一节是简短版本，配置细节见 [CONFIGURATION.md](CONFIGURATION.md)。

## 10. 获取帮助

从 doctor 输出开始：

```bash
codesmith doctor
```

在提交详细 issue 时使用 JSON：

```bash
codesmith doctor --json
```

对于身份验证问题，检查哪个来源在生效：已保存的配置、keyring、环境变量还是显式启动标志。一个过期的 `DEEPSEEK_API_KEY` 环境变量可能覆盖你预期使用的配置。

对于 provider 问题，确认当前 provider 和模型：

```text
/provider
/model
```

对于冗长或混乱的会话，使用 `/compact` 降低上下文压力，或在同一工作区开启新会话并概括你的需求。

报告 issue 时，请包含：

- CodeSmith 版本。
- 安装方式。
- 操作系统和终端。
- Provider 和模型。
- 确切的命令或提示词。
- 相关的 doctor 输出。
- 问题是否在全新的工作区中也会出现。

不要将 API key、私有源代码或密钥粘贴到公开 issue 中。

下一步：[OPERATIONS_RUNBOOK.md](OPERATIONS_RUNBOOK.md) 包含运维分诊和恢复步骤。

## 常见问题（FAQ）

### CodeSmith 只支持 DeepSeek 吗？

不是。DeepSeek 只是未配置 provider 时的回退；每一个已交付的 provider 都是一等的，托管与本地 OpenAI 兼容端点一律平等。使用 `/provider` 或 `codesmith --provider <id>` 选择 provider。配置路由时，请打开 provider 注册表文档。

### 我应该先用哪个模式？

不熟悉的代码用 Plan，常规实现用 Agent，YOLO 只用于自动执行可接受的受信任仓库。

### 为什么 CodeSmith 在运行命令前要先询问？

审批是安全模型的一部分。Shell 命令、付费工具、写入以及预期工作区之外的操作都可能产生副作用。审批提示让你保持控制，同时仍能让模型做有用的工作。

### 如何在 macOS 上运行 Python 文件？

在包含该文件的文件夹中打开终端并运行：

```bash
python3 your_file.py
```

如果 macOS 提示缺少 `python3`，请从 [python.org](https://www.python.org/downloads/macos/) 安装 Python 或使用 Homebrew：

```bash
brew install python
```

在 CodeSmith 中，让 agent 检查该文件并用 `python3 your_file.py` 运行它。如果脚本需要包，请先在虚拟环境中安装：

```bash
python3 -m venv .venv
source .venv/bin/activate
python3 -m pip install -r requirements.txt
python3 your_file.py
```

### 我的配置存储在哪里？

CodeSmith 的配置位于 `~/.codesmith/config.toml`。当存在工作区配置时，项目覆盖层也会影响行为。

### 如何保持成本可预测？

使用 `/model auto` 进行路由，需要严格成本特征时选择固定模型，并压缩长会话。对于较大的任务，让 CodeSmith 先规划再实现，避免在错误路径上浪费 token。

### 如何继续之前的工作？

CodeSmith 会保存会话。使用会话选择器或 README 与模式指南中记录的 resume/continue CLI 路径。对于有风险的实验，请在改变方向前分叉会话。

### 模型混乱时我该怎么办？

停下来，重新陈述目标、约束和当前证据。如果会话记录很长，使用 `/compact` 或以简短的交接开启新会话。如果问题出在运维层面，运行 `codesmith doctor` 并检查报告的配置和 provider 状态。

### 项目规则应该放在提示词还是文件里？

持久的项目规则放在仓库文件中，针对单轮的意图放在提示词中。如果某个工作流在多个项目中重复出现，可以考虑把它做成技能。

### CodeSmith 能编辑当前仓库之外的文件吗？

文件工具命中工作区之外的路径时不会直接失败，而是**先问**：

```text
⚠ path outside the workspace: /abs/sibling-repo (list_dir)
allow? [y] allow it now · [a] always allow it in this workspace (saved) · [n] skip:
```

`[y]` 本次回合放行该路径，`[a]` 还会为本工作区记住它（写入 `/trust add <path>`
用的同一份列表，可用 `/trust remove <path>` 撤销），`[n]` 保持拒绝。持久授权按工作区
记录，下一回合会重新读取，因此重启后受信任的兄弟仓库依然可用。子代理同样会问：
子代理命中边界时，问题会出现在派发它的那个会话里。

shell 命令的**读**本来就不受工作区限制（`rg`、`git -C ../other` 早就够得到另一个
检出）；这条提示约束的是内置文件工具——它们不能顺着模型自己编的路径走。对于贡献
工作，除非确实需要，请将指令范围限定在当前仓库内。

### 读完本指南后接下来去哪里？

阅读与你要修改的内容对应的专项参考。对大多数用户来说，接下来的页面是安装、配置、provider、模式、工具和子代理。

下一步：[INSTALL.md](INSTALL.md)、[CONFIGURATION.md](CONFIGURATION.md)、[PROVIDERS.md](PROVIDERS.md)、[PRESETS.md](PRESETS.md) 和 [TOOL_SURFACE.md](TOOL_SURFACE.md)。
