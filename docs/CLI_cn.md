# CLI 参考

`codesmith --help` 是旗标与子命令的权威清单。常用命令面：

```bash
codesmith                                         # interactive REPL
codesmith -p "explain this function"              # open the REPL with this prompt submitted
codesmith exec "explain this function"            # one-shot, non-interactive prompt
codesmith exec --auto --output-format stream-json "fix this bug"  # NDJSON backend stream
codesmith exec --resume <SESSION_ID> "follow up"  # continue a non-interactive session
codesmith --yolo                                  # auto-approve tools
codesmith auth set --provider deepseek            # save API key
codesmith doctor                                  # check setup & connectivity
codesmith doctor --json                           # machine-readable diagnostics
codesmith setup --status                          # read-only setup status
codesmith setup --tools --plugins                 # scaffold tool/plugin dirs
codesmith models                                  # list live API models
codesmith sessions                                # list saved sessions
codesmith --skip-onboarding                        # bypass first-run onboarding questions
codesmith --continue                               # continue the most recent session in this workspace
codesmith resume --last                           # resume the most recent session in this workspace
codesmith resume <SESSION_ID>                     # resume a specific session by UUID
codesmith fork <SESSION_ID>                       # fork a saved session into a sibling path
codesmith serve --http                            # HTTP/SSE API server
codesmith serve --mobile                          # LAN mobile control page; token-gated by default
codesmith serve --acp                             # ACP stdio adapter for Zed/custom agents
codesmith pr <N>                                  # fetch PR and pre-seed review prompt
codesmith mcp list                                # list configured MCP servers
codesmith mcp validate                            # validate MCP config/connectivity
codesmith mcp-server                              # run dispatcher MCP stdio server
codesmith update                                  # check for and apply binary updates
codesmith version                                 # print the CLI version (same output as --version)
codesmith docker                                  # print the container quick start
```

## Prompt 与子命令

顶层第一个裸词按子命令名解析：`codesmith docker`、`codesmith version` 都是
命令；无法识别的词会以 `unrecognized subcommand '<word>'`（退出码 2）
报错，并提示改用 `-p`。顶层不存在位置参数形式的 prompt：加引号
（`codesmith "explain this function"`）、未加引号的尾串、`--` 终结符、
非 ASCII 裸词（`codesmith 总结`）都会以同样的方式报错。

prompt 的入口有两个：

- `codesmith -p <PROMPT>` —— 接受一个或多个值（拼接为初始 prompt），
  启动交互式 REPL 并把该 prompt 作为第一轮提交。它不是一次性的，
  会话会保持打开以便继续对话。
- `codesmith exec "<prompt>"` —— 非交互的一次性形式。

在 REPL 内，`/help` 是斜杠命令的实时清单——输入 `/` 会显示命令数量，
`Tab` 补全命令名（多个匹配时连按两次列出候选），光标后的暗色文字预览匹配命令
的作用。`/plan`、`/agent`、`/yolo` 分别是对应 `/mode` 取值的快捷方式，也可以在提示符下按 `Shift+Tab` 循环这些模式（`agent → plan → yolo`，实验性 `coordinator_mode` 功能开启时再加 `coordinator`）。命令按
用户自定义（`.codesmith/commands/*.md`）> mod 注册 > 内建的层序分派；
内建命令可以不删实现地关掉：config.toml 的 `[commands] disabled = [...]`
会让这些名字从分派、`/help` 与补全中消失（未知名称令启动大声失败），
mod 侧也可在运行期用 `disable_command(name)` 达到同样效果——两种情况下
同名的用户命令或 mod 命令都继续可用。

键盘与交互细节（提示符按键、回合中的 composer、vim 模式、shell 模式）见
[KEYBINDINGS_cn.md](KEYBINDINGS_cn.md)；在空提示符下按 `?` 会打印同一张表。

除核心命令集之外，REPL 还带上了本分支用户要求的 Claude Code 交互。值得知道的：

- **回合运行中也能输入。** `Enter` 排队、`Ctrl+X Ctrl+S` 立即发送、`Esc` 把正在跑的
  shell/子代理转后台或中断回合、`Ctrl+B` 把 shell 转后台、`Ctrl+X Ctrl+K` 停掉全部
  后台工作。排队消息会在它等待的工具调用结束时交给模型。
- **`!command`** 在你的 shell 中执行，并让模型基于其输出作答
  （`respond_to_shell_commands = false` 则只记录、不启动回合）。
- **`/btw <question>`** 只依据本会话已有对话作答、无工具，且不会把这次问答加进对话
  （记录写入 `~/.codesmith/side-questions.jsonl`）。
- **`/rewind [n|list]`** 把工作区恢复到引擎在改动文件的回合前后拍下的快照之一。
  仅文件：`/undo` 与 `/retry` 是对话那一半。
- **`/code-review`、`/security-review`、`/simplify`、`/run`、`/verify`** 是内置技能
  （提示词工作流），由 `/skills` 列出，磁盘上同名技能可覆盖它们。
- **`/fewer-permission-prompts [--yes]`** 读取你保存的会话，统计你跑得最多的只读命令
  （用的是审批提示同一套 `command_safety` 判定），并为每条提出一条审批规则 —— 只有
  加 `--yes` 才写入，写法与 `/permissions allow` 完全相同。Claude Code 的
  `/auto-mode-setup` 在本地也是这件事：两者最终都落到同一份 allowlist。
- **`/stop`** 停掉全部后台工作 —— shell 作业、后台子代理、持久任务 —— 前台回合不受
  影响（那是 `Ctrl+C` 的活）。Claude Code 的 `/stop` 针对已 attach 的后台*会话*，
  属于下表排除项。
- **`/skill-doctor`** 报告每个技能在上下文里的开销（正文大小与粗略 token 估算）以及
  它实际跑过多少次 —— 次数数的是激活写进会话日志的 `# Skill: <name>` 标记；从未用过
  的磁盘技能就是可以删掉的那些。
- **`/run-skill-generator`、`/update-config`、`/deep-research <question>`、
  `/plugin-authoring`** 是其余内置技能：教会 `/run` 与 `/verify` 本项目真正的构建与
  启动方式、把口语化的设置请求变成正确的配置文件改动、跨来源检索并带引用地回答问题、
  以及照 `docs/MODS.md` 写一个 mod。
- **`/branch [name]`** 在此处给对话开分支：当前会话被保存，副本成为活动会话，
  原会话留给 `/resume`。这是 Claude Code 对 `/fork` 早就做过的事情所用的名字；
  Claude Code 的 `/fork`（把副本当作游离的后台*会话*运行）在下表的排除清单里。
- **`/subtask <task>`** 把一件自足的任务交给后台子代理：它在当前工作区、用本会话的
  工具与审批路由运行，`/agents` 会列出它，完成后结果打印到本对话。子代理**不继承**
  对话 —— Claude Code 的 `/subtask` 会 fork 对话记录，而这里的对话继承属于模型的
  `agent_open`（`fork_context: true`），因此任务本身必须自足。
- **`/batch <instruction>`** 是内置技能：把大改动规划成 5–30 个独立单元，等待批准后
  为每个单元开一个后台子代理（`agent_open`，对会碰到同一批文件的单元用
  `git worktree add` 隔离），并逐单元汇报连同验证命令。
- **`/pr <number|url>`** 通过 `gh` 把 GitHub PR —— 标题、正文、分支与截断后的
  diff —— 取回成提示行里的可编辑草稿，于是在你已有的会话与工作区里审阅。
  `--checkout` 会先执行 `gh pr checkout`；`--repo owner/name` 读别的仓库。
  命令行入口 `codesmith pr <N>` 是同一份取回，只是用它开一个新会话。
- **`/insights [7d|30d|all]`** 打印本地存储对"你是怎么工作的"的记录 —— 会话、
  token、项目、模型、工具调用及其审批/成功计数、压缩、子代理启动 —— 并把一份
  自包含的 HTML 报告写到 `~/.codesmith/insights/`。它读的是 `codesmith metrics`
  读的同样三个来源，不上传任何内容。
- **`/loop <interval> <prompt>`**（别名 `/proactive`）把这条提示的周期运行存成一条
  automation，由会话的调度器作为后台任务触发 —— 它有自己的 runtime thread，
  所以**迭代不会出现在本对话里**。`/loop list` 显示每个循环的下次与上次运行，
  `/loop run|pause|resume|stop <id>` 管理单个循环；不带提示的 `/loop <interval>`
  使用工作区的 `loop.md` 或内置维护提示。下限是一小时（调度器的 RRULE 只有
  小时/周粒度），因此 `/loop 5m` 会被点名拒绝，而不是四舍五入。
- **`/feedback up|down [N] [评语]`** 给本会话的某条回复打分 —— 默认最近一条，或按
  `/copy N` 的数法取倒数第 N 条 —— 并把评分、评语、会话 id、消息索引、模型与时间戳
  作为一行 JSON 追加到 `~/.codesmith/feedback/feedback.jsonl`。评分只属于你：
  永不加进对话、永不发给模型，也不会上传。单独的 `/feedback` 打印用法与提 bug 的入口。
- **`/effort`、`/autocompact`、`/usage`、`/tasks`、`/plugins`、`/reload-skills`、
  `/reload-plugins`、`/theme`、`/color`、`/statusline`、`/copy`、`/doctor`、
  `/debug`、`/release-notes`、`/terminal-setup`、`/keybindings`** 覆盖模型档位、
  后台视图、扩展面，以及外观与诊断杂项。`/permissions` 展示哪些操作不必询问
  —— 会话授权、持久授权、受信任路径，以及 `[sandbox] auto_allow` 规则 ——
  并写入持久授权存储（`/permissions allow "git status"` 覆盖 `git status -s`
  而不覆盖 `git push`，用的是审批提示同一套 arity 词典）。`/cd <path>` 把会话移到
  另一个工作目录 —— 只能移进当前工作区子树或已受信任路径，因此移动永远不能成为
  访问未受信任目录树的手段 —— `/add-dir <path>` 则新增那条信任记录（即
  `/trust add` 写入的同一份记录）。`/import [claude|cursor|codex|gemini]` 读取
  其他 agent CLI 的配置，把其中的 MCP 服务器复制进 `~/.codesmith/mcp.json`
  （不加 `--yes` 不写任何东西）；它同时报告哪些面无需导入——因为 CodeSmith
  已经原生读取 `CLAUDE.md`/`.claude/instructions.md`（作为记忆）、
  `.claude/commands` 与 `.cursor/commands`（作为用户命令）、`~/.claude/skills`
  （技能注册表）——以及哪些没有对应物（`.claude/agents/*.md`）。
  `/memory auto [on|off]` 报告 agent 是否记录记忆，以及**是哪一层在做决定** ——
  环境变量可以压过配置，此时报告会点名那个变量 —— 并在要求修改时写入
  `[memory] enabled`。`/recap` 按需打印一句话总结，
  `/output-style` 切换 CodeSmith 具有的压缩风格（`is_simple`），`/resume`
  接受会话*名字*，无参数的 `/rename` 会用首条消息给会话命名。

### 改名的命令

有四个名字现在取 Claude Code 的含义，旧行为仍然可达：

| 之前 | 现在 |
| :- | :- |
| `/verify`（claim-check 台账统计） | `/verify` 运行并观察程序；台账统计改为 `/claim-check` |
| `/review <target>`（激活 review 技能） | `/review` 是 `/code-review` 的别名；技能走 `/review-skill` 或 `/skill review <target>` |
| `/undo`（丢弃最后一轮问答） | 不变 —— 它是 `/rewind`（文件）配对的对话那一半 |
| `/stats`（`/home` 别名） | `/stats` 是 `/usage` 的别名；`/home` 保留 `/overview` |
| `/tasks`（`/task` 别名） | `/tasks` 是后台视图（shell 作业 + 子代理 + 持久任务）；`/task` 保留 `add\|list\|show\|cancel` |

### 未实现

只存在于 Anthropic 自家云或账号面的命令没有本地对应物，因此刻意不注册，
`/help` 不会给出做不到的东西：`/desktop` `/mobile` `/teleport`
`/remote-control` `/remote-env` `/background`（后台*会话*）`/artifacts`
`/design` `/design-login` `/design-sync` `/slides` `/voice` `/chrome`
`/claude-in-chrome` `/ide` `/upgrade` `/passes` `/usage-credits`
`/rate-limit-options` `/privacy-settings` `/web-setup` `/install-github-app`
`/install-slack-app` `/autofix-pr` `/ultrareview` `/heapdump` `/radio`
`/stickers` `/powerup` `/team-onboarding` `/fast` `/scroll-speed`
`/tui` `/focus` `/keybindings-file` `/list-agents` `/peers` `/schedule`
内置技能之外的 `/simplify` 变体，以及 `/plugin` 编写面。`/fork` 保留 CodeSmith
的语义 —— 与 `/branch` 相同的"开分支并切过去" —— 因为 Claude Code 的 `/fork` 正是
上述排除项之一：游离的后台会话。

同样在范围之外并写明原因的：键位配置文件（绑定编译在程序内，`/keybindings` 只读）、
emoji 短代码与拼写检查（二者都需要本 REPL 未随附的数据表）、全屏渲染器的
transcript viewer 按键，以及 `/pr-comments`（Claude Code 在 v2.1.91 删除它并建议
直接让模型读评论 —— `/pr` 准备的正是让会话这么做的上下文）。

`/pr` 与 `codesmith pr <N>` 都走 `gh`：没有安装（或没跑过 `gh auth login`）时
`/pr` 会明说，并且什么都不取回。

其余若干条及其"本地没有"的原因：`/artifact-capabilities` 与
`/artifact-diagramming`（Anthropic 托管的 artifact，已在上表排除）、
`/advisor [model|off]`（第二模型咨询是接在 Claude 回合循环里的；本地对应的第二模型面是
`[subagents] model_overrides` 加 `agent_open`）、`/claude-api`（Anthropic API 参考）、
`/dataviz`（附带给颜色校验脚本的技能包，本构建没有这些脚本）、
`/setup-bedrock` 与 `/setup-vertex`（Anthropic 自家 Bedrock/Vertex 路径的鉴权向导；
CodeSmith 用 `/provider` 与 `codesmith login` 配置 provider）、
`/workflows` 与 `/workflow-authoring`（CodeSmith 没有动态 workflow 引擎；它的后台工作
是 `/tasks`、`/loop`、`/subtask`）、`/auto-mode-setup`（与
`/fewer-permission-prompts` 同一份 allowlist 工作）、以及 `/ultraplan`
（Claude Code 已删除；计划模式在这里同样是 `/plan`）。


## 模型选择

顶层 `--model <id>` 旗标为该次运行覆盖模型——它与 `CODESMITH_MODEL`
环境变量走同一槽位（DeepSeek 落根级 `model` 槽，其他 provider 落
provider 作用域槽），因此优先于 config.toml，并满足自定义网关的
显式模型检查。其他选择模型的方式：

- `~/.codesmith/config.toml` 中 `[providers.<id>]` 下的 `model = <id>`
- `CODESMITH_MODEL` 环境变量
- REPL 内的 `/model <id>`（按 provider 持久化，重启后仍生效）
- 单次非交互运行用 `codesmith exec --model <id>`

`codesmith models` 列出当前接入点的实时模型；`codesmith model list` 和
`codesmith model resolve` 查看内置注册表与解析结果。

## 首次运行引导

在没有 `~/.codesmith/.onboarded` 标记的机器上，裸运行 `codesmith` 会在
stdin 上依次询问——语言、提供商、自定义端点（网关类提供商）、API 密钥、
目录信任——然后把答案写入 `config.toml`、OS 钥匙串和语言设置，记录标记
并启动 REPL。已有凭据——提供商环境变量、已保存的密钥、或 Ollama 这类
自托管运行时——的启动会跳过密钥一问；裸运行时其余问题仍会询问。非终端
stdin 不会触发询问（此时已有凭据的启动会静默写入标记），子命令同样
不会询问：凭据缺失时第一轮对话以提供商认证错误失败。密钥以明文回显；
引导流程不提供 Moonshot Kimi OAuth——请用 `codesmith login`。
`--skip-onboarding` 跳过询问。

`/logout` 会移除 API 密钥和引导标记，下一次裸运行 `codesmith` 会重新
询问；`codesmith auth set --provider <id>`（提示输入密钥）和
`codesmith login` 可不经引导流程配置密钥。首启之后，信任只决定是否加载
项目级 config 和 `.env` 文件，由 `/trust` 管理。

在 REPL 内，裸 `/provider` 打印当前 provider，`/provider <id>` 切换
（仅当前会话生效；会打印重启提示，因为完整重配置需要新会话）。裸
`/model` 打印当前模型，`/model <id>` 切换并持久化。`/models` 从活动
端点拉取并列出实时模型（`--json` 输出 pretty JSON；不支持 `/models`
目录的 provider 会报告空列表）——与 `codesmith models` 子命令同一
取数路径。

## 会话：分支与回滚

已保存的会话刻意设计为可分支。`codesmith fork <SESSION_ID>` 把一个
已有的已保存会话复制为新的同级会话，在元数据中记录父会话 id，并打开
该 fork，让你可以在不污染原路径的情况下探索另一个方向。会话选择器和
`codesmith sessions` 会以父 id 标记 fork 出的会话。

恢复加载对中断是安全的：一个在工具执行中途被打断（Ctrl-C、调用与
结果之间崩溃）而保存的会话，加载时会为每个未应答的调用补上
`TOOL_OUTCOME_UNKNOWN` 终结器，使恢复后的对话记录仍满足 provider 的
tool_call/tool_result 配对要求。由更新版本 CodeSmith 写入的未知内容块
类型在加载时被忽略，而不是让整个会话加载失败。

在 REPL 内，`/undo` 从对话记录中删除最后一次交换，`/retry` 删除后
重发同一条用户消息。`revert_turn` 是独立的
工作区回滚工具：它从 side-git 快照恢复文件，但不改写对话历史。

`codesmith serve --acp` 的会话就是同一份记录：ACP 适配器把编辑器里每个完成的
回合以普通 UUID id 保存到 `~/.codesmith/sessions/`；`session/list` 枚举存储
（编辑器传 `cwd` 时按项目过滤）；`session/resume`（同时响应协议名
`session/load`）加载一个会话并回放其回合，因此对话可以在编辑器与终端的
`/resume` 之间自由流转。`session/close` 丢弃内存中的会话；记录保留。
ACP 提示词不带工具运行，因此该表面暂无权限提示。

更多细节见 [PRESETS.md — Branching and Rollback](PRESETS.md#branching-and-rollback)。

## Docker

发布镜像发布在 GHCR：

```bash
docker volume create codesmith-home

docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/camilesing/codesmith:latest
```

固定 tag、本地镜像构建、卷所有权说明和非交互流水线用法：
[DOCKER.md](DOCKER.md)。

`codesmith docker` 会打印同一条 run 命令，且不需要启动 REPL，因此在
纯容器或 CI shell 里同样可用。
