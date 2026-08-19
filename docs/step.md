# 源码安装后的设置步骤

适用于通过 `cargo install --path crates/cli --locked` 完成安装后，让产物真正可用的全部设置。

## 背景

`cargo install --path crates/cli --locked` 只会把 `codesmith`（分发器）等 4 个二进制安装到 `~/.cargo/bin/`。但 `codesmith` 是一个门面（facade）：`run`、`exec`、`doctor`、`init`、`serve` 等几乎所有实际命令，都会在运行时去同目录查找并 spawn 伴生二进制 `codesmith-tui`（见 `crates/cli/src/lib.rs` 的 `locate_sibling_tui_binary()`），找不到会直接报错：

```text
Companion `codesmith-tui` binary not found
```

`codesmith-tui` 属于另一个包 `crates/tui`，`cargo install --path crates/cli` 不会安装它。因此源码安装必须执行两条命令（参见 `README.md` 与 `docs/INSTALL.md`："Both binaries are required"）。

## 步骤 1：安装伴生 TUI 二进制（必需）

```bash
cargo install --path crates/tui --locked
```

## 步骤 2：验证两个二进制就位

```bash
which codesmith codesmith-tui
codesmith --version
```

说明：

- 仅安装 cli 后，本地即可用的命令有 `codesmith --version`、`codesmith auth ...`、`codesmith config ...`、`codesmith model list` 等；交互与 `exec`/`doctor` 等命令需要 `codesmith-tui`。
- 若 `codesmith-tui` 不在默认位置，也可通过环境变量 `DEEPSEEK_TUI_BIN` 指向已存在的 `codesmith-tui` 绝对路径。

## 步骤 3：配置 API key（唯一硬性凭据，三选一）

默认 provider 为 DeepSeek（默认模型 `deepseek-v4-pro`，默认 base URL `https://api.deepseek.com/beta`），凭据优先级为 CLI `--api-key` → config 文件 → OS keyring → 环境变量。

方式 A（推荐，写入 `~/.codesmith/config.toml`）：

```bash
codesmith auth set --provider deepseek
```

方式 B（环境变量）：

```bash
export DEEPSEEK_API_KEY="sk-..."
```

方式 C：首次运行 `codesmith` 时按 onboarding 提示输入（同时会完成 workspace 信任确认）。

切换其他 provider 时使用对应环境变量：`OPENAI_API_KEY`、`ANTHROPIC_API_KEY`、`OPENROUTER_API_KEY`、`NVIDIA_API_KEY`、`MOONSHOT_API_KEY`、`SILICONFLOW_API_KEY`、`VOLCENGINE_API_KEY` 等，完整列表参考 `.env.example` 与 `codesmith auth set --provider <name>`。

注意（openai provider）：`provider = "openai"` 在官方端点（`https://api.openai.com/v1`）的默认模型为 `gpt-5`；若把 `OPENAI_BASE_URL` 指向第三方 OpenAI 兼容网关（例如 `https://open.bigmodel.cn/api/v1`），必须同时显式设置模型——`OPENAI_MODEL`（填网关实际提供的模型，如 `glm-4.6`）、`--model`，或 `~/.codesmith/config.toml` 中 `[providers.openai]` 的 `model`。缺少模型时启动会直接本地报错提示，而不是把默认模型名发给网关、收到 403 `model_access_denied`。

## 步骤 4：验证端到端可用

```bash
codesmith auth status
codesmith doctor          # 完整体检（需要 codesmith-tui）
codesmith exec "hello"    # 一次真实 LLM 调用
```

之后即可进入交互模式：

```bash
codesmith                  # 交互 TUI
codesmith --model auto     # 自动路由模式（README 推荐）
```

## 无需手动准备的内容

- 配置文件 `~/.codesmith/config.toml`：不存在时使用默认值，首次保存时自动创建目录并设置权限 0600；可用 `$CODESMITH_HOME` / `$CODESMITH_CONFIG_PATH` 或 `--config` 覆盖路径，支持项目级覆盖 `<workspace>/.codesmith/config.toml`。
- 状态库 `~/.deepseek/state.db`：按需自动创建。
- 核心 prompt / 子代理模板：编译期已通过 `include_str!` 嵌入二进制，运行时不依赖仓库文件。
- skills 目录（`~/.codesmith/skills`）与 MCP 配置（`~/.codesmith/mcp.json`）：可选，不存在时自动跳过，可由 `codesmith setup` 按需引导创建。
- `~/.cargo/bin`：只要 `cargo` 本身可用即在 PATH 中，无需额外设置。
