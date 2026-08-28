# 技能

技能（skill）是一个可复用的指令包：一个包含 `SKILL.md` 文件的目录，
教会 CodeSmith 如何执行某个反复出现的工作流、使用某个工具族，或遵循
某个项目约定。技能是声明式 Markdown——无需编译代码，无需注册步骤。
把一个带 `SKILL.md` 的目录放进任意发现位置，下一个会话就会自动识别
它。

CodeSmith 使用渐进式披露来保持上下文精简：会话提示只列出每个技能的
名称、描述和路径（上限 12,000 个字符）。完整的 `SKILL.md` 正文按需
加载——由模型通过 `load_skill` 工具加载，或由你通过 `/skill` 命令
加载。

本文档是完整参考：发现位置、`SKILL.md` 格式、激活路径、命令、社区
安装和故障排查。

## 快速开始

1. 为你的技能创建一个目录：

   ```bash
   mkdir -p ~/.codesmith/skills/release-checklist
   ```

2. 编写一个带 YAML frontmatter 的 `SKILL.md`：

   ```markdown
   ---
   name: release-checklist
   description: Use when cutting a release — walks the full v4 release checklist and gates each step on verification.
   ---

   # Release Checklist

   1. Run `cargo test --workspace` and confirm zero failures.
   2. Update CHANGELOG.md with the new version and today's date.
   3. ... (your steps here)
   ```

3. 重启 TUI（或开启新会话）并验证：

   ```text
   /skills
   ```

你的技能现在出现在模型可见的技能目录中、`/` 补全菜单中，并且可以用
`/skill release-checklist` 或简写的 `/release-checklist` 调用。

## 发现目录与优先级

CodeSmith 按顺序扫描以下目录，并合并找到的一切。只扫描实际存在的
目录；当两个技能的 frontmatter `name` 相同时，先命中者胜出。

| # | 作用域 | 目录 |
|---|---|---|
| 1 | workspace | `<workspace>/.agents/skills` |
| 2 | workspace | `<workspace>/skills` |
| 3 | workspace | `<workspace>/.opencode/skills` |
| 4 | workspace | `<workspace>/.claude/skills` |
| 5 | workspace | `<workspace>/.cursor/skills` |
| 6 | workspace | `<workspace>/.codesmith/skills` |
| 7 | global | `~/.agents/skills` |
| 8 | global | `~/.claude/skills` |
| 9 | global | `~/.codesmith/skills` |
| 10 | global | `~/.codesmith/skills`（旧版回退） |

这些跨工具位置（`.agents`、`.opencode`、`.claude`、`.cursor`）意味着
你已为其他代理维护的技能可原样复用——无需复制或建立符号链接。

关于目录遍历方式的说明：

- **嵌套布局没问题。** 任何包含 `SKILL.md` 的目录就是一个技能，遍历
  不会深入其下，因此你可以按厂商或类别组织——
  `<root>/<vendor>/<skill>/SKILL.md`——最大深度为 8。隐藏子目录
  （`.git`、缓存）会被跳过；符号链接目录会被跟随。
- **自定义安装目录。** 顶层 `skills_dir` 配置键（默认
  `~/.codesmith/skills`）是 `/skill install` 放置新技能的位置。如果
  你把它指到标准集合之外，它会被追加到发现列表。参见
  [CONFIGURATION.md](CONFIGURATION.md)。

## SKILL.md 格式

一个 `SKILL.md` 是一个 YAML frontmatter 块加一个 Markdown 正文。只有
`name` 是必填的。

```markdown
---
name: my-skill
description: One sentence telling the model when this skill applies.
when_to_use: Trigger hints for the model, shown alongside the description.
allowed-tools: read_file, list_dir, exec_shell
paths:
  - "docs/**/*.md"
version: 1.0.0
---

# My Skill

Instructions for the agent go here.
```

### Frontmatter 字段

| 字段 | 必填 | 含义 |
|---|---|---|
| `name` | 是 | 技能标识符；用于 `/skill <name>` 查找和冲突解析。 |
| `description` | 推荐 | 模型在目录中阅读以判断相关性的一句话。写成"use this when …"的形式。 |
| `when_to_use` | 否 | 额外的触发提示，展示在描述旁边。 |
| `allowed-tools` | 否 | 技能预期使用的工具名，逗号分隔或 YAML 列表。`allowed-tools` 和 `allowed_tools` 两种拼法都接受。 |
| `model` | 否 | 使用此技能的运行的首选模型。 |
| `effort` | 否 | 首选思考强度级别。 |
| `user-invocable` | 否 | `false` 会将技能从 `/skills`、`/` 菜单和直接调用中隐藏——它只保持模型可选。默认 `true`。 |
| `paths` | 否 | 条件激活技能的路径 glob（见下文）。 |
| `version` | 否 | 版本字符串；供内置技能升级器和社区安装使用。 |
| `context` | 否 | 执行的上下文提示。 |
| `agent` | 否 | 子代理角色提示（参见 [SUBAGENTS.md](SUBAGENTS.md)）。 |
| `shell` | 否 | 片段的 shell 偏好。 |

值得了解的解析细节：

- 列表字段接受逗号分隔字符串、YAML 块列表或 `[a, b]` 内联列表。长
  描述可使用 YAML 块标量（`>`、`|`，支持 `>-`/`|+` 修整）。
- **纯 Markdown 回退：** 没有 `---` frontmatter 围栏的文件也会被
  接受——第一个 `# Heading` 成为名称，整个文件成为正文。适合快速
  记录，但显式的 `description` 会让自动选择可靠得多。

### 用 `paths` 做条件激活

当技能声明了 `paths` 时，CodeSmith 会将 glob 与当前工作集中的文件
匹配，命中时将该技能作为*匹配条件技能*注入当轮——即使模型从未请求
过它。Glob 语义是 gitignore 风格：以 `/` 分段，段内 `*` 和 `?`，
跨段 `**`（例如 `docs/**/*.md`）。

这适合"只要动了 `ops/` 下的文件，就遵循 runbook"式的技能。

### 编写好的技能正文

- **保持窄域。** 一个技能一个工作流。试图面面俱到的技能什么也选不
  上。
- **告诉模型收集什么证据、避免什么**，而不仅仅是要做什么。
- **保持正文自包含。** 技能激活时正文被原样注入；它不应假设用户的
  消息提供了它自己就能说明的上下文。
- 不要把秘密放进技能——它们是纯文本文件，常通过 git 同步或从第三方
  安装。

## 技能如何激活

共有四条激活路径；对任何已发现的技能都无需额外设置即可工作。

1. **模型自动选择。** 会话提示携带一个 `## Skills` 目录（名称、描述、
   路径）。当你的任务与某个描述匹配时，模型调用 `load_skill` 工具
   读取完整正文，并在本轮余下时间遵循它。
2. **条件路径匹配。** 带有 `paths:` frontmatter 的技能在工作集文件
   匹配时自动注入（见上文）。
3. **手动调用。** `/skill <name>` 在你的下一条消息上激活技能；任何
   用户可调用的技能也可直接作为 `/<skill-name>` 调用——技能在原生命
   令和用户自定义命令之后加入斜杠命令命名空间——并且全部出现在 `/`
   补全菜单中。被激活技能的渲染正文会作为该轮指令前置于你的下一条
   消息。
4. **HTTP 运行时 API。** `GET /v1/skills` 列出技能，
   `POST /v1/skills/{name}` 启用或禁用一个（持久化到
   `~/.codesmith/skills_state.toml`）。参见 [RUNTIME_API.md](RUNTIME_API.md)。

## 命令参考

| 命令 | 效果 |
|---|---|
| `/skills` | 列出本地发现的技能（如有解析警告，一并显示）。 |
| `/skills <prefix>` | 按名称前缀过滤本地列表。 |
| `/skills --remote` | 浏览精选社区注册表而非本地技能。 |
| `/skills sync` | 拉取注册表索引并把每个精选技能下载到本地缓存。 |
| `/skill <name>` | 为你的下一条消息激活一个技能。 |
| `/skill new` | 启动内置的 `skill-creator` 技能来搭建新技能。 |
| `/skill install <source>` | 安装一个社区技能（见下文）。 |
| `/skill update <name>` | 从其来源刷新一个已安装的技能。 |
| `/skill uninstall <name>` | 移除一个已安装的技能。 |
| `/skill trust <name>` | 将技能标记为受信任，解锁其 shell 片段。 |

## 安装社区技能

`/skill install <source>` 接受三种来源形式：

| 来源形式 | 解析为 |
|---|---|
| `github:owner/repo` | `https://github.com/<owner>/<repo>` 归档（分支 `main`，404 时回退 `master`）。 |
| `https://github.com/owner/repo` | 同上——裸仓库 URL 会被识别。 |
| 任何其他 `http(s)://…` URL | 直接用作 tarball URL。 |
| 其他任何内容 | 精选注册表中的查找键。 |

相关配置，位于 `~/.codesmith/config.toml` 的 `[skills]` 表下：

```toml
[skills]
registry_url = "https://raw.githubusercontent.com/camilesing/codesmith-skills/main/index.json"
max_install_size_bytes = 5_242_880   # 5 MiB default
```

安装受 `[network]` 策略的网络门控（`github.com` 和
`raw.githubusercontent.com` 必须可达；默认的 `prompt` 模式询问一次并
可持久化）。下载的归档落在 `~/.codesmith/cache/skills/` 并解包到你的
技能目录，同时以 `.installed-from` 标记记录来源，供日后的
`/skill update` 使用。

### 信任与 shell 片段

技能正文可以以两种形式嵌入 shell 片段：以 <code>```!</code> 开头的
围栏块和内联的 <code>!`command`</code> 反引号片段。对于**不受信任**
的技能，这些在加载时被替换为一个禁用的占位符——模型能看到片段存在
但无法运行。运行 `/skill trust <name>` 会在 `SKILL.md` 旁放置一个
`.trusted` 标记文件；受信任的片段随后被透传，并附带通过
`exec_shell` 工具执行的指令，因此常规审批和沙箱策略仍然适用。信任
是按技能划分且需要刻意为之的：在信任一个社区技能之前，先审查它想
运行什么。MCP 提示技能（见下文）从不会启用 shell 片段。

## 伴随文件

`SKILL.md` 旁边的所有内容——辅助脚本、模板、参考文档——都是技能的
一部分。当模型通过 `load_skill` 加载技能时，它还会收到这些伴随文件
的列表（不含嵌套子目录），以便打开需要的文件。这是发布技能所依赖
脚本的标准方式：

```text
my-skill/
├── SKILL.md
├── extract_metrics.py
└── template.md
```

## 内置系统技能

首次启动会把一组内置技能安装到 `~/.codesmith/skills`（或旧版
`~/.codesmith/skills`）：`skill-creator`、`delegate`、
`v4-best-practices`、`plugin-creator`、`skill-installer`、
`mcp-builder`、`documents`、`presentations`、`spreadsheets`、`pdf`
和 `feishu`。内置集合带版本：升级会新增引入的技能，但绝不会重建你
刻意删除的技能。

`/skill new` 激活 `skill-creator`，引导你编写一个格式规范的技能。
暴露提示的 MCP 服务器也会将其提示呈现为技能（标记为 MCP 来源）；
参见 [MCP.md](MCP.md)。

## 故障排查

- **技能不出现在 `/skills`。** 检查目录是否是上述发现位置之一（且
  存在），以及 `SKILL.md` 文件名是否精确。`/skills` 会打印解析
  警告——frontmatter 中缺少 `name`（又没有 `# Heading` 回退）是
  最常见的原因。
- **错误的技能在名称冲突中胜出。** 发现按表中顺序先命中者胜——
  工作区 `.agents/skills` 中的技能会遮蔽具有相同 `name` 的全局
  `~/.codesmith/skills` 技能。重命名其中一个，或通过
  `POST /v1/skills/{name}` 禁用另一个。
- **Shell 片段显示 "disabled until trusted"。** 审查技能后运行
  `/skill trust <name>`。
- **技能激活得过于频繁。** 锐化 `description`（模型依据它进行选择），
  或用 `paths:` 加门，使其只在匹配文件时出现。
- **技能对模型可见但不在 `/` 菜单中。** 它被标记为
  `user-invocable: false`、在 `~/.codesmith/skills_state.toml` 中被
  禁用，或来自 MCP。

关于 `[skills]` / `skills_dir` 配置面参见 [CONFIGURATION.md](CONFIGURATION.md)；
技能内部机制参见 [ARCHITECTURE.md](ARCHITECTURE.md)。编译型 Rust
扩展（工具、命令、事件处理器）是另一种独立机制，见
[EXTENSIONS.md](EXTENSIONS.md)。
