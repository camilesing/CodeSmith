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
的作用。`/plan`、`/agent`、`/yolo` 分别是对应 `/mode` 取值的快捷方式。

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

在 REPL 内，`/undo` 从对话记录中删除最后一次交换，`/retry` 删除后
重发同一条用户消息。`revert_turn` 是独立的
工作区回滚工具：它从 side-git 快照恢复文件，但不改写对话历史。

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
