# 配置

codesmith 从一个 TOML 文件加上环境变量中读取配置。
进程启动时，如果存在工作区本地的 `.env` 文件，也会将其加载。
请使用纳入版本控制的 `.env.example` 作为模板；将其复制为 `.env`，
然后只编辑你需要的 provider 和安全相关选项。

## 配置查找位置

默认配置路径：

- `~/.codesmith/config.toml`
- 旧版回退：`~/.deepseek/config.toml`

覆盖方式：

- CLI：`codesmith --config /path/to/config.toml`
- 环境变量：`CODESMITH_CONFIG_PATH=/path/to/config.toml`
- 旧版环境变量别名：`DEEPSEEK_CONFIG_PATH=/path/to/config.toml`

如果两者都设置了，`--config` 优先。环境变量覆盖在文件加载之后应用。

### 按项目叠加（#485）

当 TUI 在包含 `<workspace>/.codesmith/config.toml` 文件的工作区中启动时，
该文件中声明的值会合并到全局配置之上。当 CodeSmith 路径不存在时，
仍会读取旧版 `<workspace>/.deepseek/config.toml` 文件。这使得仓库可以
锁定自己的 provider、模型、沙箱策略或审批策略，而无需改动用户的
`~/.codesmith/config.toml`。传入 `--no-project-config` 可在单次启动时
跳过该叠加。

项目叠加中支持的键（仅限顶层字段）：

| 键 | 作用 |
|---|---|
| `provider` | 切换后端（例如企业仓库使用 `"nvidia-nim"`） |
| `model` | 覆盖 `default_text_model` |
| `api_key` | 使用按仓库的密钥（通常从 `.env` 读取，**不提交到版本库**） |
| `base_url` | 指向自托管端点 |
| `reasoning_effort` | 为复杂仓库强制使用 `"high"` / `"max"` |
| `approval_policy` | 为有强约定的仓库设置 `"never"` / `"on-request"` / `"untrusted"` |
| `sandbox_mode` | `"read-only"` / `"workspace-write"` / `"danger-full-access"` |
| `mcp_config_path` | 按仓库的 MCP 服务器集合 |
| `notes_path` | 将笔记保留在仓库内 |
| `personality` | 按仓库的语气/风格叠加（`"calm"` / `"playful"`） |
| `max_subagents` | 为受限仓库钳制并发数（钳制在 1..=20） |
| `allow_shell` | 设为 `false` 以关闭 shell 工具访问 |

该叠加刻意保持狭窄——它只覆盖仓库维护者最可能希望在贡献者之间
标准化的字段。其他设置（skills_dir、hooks、capacity、retry 等）
保持为用户全局配置。如果你的仓库需要更多，请提交一个 issue
描述具体使用场景。

`codesmith` 门面（facade）和 `codesmith-tui` 二进制文件共享同一个配置文件，
用于 DeepSeek 认证和模型默认值。`codesmith auth set --provider deepseek`
（以及旧版别名 `codesmith login --api-key ...`）将密钥保存到
`~/.codesmith/config.toml`（必要时会在首次启动时迁移旧版
`~/.deepseek/config.toml`），而 `codesmith --model deepseek-v4-flash`
会作为 `DEEPSEEK_MODEL` 转发给 TUI。

凭证查找在任何显式 CLI `--api-key` 之后按 `config -> keyring -> env`
的顺序进行。运行 `codesmith auth status` 可以查看当前活跃 provider 的
配置文件、OS 密钥环后端、环境变量、胜出来源以及末四位标签，
而不会打印密钥本身。该命令只探测当前活跃 provider 的密钥环条目。

对于托管型、通用 OpenAI 兼容型或自托管 provider，可以设置
`provider = "nvidia-nim"`、`"openai"`、`"atlascloud"`、`"wanjie-ark"`、
`"volcengine"`、`"openrouter"`、`"xiaomi-mimo"`、`"novita"`、`"fireworks"`、
`"siliconflow"`、`"moonshot"`、`"sglang"`、`"vllm"` 或 `"ollama"`，或者传递
`codesmith --provider <name>`。
关于逐个 provider 的注册信息（包括认证变量、默认 base URL、模型 ID
和能力元数据），请参阅 [PROVIDERS.md](PROVIDERS.md)。
门面会将 provider 凭证保存到共享的用户配置中，并将解析出的密钥、
base URL、provider 和模型转发给 TUI 进程。使用
`codesmith auth set --provider nvidia-nim --api-key "YOUR_NVIDIA_API_KEY"` 或
`codesmith auth set --provider openai --api-key "YOUR_OPENAI_COMPATIBLE_API_KEY"` 或
`codesmith auth set --provider atlascloud --api-key "YOUR_ATLASCLOUD_API_KEY"` 或
`codesmith auth set --provider wanjie-ark --api-key "YOUR_WANJIE_API_KEY"` 或
`codesmith auth set --provider xiaomi-mimo --api-key "YOUR_XIAOMI_KEY"` 或
`codesmith auth set --provider fireworks --api-key "YOUR_FIREWORKS_API_KEY"` 或
`codesmith auth set --provider siliconflow --api-key "YOUR_SILICONFLOW_API_KEY"`
通过门面保存 provider 密钥。通用 `openai` provider 默认使用
`https://api.openai.com/v1`，接受 `OPENAI_BASE_URL`，默认模型为
`gpt-5`。将 `OPENAI_BASE_URL` 指向第三方网关时需要显式指定模型
（`OPENAI_MODEL`、`--model` 或 `[providers.openai].model`）；
否则启动会快速失败，而不是将 provider 默认模型发送给无法提供
该模型的网关。`atlascloud` 默认使用
`https://api.atlascloud.ai/v1`，接受 `ATLASCLOUD_BASE_URL`，并使用
`deepseek-ai/deepseek-v4-flash` 作为默认模型。`wanjie-ark` 指向
Wanjie Ark 的 OpenAI 兼容端点
`https://maas-openapi.wanjiedata.com/api/v1`，默认使用 `deepseek-reasoner`，
并且模型 ID 原样透传，因为 Wanjie 的模型访问是账户作用域的。
SGLang、vLLM 和 Ollama 是自托管的，默认可以在没有 API 密钥的情况下
运行。Ollama 默认使用 `http://localhost:11434/v1`，并原样发送诸如
`codesmith-coder:1.3b` 或 `qwen2.5-coder:7b` 的模型标签。自托管 provider
和环回自定义 URL（`localhost`、`127.0.0.1`、`[::1]`、`0.0.0.0`）不会
读取密钥存储，除非显式要求 API 密钥认证；当本地服务器确实需要
bearer 认证时，请使用环境变量或配置文件中的密钥。
SiliconFlow 默认使用 `https://api.siliconflow.com/v1`，接受
`SILICONFLOW_BASE_URL`，并默认使用 `deepseek-ai/DeepSeek-V4-Pro`。
当用户需要区域端点时，仍可显式配置
`https://api.siliconflow.cn/v1`。

### 自定义 OpenAI 兼容网关

对于实现了 OpenAI Chat Completions API 的第三方服务，使用内置的
`openai` provider 名称，并将其 provider 表指向该网关：

```toml
provider = "openai"
default_text_model = "your-model-id"

[providers.openai]
api_key = "YOUR_OPENAI_COMPATIBLE_API_KEY"
base_url = "https://your-gateway.example/v1"
```

不要发明自定义 provider 名称；`provider` 必须是上面列出的已知
provider 之一。将端点放在 `[providers.openai]` 之下，而不是旧版的
顶层 `base_url`，这样 OpenAI 兼容 provider 才能收到它。
`default_text_model` 是发送给网关的模型 ID；如果在一个配置中保留
多个 provider 表，可以使用 `[providers.openai].model` 作为 OpenAI
provider 专属的覆盖值。

当 Ollama、SGLang 和 vLLM 等本地 HTTP 端点使用 localhost 或环回地址时，
默认是允许的。对于非本地 `http://` 网关，仅在可信网络上使用
`DEEPSEEK_ALLOW_INSECURE_HTTP=1` 启动：

```bash
DEEPSEEK_ALLOW_INSECURE_HTTP=1 codesmith
```

需要额外请求头的第三方 OpenAI 兼容网关，可以在顶层或 provider 表
（如 `[providers.deepseek]`）下设置
`http_headers = { "X-Model-Provider-Id" = "your-model-provider" }`。
配置后，codesmith 会在模型 API 请求中发送这些自定义请求头。等价的
环境变量覆盖是 `DEEPSEEK_HTTP_HEADERS`，使用逗号分隔的 `name=value`
对，例如 `X-Model-Provider-Id=your-model-provider,X-Gateway-Route=dev`。
`Authorization` 和 `Content-Type` 由客户端管理，不会被此设置覆盖。

### 视觉模型

CodeSmith 的聊天 provider 和 `image_analyze` 工具是分开配置的。
主聊天路径仍然是所选的文本/工具 provider；当启用 `vision_model`
功能时，图像分析通过 `[vision_model]` 运行。

小米当前的图像理解文档中包含用于图像输入的 `mimo-v2.5`。
要在 `image_analyze` 中使用 MiMo，请显式配置视觉模型：

```toml
[features]
vision_model = true

[vision_model]
model = "mimo-v2.5"
api_key = "YOUR_XIAOMI_KEY"
base_url = "https://api.xiaomimimo.com/v1"
```

### 辅助模型

`[utility_model]` 指定一个廉价/快速的次级 LLM 用于后台辅助，
让主模型的预算花在实际对话上：

- Workshop 大输出综合（#548）：超过 `large_output_threshold_tokens`
  阈值的工具结果由该模型压缩；只有综合结果进入父上下文，原始文本
  保留在 workshop 变量 `last_tool_result` 中（工具调用上设置
  `raw = true` 可绕过路由）
- 自动路由分类（`/model auto` 和子代理路由）
- Flash seam 默认值（`[context] seam_model`，当该键未设置时）

当该表缺失时，每个辅助任务都使用主模型——对单模型配置而言
行为不变。

```toml
[utility_model]
model = "deepseek-v4-flash"        # required: setting the table enables it
# provider = "openai"              # optional: defaults to the main provider
# api_key = "YOUR_API_KEY"         # optional: defaults to the main api_key
# base_url = "https://..."         # optional: defaults to the main base_url
```

相同 provider 的配置会复用主客户端并按请求覆盖模型；不同的
`provider` 则会构建一个专门的第二个客户端（例如主模型 = anthropic，
辅助模型 = deepseek），此时需要自己的 `api_key`——一家厂商的密钥
永远不会被发送给另一家。模型 id 也可以通过 `CODESMITH_UTILITY_MODEL`
（旧版别名 `DEEPSEEK_UTILITY_MODEL`）设置。

### 代码索引

`[index]` 配置按工作区持久化的代码索引，它支撑 `symbol_search` 和
`find_references` 工具（参见 `docs/INDEX.md`）。该表是可选的——
缺失时表示启用，使用内置的 `tree-sitter` 后端支持 rust/python/js/ts/go，
SQLite 存储位于 `~/.codesmith/index/<workspace>/`，并在每次查询时进行
惰性增量刷新（没有文件监视器）：

```toml
[index]
enabled = true              # master switch (CODESMITH_INDEX_ENABLED)
refresh_budget_ms = 2000    # per-query incremental refresh budget

[index.symbols]
backend = "tree-sitter"     # backend id (CODESMITH_INDEX_SYMBOLS_BACKEND)
[index.symbols.languages]   # per-language switches, absent = enabled
python = false              # e.g. skip python
```

设置 `enabled = false`（或 `[index.symbols] enabled = false`）会将
这两个工具从会话目录中完全移除。`[index.semantic]` 是为基于嵌入的
搜索预留的接缝（seam），但尚无内置后端——请保持禁用。

要在解析出的路径上初始化 MCP 和 skills 目录，请运行 `codesmith-tui setup`。
只搭建 MCP，请运行 `codesmith-tui mcp init`。

注意：setup、doctor、mcp、features、sessions、resume/fork、exec、review 和 eval
是 `codesmith-tui` 二进制文件的子命令。`codesmith` 调度器提供的是另一组
命令（`auth`、`config`、`model`、`thread`、`sandbox`、`app-server`、
`mcp-server`、`completion`），并将纯提示词转发给 `codesmith-tui`。

### 启动更新检查

默认情况下，TUI 会在启动时后台检查最新的稳定版 CodeSmith，并且仅当
有更新的版本可用且官方发布资源完整时，才显示一条简短的 toast 提示。

对于隔离网络（air-gapped）、企业代理或托管桌面环境，可以完全禁用
启动检查：

```toml
[update]
check_for_updates = false
```

要重定向启动检查，可将 `update_uri` 设置为返回 GitHub 兼容
latest-release JSON 的内部端点。只包含 `tag_name` 字段的最小镜像元数据
即可被接受；如果存在 `assets`，CodeSmith 会要求其与官方发布具有相同的
已上传资源集合，然后才显示 toast。

```toml
[update]
check_for_updates = true
update_uri = "https://internal.mirror.example/codesmith/releases/latest"
```

当 `update_uri` 未设置时，启动检查会优先使用发布镜像环境变量（如
`CODESMITH_RELEASE_BASE_URL`），然后才回退到官方 GitHub API 端点。
如果已配置的 `update_uri` 无法获取或解析，并且设置了发布镜像环境
变量，TUI 会回退到该镜像而不是启动失败。

## 配置档案（Profiles）

你可以在同一个文件中定义多个 profile：

```toml
api_key = "PERSONAL_KEY"
default_text_model = "deepseek-v4-pro"

[profiles.work]
api_key = "WORK_KEY"
base_url = "https://api.deepseek.com/beta"

[profiles.nvidia-nim]
provider = "nvidia-nim"
api_key = "NVIDIA_KEY"
base_url = "https://integrate.api.nvidia.com/v1"
default_text_model = "deepseek-ai/deepseek-v4-pro"

[profiles.fireworks]
provider = "fireworks"
default_text_model = "accounts/fireworks/models/deepseek-v4-pro"

[profiles.siliconflow]
provider = "siliconflow"
default_text_model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.siliconflow.providers.siliconflow]
base_url = "https://api.siliconflow.com/v1"

[profiles.openai-compatible]
provider = "openai"

[profiles.openai-compatible.providers.openai]
base_url = "https://openai-compatible.example/v4"
model = "glm-5"

[profiles.atlascloud]
provider = "atlascloud"

[profiles.atlascloud.providers.atlascloud]
base_url = "https://api.atlascloud.ai/v1"
model = "deepseek-ai/deepseek-v4-flash"

[profiles.sglang]
provider = "sglang"
base_url = "http://localhost:30000/v1"
default_text_model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.vllm]
provider = "vllm"
base_url = "http://localhost:8000/v1"
default_text_model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.ollama]
provider = "ollama"
base_url = "http://localhost:11434/v1"
default_text_model = "codesmith-coder:1.3b"
```

通过以下方式选择 profile：

- CLI：`codesmith --profile work`
- 环境变量：`CODESMITH_PROFILE=work`

如果选择了不存在的 profile，codesmith 会报错退出并列出可用的 profile。

## 环境变量

大多数运行时环境变量会覆盖配置值。API 密钥变量排在已保存配置和
密钥环凭证之后作为回退。

每个应用级变量都接受 `CODESMITH_*` 名称（首选），并以其旧版
`DEEPSEEK_*`（以及更早的 `CODEWHALE_*`）别名作为回退。当两种形式
都设置时，`CODESMITH_*` 的值优先：

- `CODESMITH_PROVIDER`（首选）/ `DEEPSEEK_PROVIDER`（旧版别名）—
  `deepseek|nvidia-nim|openai|atlascloud|wanjie-ark|openrouter|xiaomi-mimo|novita|fireworks|siliconflow|moonshot|sglang|vllm|ollama`
- `CODESMITH_MODEL`（首选）/ `DEEPSEEK_MODEL`（旧版别名）— 当前活跃 provider 的默认模型
- `CODESMITH_BASE_URL`（首选）/ `DEEPSEEK_BASE_URL`（旧版别名）— 当前活跃 provider 的 base URL

其余应用级变量（每个也响应其 `DEEPSEEK_*` 别名）：

- `CODESMITH_API_KEY`
- `CODESMITH_HTTP_HEADERS`（自定义模型请求头，逗号分隔的 `name=value` 对）
- `DEEPSEEK_DEFAULT_TEXT_MODEL`（`CODESMITH_MODEL` 的额外旧版别名）
- `NVIDIA_API_KEY` 或 `NVIDIA_NIM_API_KEY`（当 provider 为 `nvidia-nim` 时首选；回退到 `CODESMITH_API_KEY` / `DEEPSEEK_API_KEY`）
- `NVIDIA_NIM_BASE_URL`、`NIM_BASE_URL` 或 `NVIDIA_BASE_URL`
- `NVIDIA_NIM_MODEL`
- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`
- `OPENAI_MODEL`
- `ATLASCLOUD_API_KEY`
- `ATLASCLOUD_BASE_URL`
- `ATLASCLOUD_MODEL`
- `WANJIE_ARK_API_KEY`、`WANJIE_API_KEY` 或 `WANJIE_MAAS_API_KEY`
- `WANJIE_ARK_BASE_URL`、`WANJIE_BASE_URL` 或 `WANJIE_MAAS_BASE_URL`
- `WANJIE_ARK_MODEL`、`WANJIE_MODEL` 或 `WANJIE_MAAS_MODEL`
- `VOLCENGINE_API_KEY`、`VOLCENGINE_ARK_API_KEY` 或 `ARK_API_KEY`
- `VOLCENGINE_BASE_URL`、`VOLCENGINE_ARK_BASE_URL` 或 `ARK_BASE_URL`
- `VOLCENGINE_MODEL` 或 `VOLCENGINE_ARK_MODEL`
- `OPENROUTER_API_KEY`
- `OPENROUTER_BASE_URL`
- `XIAOMI_MIMO_API_KEY`、`XIAOMI_API_KEY` 或 `MIMO_API_KEY`
- `XIAOMI_MIMO_BASE_URL` 或 `MIMO_BASE_URL`
- `XIAOMI_MIMO_MODEL` 或 `MIMO_MODEL`
- `NOVITA_API_KEY`
- `NOVITA_BASE_URL`
- `FIREWORKS_API_KEY`
- `FIREWORKS_BASE_URL`
- `SILICONFLOW_API_KEY`
- `SILICONFLOW_BASE_URL`
- `SILICONFLOW_MODEL`
- `MOONSHOT_API_KEY` 或 `KIMI_API_KEY`
- `MOONSHOT_BASE_URL` 或 `KIMI_BASE_URL`
- `MOONSHOT_MODEL`、`KIMI_MODEL_NAME` 或 `KIMI_MODEL`
- `SGLANG_BASE_URL`
- `SGLANG_MODEL`
- `SGLANG_API_KEY`（可选；许多 localhost SGLang 服务器不需要认证）
- `VLLM_BASE_URL`
- `VLLM_MODEL`
- `VLLM_API_KEY`（可选；许多 localhost vLLM 服务器不需要认证）
- `OLLAMA_BASE_URL`
- `OLLAMA_MODEL`
- `OLLAMA_API_KEY`（可选；许多 localhost Ollama 服务器不需要认证）
- `CODESMITH_LOG_LEVEL` 或 `RUST_LOG`（`info`/`debug`/`trace` 启用轻量详细日志）
- `CODESMITH_SKILLS_DIR`
- `CODESMITH_MCP_CONFIG`
- `CODESMITH_NOTES_PATH`
- `CODESMITH_MEMORY`（`1|on|true|yes|y|enabled` 开启用户记忆）
- `CODESMITH_MEMORY_PATH`
- `CODESMITH_ALLOW_SHELL`（`1`/`true` 启用）
- `CODESMITH_APPROVAL_POLICY`（`on-request|untrusted|never`）
- `CODESMITH_SANDBOX_MODE`（`read-only|workspace-write|danger-full-access|external-sandbox`）
- `CODESMITH_MANAGED_CONFIG_PATH`
- `CODESMITH_REQUIREMENTS_PATH`
- `CODESMITH_MAX_SUBAGENTS`（钳制在 `1..=20`）
- `CODESMITH_TASKS_DIR`（运行时任务队列/产物存储，默认为
  `~/.codesmith/tasks`，当仅存在旧版目录时回退到
  `~/.deepseek/tasks`）
- `CODESMITH_HOME`（覆盖基础数据目录；默认为 `~/.codesmith`）。
  如果你之前导出过 `DEEPSEEK_HOME`，请将其改名为 `CODESMITH_HOME`；
  新的 CodeSmith 状态路径不再使用旧的环境变量。
- `CODESMITH_RELEASE_BASE_URL`（发布资源镜像，供 `codesmith update`
  以及 `[update].update_uri` 未设置时的 TUI 启动更新检查使用，
  或在该配置的 URI 无法获取时作为回退）
- `CODESMITH_AUTOMATIONS_DIR`（覆盖 automations 存储目录；默认使用
  `~/.codesmith/automations`，当仅存在旧版目录时回退到
  `~/.deepseek/automations`）
- `CODESMITH_CAPACITY_ENABLED`
- `CODESMITH_CAPACITY_LOW_RISK_MAX`
- `CODESMITH_CAPACITY_MEDIUM_RISK_MAX`
- `CODESMITH_CAPACITY_SEVERE_MIN_SLACK`
- `CODESMITH_CAPACITY_SEVERE_VIOLATION_RATIO`
- `CODESMITH_CAPACITY_REFRESH_COOLDOWN_TURNS`
- `CODESMITH_CAPACITY_REPLAN_COOLDOWN_TURNS`
- `CODESMITH_CAPACITY_MAX_REPLAY_PER_TURN`
- `CODESMITH_CAPACITY_MIN_TURNS_BEFORE_GUARDRAIL`
- `CODESMITH_CAPACITY_PROFILE_WINDOW`
- `CODESMITH_CAPACITY_PRIOR_CHAT`
- `CODESMITH_CAPACITY_PRIOR_REASONER`
- `CODESMITH_CAPACITY_PRIOR_V4_PRO`
- `CODESMITH_CAPACITY_PRIOR_V4_FLASH`
- `CODESMITH_CAPACITY_PRIOR_FALLBACK`
- `NO_ANIMATIONS`（`1|true|yes|on` 会在启动时强制 `low_motion = true` 和
  `fancy_animations = false`，无论已保存的设置如何；参见
  [`docs/ACCESSIBILITY.md`](./ACCESSIBILITY.md)）。
- `SSL_CERT_FILE` — 企业代理 / TLS 检测 MITM 用户将其指向一个
  PEM 捆绑包（或单个 DER 证书），这些证书会被添加到平台系统信任库
  旁边。失败会记录警告并继续——现有的系统根证书仍然适用。

### 指令来源（`instructions = [...]`，#454）

添加一组额外的系统提示词来源，它们会按声明顺序与自动加载的
`AGENTS.md` 拼接在一起：

```toml
instructions = [
    "./AGENTS.md",
    "~/.codesmith/global.md",
    "~/team/agents-shared.md",
]
```

规则：

- 路径会经过 `expand_path` 处理，因此 `~` 和环境变量都可用。
- 每个文件上限为 100 KiB；超限的文件会被截断并加上 `[…elided]`
  标记，而不是被跳过。
- 缺失的文件会被跳过并记录一条 tracing 警告，因此过期的条目
  不会导致启动失败。
- 项目配置（`<workspace>/.codesmith/config.toml`，或旧版
  `<workspace>/.deepseek/config.toml`）会整体**替换**用户数组而不是
  合并。如果两者都想要，请在项目数组中列出 `~/global.md`。在项目
  中设置 `instructions = []` 可为该仓库清空用户列表。

### 系统提示词自定义

系统提示词由可组合的层装配而成：工具分类法 → CodeSmith 宪法
（`base.md`）→ 个性叠加层 → 模式增量 → 审批策略，外加运行时小节
（项目上下文、skills、环境、instructions、memory）。其中三层可以在
`config.toml` 中由用户调整，这里按从最安全到最具侵入性的顺序排列：

```toml
# Voice and tone overlay: "calm" (default) or "playful".
# Presentation-only — it changes how the agent speaks, never
# what it does. Case-insensitive; invalid values fail startup
# validation. Project config may override it.
personality = "calm"

# Additional prompt sections appended after the assembled
# prompt. Entries are file paths (~ and env vars expanded),
# rendered in declared order.
append_system_prompt = ["~/.codesmith/extra-prompt.md"]

# Full system prompt override. Inline `system_prompt` wins over
# `system_prompt_file`. Replaces the built-in prompt entirely —
# append sections are still rendered after it.
# system_prompt = "You are a release engineering assistant..."
# system_prompt_file = "~/.codesmith/prompt.md"
```

优先使用 `instructions = [...]` 和 `append_system_prompt` 而不是完全
覆盖：它们能保持宪法（真实性、验证纪律、工具使用强制要求）、
工具分类法以及模式/审批层不变。设置 `system_prompt` /
`system_prompt_file` 会丢弃所有这些，模型因此失去根基——只在你确实
需要从头构建一个角色时才使用。在 TUI 中运行 `/system`（或
`/xitong`）可以查看最终装配的提示词并确认你的层已生效。

exec 模式 CLI 也接受一次性等价参数：
`--system-prompt` / `--system-prompt-file` 和
`--append-system-prompt` / `--append-system-prompt-file`。CLI 内联值
优先于文件值，文件值优先于配置。

### `/hooks` 列表

在 TUI 中运行 `/hooks`（或 `/hooks list`）可以查看每个已配置的
生命周期钩子（按事件分组），包括每个钩子的名称、命令预览、超时和
条件。`[hooks].enabled` 标志的状态会显示在顶部，因此当钩子被全局
禁用时一目了然。钩子在 `[[hooks.hooks]]` 条目下配置——完整的
schema、事件列表、条件和 I/O 契约请参阅
[docs/HOOKS.md](HOOKS.md)。

### 可变的 `message_submit` 钩子

`message_submit` 钩子在提交的消息被加入历史或发送给模型之前运行。
与仅观察的生命周期钩子不同，非后台的 `message_submit` 钩子可以
替换或阻止提交的文本。

```toml
[[hooks.hooks]]
event = "message_submit"
command = "~/.codesmith/hooks/inject-context.sh"
timeout_secs = 2
continue_on_error = true
```

钩子在 stdin 上接收 JSON：

```json
{
  "event": "message_submit",
  "text": "original user text",
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "total_tokens": 1234
}
```

如果钩子以退出码 `0` 退出并输出带有非空字符串 `text` 字段的 JSON，
该值会替换提交的文本：

```json
{ "text": "replacement user text" }
```

以 `0` 退出但 stdout 为空，或 stdout JSON 中没有 `text`，则保持当前
文本不变。JSON 的 `text` 字段不得为空；`{"text":""}` 会被视为无效
stdout 并被忽略。以 `2` 退出会在该轮开始前阻止提交；`reason` 字段、
stderr 或 stdout 可以提供在 TUI 中显示的状态消息。其他非零退出遵循
钩子的 `continue_on_error` 设置。当 `continue_on_error = true` 允许
提交继续时，超时和启动失败也会作为短暂的 TUI 状态消息显示。

多个 `message_submit` 钩子按配置顺序运行，每个钩子接收上一个钩子
产生的文本。标记为 `background = true` 的钩子仅用于观察，不能转换或
阻止消息。现有环境变量仍然可用。`shell_env` 钩子保持其现有的
`KEY=VALUE` stdout 契约；JSON stdout 契约仅适用于 `message_submit`。

`session_id` 字段（以及 `DEEPSEEK_SESSION_ID` 环境变量）携带的是
**临时性**的按构建遥测 id——它在每次会话启动时都会变化，不会跨
重启关联。要跨重启关联（resume、capacity 记忆连续性），请使用
`DEEPSEEK_THREAD_ID`，它携带持久线程 id，并且在结构化钩子载荷中也
以 `thread_id` 字段提供。

### 输入框暂存（`/stash`，Ctrl+S）

在输入框中按 **Ctrl+S** 可将当前草稿存入
`~/.codesmith/composer_stash.jsonl`。`/stash list` 显示已暂存的草稿
及其单行预览和时间戳；`/stash pop` 恢复最近暂存的草稿（LIFO）；
`/stash clear` 清空该文件。上限为 200 条；多行草稿可完整往返。

## 设置文件（持久化 UI 偏好）

codesmith 还将用户偏好存储在：

- `~/.config/deepseek/settings.toml`

值得注意的设置包括 `auto_compact`（默认 `false`），它仅在接近当前
模型上限时才启用替换式摘要。默认的 V4 路径会保留稳定的消息前缀以
复用缓存；只有在明确想要自动替换压缩时才使用手动 `/compact` 或启用
`auto_compact`。你可以在 TUI 中使用 `/settings` 和 `/config`
（交互式编辑器）查看或更新这些设置。

常用设置键：

- `theme`（`system`、`dark`、`light`、`grayscale`、`catppuccin-mocha`、
  `tokyo-night`、`dracula`、`gruvbox-dark`；默认 `system`）：`system`
  跟随终端背景检测，`dark`/`light` 使用 DeepSeek 调色板，`grayscale`
  是极简的黑/白主题，具名社区预设则应用于整个 TUI。接受诸如 `whale`、
  `mono`、`black-white`、`tokyonight` 和 `gruvbox` 之类的别名。
- `auto_compact`（开/关，默认关）
- `paste_burst_detection`（开/关，默认开）：为不发出括号粘贴
  （bracketed-paste）事件的终端提供回退的快速按键粘贴检测。这与
  终端的括号粘贴模式无关。
- `mention_menu_limit`（整数，默认 `128`）：输入框渲染可见窗口之前
  保留的 `@`-提及弹出候选的最大数量。可见行数仍取决于终端高度。
- `mention_walk_depth`（整数，默认 `6`）：`@`-提及补全遍历的最大
  工作区深度。在深度嵌套的工作区中可设为 `0` 以获得无限深度；在超大
  仓库中除非必要否则保持默认值。
- `show_thinking`（开/关）
- `is_simple`（开/关，默认关）：以最大压缩的"简单"（caveman）会话
  风格作答——短句，没有填充词或客套话——同时代码、命令和错误消息
  保持字节级精确，并保留用户的语言。安全警告和破坏性操作通知会自动
  切换回完全清晰的表达。通过 `/config is_simple on` 切换；从下一轮
  开始生效。
- `show_tool_details`（开/关）
- `locale`（`auto`、`en`、`ja`、`zh-Hans`、`pt-BR`、`es-419`、`vi`；默认 `auto`）：
  UI 界面语言。`auto` 依次检查 `LC_ALL`、`LC_MESSAGES`、`LANG`；
  不支持或缺失的语言回退为英语。运行时还会把解析出的语言写入系统
  提示词，作为 V4 推理和回复在最新用户消息有歧义时的回退自然语言。
  明确的用户语言仍然优先；即使解析出的语言是英语，中文对话也应当
  产生中文 `reasoning_content` 和中文最终回复。
- `background_color`（`#RRGGBB`、`RRGGBB` 或 `default`）：可选的
  主 TUI 背景色，应用于根界面、页眉、会话记录和页脚表面，同时保持
  面板对比度。
- `cost_currency`（`usd`、`cny`；默认 `usd`）：页脚、上下文面板、
  `/cost`、`/tokens` 以及长轮次通知摘要使用的货币。别名 `rmb` 和
  `yuan` 会归一化为 `cny`。
- `default_mode`（agent、plan、yolo；接受旧版 `normal` 并归一化为 `agent`）
- `sidebar_focus`（`auto`、`work`、`tasks`、`agents`、`context`、
  `hidden`；默认 `auto`）：选择右侧边栏的焦点。`auto` 依次优先
  Work、Tasks、Agents，然后是可选的 Context，并将 Work 用作唯一的
  安静空状态。`hidden` 完全禁用右侧边栏，使原生终端选区无法从会话
  记录跨入边栏边框。接受旧版 `plan` 和 `todos` 值并归一化为 `work`。
- `max_history`（已提交输入历史条目的数量；被清除的草稿也会在本地
  保留，用于输入框历史搜索）
- `default_model`（模型名称覆盖）

UI 中只有 `agent`、`plan` 和 `yolo` 是可见模式。使用 `/mode` 在它们
之间切换。为兼容起见，带有 `default_mode = "normal"` 的旧设置文件仍会
加载为 `agent`。

本地化范围记录在 [LOCALIZATION.md](LOCALIZATION.md) 中。v0.7.6 核心
包仅覆盖高可见度的 TUI 界面；provider/工具 schema、个性提示词和完整
文档保持英语，除非以后显式翻译。

可读性语义：

- 选区在会话记录、输入框菜单和模态框中使用统一样式。
- 页脚提示使用专门的语义角色（`FOOTER_HINT`），使提示文本在各主题下
  保持可读。
- 页脚包含一个紧凑的 `coherence` 标签，描述当前会话此刻的稳定性和
  专注度。可能的状态有 `healthy`、`crowded`、`refreshing`、
  `verifying` 和 `resetting`；它们派生自容量和压缩事件，不会在普通
  UI 中暴露内部公式。

### Token 数量与驱动项

DeepSeek V4 前缀缓存使得 token 标签很重要。这些数量是分开维护的：

| 数量 | 含义 | 允许驱动 |
|---|---|---|
| 活跃请求输入估算 | 对下一次请求的实时系统提示词和会话记录载荷的保守估算。 | 页眉/页脚上下文百分比、硬循环触发器、可选的 Flash seam 触发器，以及紧急溢出预检。 |
| 预留响应余量 | 内部轮次预算加上安全余量。v0.8.16 将普通轮次的预留输出 token 保持为 `262144`，并为上下文窗口检查额外加上 `1024` 个安全 token，即使 V4 能力元数据报告的官方最大输出为 `384000`。 | 仅用于硬循环和紧急溢出预算检查。 |
| 累计 API 用量 | 各次已完成 API 调用的 provider 报告输入与输出 token 之和；多工具轮次可能会多次计入同一稳定前缀。 | 仅用于会话用量和近似成本遥测。 |
| 提示词缓存命中/未命中 | 最近一次调用的 provider 缓存遥测（如果可用）。 | 仅用于缓存命中显示和成本估算；绝不用于压缩、seam 或循环触发。 |
| 上下文百分比 | 活跃请求输入估算除以模型上下文窗口。 | 仅用于显示；它反映了上下文防护所使用的活跃输入基准。 |
| 成本估算 | 基于 provider 用量和已配置 DeepSeek 费率的近似花费。 | 仅用于显示。 |

对于默认的 V4 路径，当活跃输入达到已配置的循环阈值（`768000`）与
模型窗口减去预留响应余量两者中较小的那个时，会触发硬循环。替换压缩
仍为可选（默认 `auto_compact = false`），Flash seam 管理器仍为可选
（`[context].enabled = false`），容量控制器除非配置否则保持禁用。

### 命令迁移说明

如果你从旧版本升级：

- 旧：`/codesmith`
  新：`/links`（别名：`/dashboard`、`/api`）
- 旧：`/set model deepseek-reasoner`
  新：使用 `/config` 并将 `model` 行编辑为 `deepseek-v4-pro` 或 `deepseek-v4-flash`
- 旧：可见的 `Normal` 模式或 `default_mode = "normal"`
  新：使用 `Agent` / `default_mode = "agent"`；旧版 `normal` 仍映射为 `agent`
- 旧：在斜杠 UX/帮助中发现 `/set`
  新：使用 `/config` 进行编辑，使用 `/settings` 进行只读查看

## 配置键参考

### 核心键（供 TUI/引擎使用）

- `provider`（字符串，可选）：`deepseek`（默认）、`nvidia-nim`、`openai`、`atlascloud`、`wanjie-ark`、`openrouter`、`xiaomi-mimo`、`novita`、`fireworks`、`siliconflow`、`moonshot`、`sglang`、`vllm` 或 `ollama`。旧版 `deepseek-cn` 配置仍被接受，作为 `deepseek` 的别名；DeepSeek 在全球使用相同的官方主机 [`https://api.deepseek.com`](https://api-docs.deepseek.com/)。`nvidia-nim` 通过 `https://integrate.api.nvidia.com/v1` 指向 NVIDIA NIM 托管的 DeepSeek 端点；`openai` 指向通用 OpenAI 兼容端点，默认为 `https://api.openai.com/v1`；`atlascloud` 指向 AtlasCloud 的 OpenAI 兼容端点 `https://api.atlascloud.ai/v1`；`wanjie-ark` 指向 Wanjie Ark 的 OpenAI 兼容端点 `https://maas-openapi.wanjiedata.com/api/v1`；`openrouter` 指向 `https://openrouter.ai/api/v1`；`xiaomi-mimo` 指向小米 MiMo 的 OpenAI 兼容端点 `https://api.xiaomimimo.com/v1`；`novita` 指向 `https://api.novita.ai/v1`；`fireworks` 指向 `https://api.fireworks.ai/inference/v1`；`siliconflow` 指向 SiliconFlow，默认为 `https://api.siliconflow.com/v1`；`moonshot` 指向 Moonshot/Kimi，默认为 `https://api.moonshot.ai/v1`；`sglang` 指向自托管的 OpenAI 兼容端点，默认为 `http://localhost:30000/v1`；`vllm` 指向自托管的 vLLM OpenAI 兼容端点，默认为 `http://localhost:8000/v1`；`ollama` 指向 Ollama 的 OpenAI 兼容端点，默认为 `http://localhost:11434/v1`。
- `api_key`（字符串，托管 provider 必填）：对 DeepSeek/托管 provider 必须非空（或设置该 provider 的 API 密钥环境变量）。自托管的 SGLang、vLLM 和 Ollama 可以省略。
- `base_url`（字符串，可选）：对 DeepSeek 的 OpenAI 兼容 Chat Completions API 默认为 `https://api.deepseek.com/beta`，包括旧版 `provider = "deepseek-cn"` 配置。其他默认值：`nvidia-nim` 为 `https://integrate.api.nvidia.com/v1`，`openai` 为 `https://api.openai.com/v1`，`atlascloud` 为 `https://api.atlascloud.ai/v1`，`wanjie-ark` 为 `https://maas-openapi.wanjiedata.com/api/v1`，`openrouter` 为 `https://openrouter.ai/api/v1`，`xiaomi-mimo` 为 `https://api.xiaomimimo.com/v1`，`novita` 为 `https://api.novita.ai/v1`，`fireworks` 为 `https://api.fireworks.ai/inference/v1`，`siliconflow` 为 `https://api.siliconflow.com/v1`，`moonshot` 为 `https://api.moonshot.ai/v1`，`sglang` 为 `http://localhost:30000/v1`，`vllm` 为 `http://localhost:8000/v1`，`ollama` 为 `http://localhost:11434/v1`。显式设置 `https://api.deepseek.com` 或 `https://api.deepseek.com/v1` 可退出 DeepSeek beta 功能。
- `default_text_model`（字符串，可选）：DeepSeek 和通用 OpenAI 兼容端点默认为 `deepseek-v4-pro`，NVIDIA NIM 为 `deepseek-ai/deepseek-v4-pro`，AtlasCloud 为 `deepseek-ai/deepseek-v4-flash`，Wanjie Ark 为 `deepseek-reasoner`，OpenRouter 和 Novita 为 `deepseek/deepseek-v4-pro`，小米 MiMo 为 `mimo-v2.5-pro`，Fireworks 为 `accounts/fireworks/models/deepseek-v4-pro`，SiliconFlow 为 `deepseek-ai/DeepSeek-V4-Pro`，Moonshot 为 `kimi-k2.6`，SGLang/vLLM 为 `deepseek-ai/DeepSeek-V4-Pro`，Ollama 为 `deepseek-coder:1.3b`。当前公开的 DeepSeek ID 是 `deepseek-v4-pro` 和 `deepseek-v4-flash`，两者都具有 1M 上下文窗口、384K 最大输出，并且默认启用思考模式。旧版 `deepseek-chat` 和 `deepseek-reasoner` 仍作为 `deepseek-v4-flash` 的兼容别名解析（移除已列入计划，但未承诺具体日期），但 SiliconFlow 除外：它将 `deepseek-reasoner` 和 `deepseek-r1` 映射到其 Pro 模型，而 `deepseek-chat` 和 `deepseek-v3` 映射到 Flash。Provider 专属映射会在支持的情况下将 `deepseek-v4-pro` / `deepseek-v4-flash` 转换为各 provider 的模型 ID。OpenRouter 还识别较新的大型 ID，如 `arcee-ai/trinity-large-thinking`、`qwen/qwen3.7-max`、`xiaomi/mimo-v2.5-pro`、`qwen/qwen3.6-35b-a3b`、`google/gemma-4-31b-it` 和 `moonshotai/kimi-k2.6`。通用 `openai`、`atlascloud`、`wanjie-ark`、`xiaomi-mimo` 以及 Ollama 的模型 ID 会原样透传。带有自定义 `base_url` 的 OpenRouter 和 SiliconFlow provider 配置也会保留显式模型值，这使得 OpenAI 兼容网关可以接受裸模型 ID。使用 `/models` 或 `codesmith models` 从你配置的端点发现可用 ID。`CODESMITH_MODEL` 可为单个进程覆盖此项；`DEEPSEEK_MODEL` 是旧版别名。
- `reasoning_effort`（字符串，可选）：`off`、`low`、`medium`、`high` 或 `max`；默认为已配置的 UI 档位。DeepSeek 平台通过顶层 `thinking` / `reasoning_effort` 字段接收。NVIDIA NIM 通过 `chat_template_kwargs` 接收等价设置。
- `allow_shell`（布尔，可选）：默认为 `true`（受沙箱保护）。
- `telemetry`（布尔，可选，默认 `false`）：可选择加入的**仅本地**遥测。当为 `true` 时，容量决策分析事件会在通过工作区信任边界后写入 `~/.codesmith/telemetry/events.jsonl`。绝不联网；接收器在信任前在内存中排队，仅在信任后才附加（写入），因此在获得同意之前不会有任何工作区控制的数据落盘。事件携带临时的按会话 id，而不是持久线程 id。
- `approval_policy`（字符串，可选）：`on-request`、`untrusted` 或 `never`。在 `/config` 中运行时编辑 `approval_mode` 时也接受 `on-request` 和 `untrusted` 别名。
- `sandbox_mode`（字符串，可选）：`read-only`、`workspace-write`、`danger-full-access`、`external-sandbox`。
  各平台的支持并不相同。macOS 使用 Seatbelt 进行策略执行。Linux 支持
  通过辅助程序围绕 Landlock 或可选的 bubblewrap（`prefer_bwrap = true`）
  把关。Windows 目前没有 OS 沙箱；计划中的 Windows 辅助程序契约仅从
  进程树隔离开始，在实现之前不得将其描述为只读文件系统隔离、工作区
  写入强制、网络阻断、注册表隔离或 AppContainer 隔离。高级沙箱控件
  可在 `[sandbox]` 下设置：`enabled`、`fail_if_unavailable`、
  `enabled_platforms`、`excluded_commands`、`auto_allow_bash_if_sandboxed`、
  `prefer_bwrap`，以及 `[sandbox.filesystem]` 和 `[sandbox.network]` 表。
  Shell 结果会同时报告请求的和实际生效的沙箱元数据，使回退行为
  明确可见。
- `managed_config_path`（字符串，可选）：在用户/环境配置之后加载的托管配置文件。
- `requirements_path`（字符串，可选）：用于强制限定允许的审批/沙箱值的 requirements 文件。
- `max_subagents`（整数，可选）：默认为 `10`，并钳制在 `1..=20`。
- `subagents.*`（可选）：为 `agent_open` 及相关持久子代理会话设置按
  角色/类型的模型默认值。显式工具 `model` 值优先，其次是角色/类型
  覆盖，再次是父运行时模型。支持的便捷键有 `default_model`、
  `worker_model`、`explorer_model`、`awaiter_model`、`review_model`、
  `custom_model`、`max_concurrent`、`api_timeout_secs` 和
  `inherit_full_registry`。`[subagents] max_concurrent` 值会覆盖顶层
  `max_subagents`，同样钳制在 `1..=20`；`[subagents] api_timeout_secs`
  控制子代理模型调用的每步 API 超时，钳制在 `1..=1800`，为 `0` 或
  未设置时保持旧版 120 秒默认值。
  `[subagents] inherit_full_registry`（布尔，可选，默认 `false`）
  控制子权限收窄（`restrictToSubset`）：当为 `false` 时，子代理的
  工具面是其父代理有效工具的子集——子代理永远无法调用父代理没有的
  工具，且被收窄的父代理的孙代理继承收窄后的集合，而不是重新扩展到
  完整工具面。顶层 General 子代理仍获得完整工具面（其父代理不受
  限制），因此递归生成得以保留。设为 `true` 可恢复旧版 v0.6.6 的
  行为：无论父代理的有效集合如何，每个子代理都继承完整的代理
  工具面。`[subagents.models]` 接受小写的角色或类型键，如 `worker`、
  `explorer`、`general`、`explore`、`plan` 和 `review`。值必须归一化
  为受支持的 DeepSeek 模型 id，代理才能被生成。
- `skills_dir`（字符串，可选）：默认为 `~/.codesmith/skills`（每个
  skill 是一个包含 `SKILL.md` 的目录）。如果存在工作区本地的
  `.agents/skills` 或 `./skills` 则优先使用；运行时还会发现全局的
  agentskills.io 兼容目录 `~/.agents/skills` 以及更广泛的 Claude 生态
  目录 `~/.claude/skills`。首次启动会安装带版本号的内置 skills，
  覆盖常见工作流，包括 skill 创建、委派、MCP/插件脚手架、文档、
  演示文稿、电子表格、PDF 以及飞书/Lark。
- `mcp_config_path`（字符串，可选）：默认为 `~/.codesmith/mcp.json`，
  当 CodeSmith 路径不存在时回退到旧版 `~/.deepseek/mcp.json`。它显示
  在 `/config` 中，可以从 TUI 修改。新路径会被 `/mcp` 立即使用，但
  重建模型可见的 MCP 工具池需要重启 TUI。
- `notes_path`（字符串，可选）：默认为 `~/.codesmith/notes.txt`，
  当 CodeSmith 路径不存在时回退到旧版 `~/.deepseek/notes.txt`，
  由模型可见的 `note` 工具使用。
- `personality`（字符串，可选）：`calm`（默认）或 `playful`——
  系统提示词中的语气与风格叠加层。不区分大小写；其他值会导致启动
  校验失败。仅影响表达：它不能改变代理做什么，只能改变它怎么说。
  参见[系统提示词自定义](#system-prompt-customization)。
- `append_system_prompt`（字符串数组，可选）：文件路径，在装配好的
  系统提示词之后按声明顺序渲染为额外的提示词小节。`~` 和环境变量
  会被展开；无法读取的文件会被跳过并给出警告。
- `system_prompt` / `system_prompt_file`（字符串，可选）：完整系统
  提示词覆盖——内联文本或文件路径，两者都设置时内联优先。替换内置
  提示词（宪法、工具分类法、模式/审批层）；`append_system_prompt`
  小节仍会在其后渲染。除非需要从头构建角色，否则优先使用
  `instructions` + `append_system_prompt`。参见
  [系统提示词自定义](#system-prompt-customization)。
- `[memory].enabled`（布尔，可选）：默认为 `false`。当为 `true` 时，
  TUI 将用户记忆文件加载到 `<user_memory>` 提示词块中，在输入框中
  启用 `# foo` 快速记录，显示 `/memory` 斜杠命令，并注册 `remember`
  工具。同样的开关可通过 `DEEPSEEK_MEMORY=on` 使用。
- `memory_path`（字符串，可选）：默认为 `~/.codesmith/memory.md`，
  当 CodeSmith 路径不存在时回退到旧版 `~/.deepseek/memory.md`。
  启用后由用户记忆功能使用——完整功能面请参阅
  [`MEMORY.md`](MEMORY.md)（`# foo` 输入框前缀、`/memory` 斜杠命令、
  `remember` 工具、可选开关）。
- `snapshots.*`（可选）：用于文件回滚的 side-git 工作区快照：
  - `[snapshots].enabled`（布尔，默认 `true`）
  - `[snapshots].max_age_days`（整数，默认 `7`）
  - 快照位于
    `~/.codesmith/snapshots/<project_hash>/<worktree_hash>/.git`，
    当仅存在旧版状态时回退到 `~/.deepseek/snapshots/...`，并且绝不
    使用工作区自身的 `.git` 目录
- `context.*`（可选）：只增不减的 Fin seam 管理器，目前为可选。
  Fin 是关闭思考的快速 `deepseek-v4-flash` 路径，用于协调工作，
  如路由、摘要和上下文维护。阈值使用活跃请求输入估算，而不是生命
  周期累计 API 用量：
  - `[context].enabled`（布尔，默认 `false`）
  - `[context].verbatim_window_turns`（整数，默认 `16`）
  - `[context].l1_threshold`（整数，默认 `192000`）
  - `[context].l2_threshold`（整数，默认 `384000`）
  - `[context].l3_threshold`（整数，默认 `576000`）
  - `[context].cycle_threshold`（整数，默认 `768000`）
  - `[context].seam_model`（字符串，默认：已配置的 `[utility_model]`
    模型 id，否则为 `deepseek-v4-flash`）
- `retry.*`（可选）：API 请求的重试/退避设置：
  - `[retry].enabled`（布尔，默认 `true`）
  - `[retry].max_retries`（整数，默认 `3`）
  - `[retry].initial_delay`（浮点秒数，默认 `1.0`）
  - `[retry].max_delay`（浮点秒数，默认 `60.0`）
  - `[retry].exponential_base`（浮点，默认 `2.0`）
- `capacity.*`（可选）：运行时上下文容量控制器。这是可选的，因为其
  主动干预可以改写实时会话记录。
  - `[capacity].enabled`（布尔，默认 `false`）
  - `[capacity].low_risk_max`（浮点，默认 `0.50`）
  - `[capacity].medium_risk_max`（浮点，默认 `0.62`）
  - `[capacity].severe_min_slack`（浮点，默认 `-0.25`）
  - `[capacity].severe_violation_ratio`（浮点，默认 `0.40`）
  - `[capacity].refresh_cooldown_turns`（整数，默认 `6`）
  - `[capacity].replan_cooldown_turns`（整数，默认 `5`）
  - `[capacity].max_replay_per_turn`（整数，默认 `1`）
  - `[capacity].min_turns_before_guardrail`（整数，默认 `4`）
  - `[capacity].profile_window`（整数，默认 `8`）
  - `[capacity].deepseek_v3_2_chat_prior`（浮点，默认 `3.9`）
  - `[capacity].deepseek_v3_2_reasoner_prior`（浮点，默认 `4.1`）
  - `[capacity].deepseek_v4_pro_prior`（浮点，默认 `3.5`）
  - `[capacity].deepseek_v4_flash_prior`（浮点，默认 `4.2`）
  - `[capacity].fallback_default_prior`（浮点，默认 `3.8`）
- `[notifications].method`（字符串，可选）：`auto`、`osc9`、`bel` 或
  `off`。默认为 `auto`。TUI 会在耗时达到 `threshold_secs` 的已完成
  （成功）轮次上触发；失败和取消的轮次保持静默。`auto` 对
  `iTerm.app`、`Ghostty` 和 `WezTerm`（通过 `$TERM_PROGRAM` 检测）
  解析为 `osc9`。否则回退为 macOS / Linux 上的 `bel` 和 Windows 上的
  `off`（在 Windows 上 BEL 会映射为系统错误提示音——完整原因请参阅
  [通知](#notifications)小节，#583）。
- `[notifications].threshold_secs`（整数，可选）：默认为 `30`。
  只有耗时达到或超过该值的已完成轮次才会触发通知。
- `[notifications].include_summary`（布尔，可选）：默认为 `false`。
  当为 `true` 时，通知正文包含耗时以及按配置显示货币计的该轮成本。
- `tui.alternate_screen`（字符串，可选）：`auto`、`always` 或 `never`。
  保留此项是为了配置兼容性，但交互式会话现在始终使用 TUI 拥有的备用
  屏幕，因此宿主终端的回滚缓冲无法劫持视口。
- `tui.mouse_capture`（布尔，可选，在非 Windows 终端上以及备用屏幕
  激活时的 Windows Terminal/ConEmu/Cmder 上默认为 `true`；在旧版
  Windows 控制台和 JetBrains JediTerm 内——PyCharm/IDEA/CLion 等——
  默认为 `false`，因为在这些环境中鼠标事件转义序列会以乱码文本的
  形式泄漏到输入流中，参见 #878 / #898）：启用内部鼠标滚动、会话
  记录选择、右键上下文操作以及会话记录滚动条拖动。TUI 拥有的拖动
  选择只复制会话记录文本，移除段落中因视觉换行产生的换行符，并使
  选区限定在会话记录窗格内。将其设为 `false` 或使用
  `--no-mouse-capture` 运行可获得原生终端选择；设为 `true` 或使用
  `--mouse-capture` 运行可在任何默认关闭的地方选择开启。在原生终端
  选择下，尤其是在旧版 Windows 控制台上或禁用鼠标捕获时，选区可能
  会跨过右侧边栏并包含视觉换行，因为此时选择由终端而非 TUI 拥有。
- `tui.terminal_probe_timeout_ms`（整数，可选，默认 `500`）：启动时
  终端模式探测的超时时间，单位毫秒。值被钳制在 `100..=5000`；超时
  会发出警告并中止启动，而不是无限挂起。
- `tui.osc8_links`（布尔，可选，默认 `true`）：在会话记录输出中的
  URL 周围发出 OSC 8 转义序列，使支持它的终端（iTerm2、
  Terminal.app 13+、Ghostty、Kitty、WezTerm、Alacritty、较新的
  gnome-terminal/konsole）将其渲染为 Cmd+点击的超链接。不支持
  OSC 8 的终端渲染纯 URL 并忽略该转义。对错误渲染该序列的终端设为
  `false`；选择/剪贴板输出总是会剥离这些转义。
- `hooks`（可选）：生命周期钩子配置（参见 `config.example.toml`）。
- `features.*`（可选）：功能开关覆盖（见下文）。

### 工作区笔记

`/note` 管理当前工作区 `.deepseek/notes.md` 中的一个简单笔记文件。
现有的 `/note <text>` 用法仍然会追加笔记。管理形式如下：

| 命令 | 操作 |
|---|---|
| `/note <text>` | 追加笔记（旧版简写） |
| `/note add <text>` | 显式追加笔记 |
| `/note list` | 以临时的从 1 开始的编号列出笔记 |
| `/note show <n>` | 显示编号 `n` 的完整笔记 |
| `/note edit <n> <text>` | 用新文本替换笔记 `n` |
| `/note remove <n>` | 删除笔记 `n`；`rm` 和 `delete` 是别名 |
| `/note clear` | 清空工作区笔记文件 |
| `/note path` | 显示解析后的工作区笔记路径 |

`/note list` 显示的编号不存储在文件中；每次读取笔记时都会根据当前
顺序推导。这使文件格式与现有的以 `---` 分隔的笔记保持兼容。

### 用户记忆

用户记忆由一个顶层路径设置和一个开关表组成（该功能默认开启）：

```toml
memory_path = "~/.codesmith/memory.md"

[memory]
enabled = true
```

注意：

- `memory_path` 与 `notes_path` 和 `skills_dir` 一样位于顶层；
  它不嵌套在 `[memory]` 之下。
- `CODESMITH_MEMORY_PATH` 从环境中覆盖文件路径。
- `DEEPSEEK_MEMORY=on`（也接受 `1`、`true`、`yes`、`y` 或 `enabled`）
  无需编辑 `config.toml` 即可开启该功能；
  `DEEPSEEK_DISABLE_AUTO_MEMORY=1`（或 `enabled = false`）可退出。
- bare/simple 会话和无持久存储的远程会话会自动禁用记忆。
- 禁用时该功能不起作用：不注入任何文件，`# foo` 按普通消息提交处理，
  模型看不到 `remember` 工具。
- 示例和完整的 `/memory` 命令面请参阅 [`MEMORY.md`](MEMORY.md)。

### 通知

当一轮**成功完成**且耗时超过阈值时，TUI 可以发出桌面通知（OSC 9
转义或纯 BEL），这样你可以在长任务运行时切换到其他窗口。失败或
取消的轮次有意保持静默——该通知是"你的任务已就绪"的提示，而不是
通用的提醒。配置位于 `[notifications]` 之下：

```toml
[notifications]
method          = "auto"  # auto | osc9 | bel | off
threshold_secs  = 30      # only notify when the turn took >= this many seconds
include_summary = false   # include elapsed time + cost in the notification body
```

各方法的语义：

- `auto`（默认）——对 `iTerm.app`、`Ghostty` 和 `WezTerm`（通过
  `$TERM_PROGRAM` 检测）选择 `osc9`。在 macOS 和 Linux 上回退为
  `bel`。**在 Windows 上回退为 `off`** 而不是 `bel`，因为 Windows
  音频栈会把 `\x07` 映射为 `SystemAsterisk` / `MB_OK` 提示音——与
  应用错误弹窗使用的声音相同，因此成功轮次的通知听起来会像错误
  （#583）。
- `osc9` —— 发出 `\x1b]9;<msg>\x07`。在 tmux 内该序列会被包装在
  DCS passthrough 中，以便到达外层终端。
- `bel` —— 发出单个 `\x07` 字节。在 Windows 上只有当你确实想要恢复
  提示音时才使用它。
- `off` —— 完全禁用轮次结束通知。

在已知 OSC-9 终端（例如 Windows 上的 WezTerm）内运行的 Windows 用户
仍会收到 OSC-9 通知；`off` 回退仅在未检测到可识别的 `TERM_PROGRAM`
时适用。

### 已解析但当前未使用（为未来版本保留）

这些键会被配置加载器接受，但目前交互式 TUI 或内置工具并不使用：

- `tools_file`

## 工具目录

CodeSmith 默认加载一个精简的核心原生工具目录，并让不太常用的原生
工具可通过 ToolSearch 发现。要让特定原生工具在每次请求中都保持
加载，可将它们加入 `[tools].always_load`：

```toml
[tools]
always_load = ["git_show", "notify"]
```

## 功能开关

功能开关位于 `[features]` 表之下，并跨 profile 合并。内置工具的默认值
为启用，因此你只需设置想要强制开启或关闭的条目。

```toml
[features]
shell_tool = true
subagents = true
web_search = true # enables canonical web.run plus the compatibility web_search alias
apply_patch = true
mcp = true
exec_policy = true
# file_freshness = true # read-before-edit validation for edit_file/write_file/fim_edit/apply_patch
```

`file_freshness`（默认开启）使编辑工具拒绝会话中从未读取过、或自上次
读取后在磁盘上发生变化的文件——错误信息会提示模型先 `read_file`。
它是正确性防护，不是安全控制，在包括 Yolo 在内的所有模式下都适用。

你也可以为单次运行覆盖功能开关：

- `codesmith-tui --enable web_search`
- `codesmith-tui --disable subagents`

使用 `codesmith-tui features list` 查看已知开关及其生效状态。

## 网页搜索 Provider

`web_search` 默认使用 DuckDuckGo，不需要 API 密钥。DuckDuckGo 路径
在 DDG 返回机器人质询或无可解析结果时保留 Bing 回退。Bing 仍可供
明确想要它的用户选择，并且在偏好基于 API 的 provider 时，可以选择
Tavily、Bocha、Metaso 或 Baidu。

**Metaso**（[metaso.cn](https://metaso.cn)）每天有 100 次免费搜索
额度；设置 `METASO_API_KEY` 或 `[search] api_key` 可获得更高额度。

**Baidu** 使用位于
`https://qianfan.baidubce.com/v2/ai_search/web_search` 的百度 AI 搜索。
设置 `BAIDU_SEARCH_API_KEY` 或 `[search] api_key`。这只是搜索工具
后端；不会添加百度模型 provider。

```toml
[search]
provider = "baidu" # duckduckgo | bing | tavily | bocha | metaso | baidu
# api_key = "YOUR_KEY" # required for tavily, bocha, and baidu; optional for metaso
```

## 本地媒体附件

在输入框中使用 `@path/to/file` 可将本地文本文件或目录上下文添加到
下一条消息。使用 `/attach <path>` 附加本地图像/视频媒体路径，或使用
`Ctrl+V` 从剪贴板附加图像。DeepSeek 公开的 Chat Completions API 目前
只接受文本消息内容，因此媒体附件以显式的本地路径引用发送，而不是
原生图像/视频载荷。附件行在提交前显示在输入框上方；移动到输入框
开头，按 `↑` 选择附件行，然后按 `Backspace` 或 `Delete` 将其移除，
无需手动编辑占位文本。

## 托管配置与 requirements

codesmith 支持策略分层模型：

1. 用户配置 + profile + 环境变量覆盖
2. 托管配置（如果存在）
3. requirements 校验（如果存在）

在 Unix 上默认为：
- 托管配置：`/etc/deepseek/managed_config.toml`
- requirements：`/etc/deepseek/requirements.toml`

requirements 文件的形式：

```toml
allowed_approval_policies = ["on-request", "untrusted", "never"]
allowed_sandbox_modes = ["read-only", "workspace-write"]
```

如果配置的值违反 requirements，启动会失败并给出描述性错误。

公式、干预行为和遥测请参阅 `docs/capacity_controller.md`。

## 关于 `codesmith-tui doctor` 的说明

`codesmith-tui doctor` 遵循与 TUI 其余部分相同的配置解析规则。也就是
说 `--config`、`CODESMITH_CONFIG_PATH` 和旧版 `DEEPSEEK_CONFIG_PATH`
都会被尊重，MCP/skills 检查使用解析后的 `mcp_config_path` /
`skills_dir`（包括环境变量覆盖）。

要初始化缺失的 MCP/skills 路径，请运行 `codesmith-tui setup --all`。
也可以运行 `codesmith-tui setup --skills --local` 创建工作区本地的
`./skills` 目录。

`codesmith-tui doctor --json` 输出机器可读的报告，并跳过实时 API
连通性探测。顶层键有：`version`、`config_path`、`config_present`、
`workspace`、`api_key.source`、`base_url`、`default_text_model`、
`mcp`、`skills`、`tools`、`plugins`、`sandbox`、`platform`、
`api_connectivity`、`capability`。CI 使用方应依赖 `api_key.source`
（`env`/`config`/`missing`），而不是解析人类可读的 `doctor` 文本。

`capability` 键包含按 provider 的能力信息，这些信息派生自静态知识
（发布文档、API 指南）而不是实时 API 探测。顶层子键有：
`resolved_provider`、`resolved_model`、`context_window`、`max_output`、
`thinking_supported`、`cache_telemetry_supported` 和
`request_payload_mode`。

在 CI 脚本中使用 `capability.context_window` 和 `capability.max_output`
进行模型上限检查；不要把 `capability.max_output` 当作每轮请求预算。
使用 `capability.thinking_supported` 决定是否配置推理力度。

## setup 的 status、clean 与扩展目录

`codesmith-tui setup` 在现有的 `--mcp`、`--skills`、`--local`、
`--all` 和 `--force` 之外还接受几个标志：

- `--status` —— 打印紧凑的一屏状态（api key、base URL、模型、
  MCP/skills/tools/plugins 数量、沙箱、`.env` 是否存在）。只读且不
  联网；可在 CI 中安全运行。如果 `.env` 缺失而工作区中存在
  `.env.example`，状态输出会提示 `cp .env.example .env`。
- `--tools` —— 搭建 `~/.codesmith/tools/`，其中包含描述自描述
  frontmatter 约定（`# name:` / `# description:` / `# usage:`）的
  `README.md` 以及遵循该约定的 `example.sh`。该目录有意不做自动
  加载；请通过 MCP、hooks 或 skills 将单个脚本接入代理。
- `--plugins` —— 搭建 `~/.codesmith/plugins/`，其中包含 `README.md`
  和使用与 `SKILL.md` 相同 frontmatter 形式的 `example/PLUGIN.md`
  占位文件。插件同样不会自动加载；当你希望它们生效时，请从 skill、
  hook 或 MCP 包装器中引用它们。
- `--all` —— 现在一起搭建 MCP + skills + tools + plugins。
- `--clean` —— 列出 `~/.codesmith/sessions/checkpoints/latest.json` 和
  `offline_queue.json`（如果存在）。旧版
  `~/.deepseek/sessions/checkpoints/` 文件不会被自动扫描；设置
  `CODESMITH_HOME=~/.deepseek` 可进行一次性的旧版清理。传入
  `--force` 才会实际删除匹配的文件。这绝不会触碰真实的会话历史或
  任务队列。

`--status` 和 `--clean` 与脚手架类标志互斥。

## 引擎为何剥离 XML/`[TOOL_CALL]` 文本

codesmith 仅通过 API 工具通道（结构化的 `tool_use` / `tool_call` 项）
发送和接收工具调用。`crates/tui/src/core/engine.rs` 中的流式循环会
识别一组固定的伪包装器起始标记——`[TOOL_CALL]`、`<codesmith:tool_call`、
`<tool_call`、`<invoke `、`<function_calls>`——并将它们从可见的助手
文本中清除，而绝不会把它们变成结构化工具调用。当包装器被剥离时，
循环会在该轮发出一条紧凑的 `status` 通知，让用户明白为什么可见文本
变少了。任何重新启用基于文本的工具执行的更改都应视为回归；
`crates/tui/tests/protocol_recovery.rs` 中的协议恢复测试锁定了该契约。
