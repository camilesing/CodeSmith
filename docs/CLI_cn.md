# CLI 参考

`codesmith --help` 是旗标与子命令的权威清单。常用命令面：

```bash
codesmith                                         # interactive TUI
codesmith "explain this function"                 # one-shot prompt
codesmith exec --auto --output-format stream-json "fix this bug"  # NDJSON backend stream
codesmith exec --resume <SESSION_ID> "follow up"  # continue a non-interactive session
codesmith --model deepseek-v4-flash "summarize"   # model override
codesmith --model auto "fix this bug"             # auto-select model + thinking
codesmith --yolo                                  # auto-approve tools
codesmith auth set --provider deepseek            # save API key
codesmith doctor                                  # check setup & connectivity
codesmith doctor --json                           # machine-readable diagnostics
codesmith setup --status                          # read-only setup status
codesmith setup --tools --plugins                 # scaffold tool/plugin dirs
codesmith models                                  # list live API models
codesmith sessions                                # list saved sessions
codesmith resume --last                           # resume the most recent session in this workspace
codesmith resume <SESSION_ID>                     # resume a specific session by UUID
codesmith fork <SESSION_ID>                       # fork a saved session into a sibling path
codesmith serve --http                            # HTTP/SSE API server
codesmith serve --mobile                          # LAN mobile control page; token-gated by default
codesmith serve --acp                             # ACP stdio adapter for Zed/custom agents
codesmith run pr <N>                              # fetch PR and pre-seed review prompt
codesmith mcp list                                # list configured MCP servers
codesmith mcp validate                            # validate MCP config/connectivity
codesmith mcp-server                              # run dispatcher MCP stdio server
codesmith update                                  # check for and apply binary updates
codesmith version                                 # print the CLI version (same output as --version)
codesmith docker                                  # print the container quick start
```

## Prompt 与子命令

顶层第一个裸词按子命令名解析：`codesmith docker`、`codesmith version` 都是
命令；无法识别的词会以 `unrecognized subcommand '<word>'`（退出码 2）报错，
而不是打开交互界面。

prompt 的既有形式不变：`codesmith "explain this function"`（单个带引号的
参数）、`codesmith hello world`（未加引号的整个尾串），单词 prompt 则要走
`codesmith -p <PROMPT>`，例如 `codesmith -p docker`。`--` 终结符同样结束
子命令匹配，`codesmith -- docker` 也会把 `docker` 作为 prompt 发送。

没给 `-p` 时，单个 ASCII 词无法与拼错的命令区分，因此会被拒绝并在提示
中指向 `-p`；非 ASCII 词（`codesmith 总结`）仍按 prompt 处理。

在 TUI 内，`/provider` 打开 provider 选择器，`/model` 打开本地模型/
思考选择器。`/provider openrouter` 和 `/model <id>` 直接切换；当当前
provider 支持模型列举时，`/models` 会显式拉取并列出实时的 API 模型。

两个输入框前缀与斜杠命令互补：`!cmd` 直接在你的 shell 中运行命令，
并把捕获的输出提交到会话；`:name:` 展开 emoji 短代码（输入时会出现
建议弹窗）。完整的输入框编辑按键集见 [KEYBINDINGS.md](KEYBINDINGS.md)。

## 会话：分支与回滚

已保存的会话刻意设计为可分支。`codesmith fork <SESSION_ID>` 把一个
已有的已保存会话复制为新的同级会话，在元数据中记录父会话 id，并打开
该 fork，让你可以在不污染原路径的情况下探索另一个方向。会话选择器和
`codesmith sessions` 会以父 id 标记 fork 出的会话。

在 TUI 内，Esc-Esc 回退可以把实时对话记录回退到之前的某个用户提示，
并把该提示放回输入框以便编辑。`/restore` 和 `revert_turn` 是独立的
工作区回滚工具：它们从 side-git 快照恢复文件，但不改写对话历史。

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

`codesmith docker` 会打印同一条 run 命令，且不需要 TUI 二进制，因此在
纯容器或 CI shell 里同样可用。
