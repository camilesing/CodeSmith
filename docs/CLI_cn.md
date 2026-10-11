# CLI 参考

`codesmith --help` 是旗标与子命令的权威清单。常用命令面：

```bash
codesmith                                         # interactive REPL
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
codesmith --skip-onboarding                        # bypass first-run onboarding questions
codesmith --continue                               # continue the most recent session in this workspace
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
而不是启动 REPL。

prompt 的既有形式不变：`codesmith "explain this function"`（单个带引号的
参数）、`codesmith hello world`（未加引号的整个尾串），单词 prompt 则要走
`codesmith -p <PROMPT>`，例如 `codesmith -p docker`。`--` 终结符同样结束
子命令匹配，`codesmith -- docker` 也会把 `docker` 作为 prompt 发送。

没给 `-p` 时，单个 ASCII 词无法与拼错的命令区分，因此会被拒绝并在提示
中指向 `-p`；非 ASCII 词（`codesmith 总结`）仍按 prompt 处理。

## 首次运行引导

在没有 `~/.codesmith/.onboarded` 标记的机器上，裸 `codesmith` 会在 REPL
启动前以纯命令行问答完成引导：语言 → 提供商 → 接入地址（仅
OpenAI/Anthropic 路由）→ API 密钥 → 目录信任。答案写入与原 TUI 向导
相同的持久化位置（`~/.codesmith/config.toml`、settings 的 locale、
OS 钥匙串、信任表）。已有凭据——提供商环境变量、已保存的密钥、或
Ollama 这类自托管运行时——会跳过密钥问题；拒绝信任提示则直接退出，
不写入任何内容。传 `--skip-onboarding` 可完全跳过问答。

在 REPL 内，两个门控仍可能拦截后续会话：凭据缺失（例如 `/logout` 之后）
会重新弹出密钥屏；首次进入未信任的目录会重新弹出信任提示。

在 REPL 内，`/provider` 打开 provider 选择器，`/model` 打开本地模型/
思考选择器。`/provider openrouter` 和 `/model <id>` 直接切换；当当前
provider 支持模型列举时，`/models` 会显式拉取并列出实时的 API 模型。

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
