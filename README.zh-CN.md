# CodeSmith

> **面向 [DeepSeek V4](https://platform.deepseek.com) 的终端原生编程智能体：100 万 token 上下文、思考模式流式推理、前缀缓存感知。以 `codesmith` 调度器和 `codesmith-tui` 运行时这一组自包含 Rust 二进制发布——开箱即带 MCP 客户端、沙箱和持久化任务队列。**

[English README](README.md)


## 安装

`codesmith` 以一组自包含 Rust 发布二进制安装：`codesmith` 调度器命令，
以及它在交互会话中启动的同级 `codesmith-tui` 运行时。npm、Homebrew 和
Docker 会自动安装这两个二进制；Cargo 或手动下载时必须把两者放在同一目录
（通常是 `PATH` 上的某个目录）。运行时不依赖 Node.js 或 Python。

```bash
# 1. npm —— 已装 Node 的最方便方式。npm 包只是一个下载器，
#    会从 GitHub Releases 拉取对应平台的预编译二进制对，
#    并不会让 codesmith 本身依赖 Node 运行时。
npm install -g codesmith

# 2. Cargo —— 无需 Node，两个 crate 都要安装。
cargo install codesmith-cli --locked   # `codesmith` 入口
cargo install codesmith-tui     --locked   # `codesmith-tui` TUI 二进制

# 3. Homebrew —— macOS 包管理器。
#    tap/formula 名称仍是旧名；实际安装 codesmith 和 codesmith-tui。
brew tap Hmbown/deepseek-tui
brew install codesmith

# 4. 直接下载 —— GitHub Releases 的平台压缩包。
#    https://github.com/Hmbown/CodeSmith/releases
#    压缩包包含 codesmith 和 codesmith-tui 以及安装脚本；
#    也提供单独二进制给脚本使用，手动安装时请把这一对放在一起。

# 5. Docker —— 预构建发布镜像。
docker volume create codesmith-home
docker run --rm -it \
  -e DEEPSEEK_API_KEY="$DEEPSEEK_API_KEY" \
  -v codesmith-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/hmbown/codesmith:latest
```

> 中国大陆访问较慢时，npm 可加 `--registry=https://registry.npmmirror.com`，
> 或使用下方的 [Cargo 镜像](#中国大陆--镜像友好安装)。
>
> 下载安全：官方二进制只发布在
> `https://github.com/Hmbown/CodeSmith/releases`。手动下载时请校验
> SHA-256 manifest，并避免相似仓库名或搜索结果里的镜像站。详见
> [下载安全与校验](docs/INSTALL.md#2-download-safety-and-checksums)。

已经安装过？按你的安装方式更新：

```bash
codesmith update                         # release 二进制更新器
npm install -g codesmith@latest      # npm 包装器
brew update && brew upgrade codesmith
cargo install codesmith-cli --locked --force
cargo install codesmith-tui     --locked --force
```
> codesmith update 现在可添加 --proxy ,通过代理下载更新
> eg: codesmith update --proxy https://localhost:7897

[![CI](https://github.com/Hmbown/CodeSmith/actions/workflows/ci.yml/badge.svg)](https://github.com/Hmbown/CodeSmith/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/codesmith)](https://www.npmjs.com/package/codesmith)
[![crates.io](https://img.shields.io/crates/v/codesmith-cli?label=crates.io)](https://crates.io/crates/codesmith-cli)
[DeepWiki project index](https://deepwiki.com/Hmbown/CodeSmith)

![codesmith 截图](assets/screenshot.png)

---

## 这是什么？

模型回答问题。智能体完成任务。区别在于运行框架——一套在模型偏离时保持方向的规则、证据和反馈系统。

CodeSmith 就是这套框架，围绕 DeepSeek V4 构建，基于三个理念：

| 原则 | 如何运作 |
|---|---|
| **从信任开始** | 每一轮以"A"开始——可能性先于确定性，匠心先于便利 |
| **清晰的管辖权** | 成文宪法，九层权威。用户意图优先于陈旧指令。验证优先于自信。 |
| **递归改进** | V4 参与了框架的编写。框架改进 → V4 更高效 → 进一步改进框架。每轮从更强的位置开始。 |

开源、终端原生，并以 `codesmith` / `codesmith-tui` 这一组 Rust 二进制发布。

## 框架如何工作

智能体模型面临大规模的冲突信息：用户意图、项目规则、系统默认值、工具输出和陈旧记忆在单轮对话中争夺权威。LLM 作为裁判需要管辖权——当它们冲突时，哪个来源胜出？

CodeSmith 用一部**宪法**（`prompts/base.md`）来回答这个问题。它是一个形式化的法律层级——第七条将九个来源从宪法本身的条款排到前序会话的交接记录。用户当前消息优先于陈旧的项目指令。实时工具输出优先于假设。验证优先于自信。模型每轮继承清晰的权威链，永远不需要猜测该服从哪条指令。

七条条款位于层级之上，定义模型的身份、职责和能动性：验证强制（第五条——每个行动留下证据，绝不凭信念宣告成功）、协作遗产（第六条——让工作区对下一位智能体保持可读）、以及真相优先条款（第二条——任何下级规则不得覆盖它）。

DeepSeek V4 的前缀缓存使其可行。宪法篇幅长且详细，但一旦缓存，每轮成本约为冷读取的百分之一。模型递归引用它——通过 RLM 会话窥视、扫描和查询——按需重访信息，而非依赖单次记忆读取。它的表现更像是开卷考试而非闭卷考试。

因为权威结构是显式的，失败不会被隐藏。非零退出码、两次轮次间来自 rust-analyzer 的类型错误、沙箱拒绝——这些被作为修正向量反馈。模型用自己的漂移进行自我校正。

三种模式控制行动空间。Plan 只读。Agent 对破坏性操作设审批门控。YOLO 在可信工作区自动批准。操作系统级沙箱按平台执行：macOS Seatbelt、Linux Landlock + seccomp（外加可选的 bubblewrap）、Windows Job Object v1。详见 [docs/SANDBOX.md](docs/SANDBOX.md)。

Fin——关闭思考的廉价 Flash 调用——每轮处理模型自动路由。`--model auto` 是默认值。

每轮记录 side-git 快照，在仓库 `.git` 之外。`/restore` 和 `revert_turn` 即刻回滚工作区。

子智能体并发运行（最多 20 个）。`agent_open` 立即返回；结果以内联完成哨兵形式到达，携带摘要。完整对话记录通过 `agent_eval` 的有界句柄保存。详见 [docs/SUBAGENTS.md](docs/SUBAGENTS.md)。

其余功能面：每次编辑后的 LSP 诊断（rust-analyzer、pyright、typescript-language-server、gopls、clangd、jdtls、vue-language-server）、RLM 会话批量分析、MCP 协议、HTTP/SSE 运行时 API、持久化任务队列、Zed 的 ACP 适配器、SWE-bench 导出、以及带缓存命中/未命中明细的实时成本追踪。

---

## 运行框架

`codesmith`（调度器 CLI）→ `codesmith-tui`（伴随二进制）→ ratatui 界面 ↔ 异步引擎 ↔ OpenAI 兼容流式客户端。工具调用通过类型化注册表（shell、文件操作、git、web、子智能体、MCP、RLM）路由，结果流式返回对话记录。引擎管理会话状态、轮次追踪、持久化任务队列和 LSP 子系统——它在下一步推理前将编辑后诊断反馈到模型上下文中。

详见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

### 子智能体：并发后台执行

codesmith 可以同时调度多个子智能体并行运行——类似于并发任务队列：

- **非阻塞启动。** `agent_open` 立即返回。子智能体获得独立的上下文和工具注册表，独立运行。父进程继续工作。
- **后台执行。** 子智能体并发运行（默认上限 10，可配置至 20）。引擎管理线程池——无需轮询循环。
- **完成通知。** 子智能体完成后，运行时向父对话注入 `<codesmith:subagent.done>` 哨兵。人类可读的摘要（包含子智能体的发现、变更文件和风险）位于哨兵的紧前一行。父模型读取该摘要并整合结果，无需额外工具调用。
- **按需读取结果。** 完整子对话记录通过 `agent_eval` 获取的 `transcript_handle` 暂存。摘要不够时，父进程通过 `handle_read` 按切片、行范围或 JSONPath 投影读取——保持父上下文精简而不丢失细节。

详见 [docs/SUBAGENTS.md](docs/SUBAGENTS.md)。

---

## 快速开始

```bash
npm install -g codesmith
codesmith --version
codesmith --model auto
```

预构建二进制对和平台压缩包覆盖 **Linux x64**、**Linux ARM64**（v0.8.8 起）、**macOS x64**、**macOS ARM64** 和 **Windows x64**。其他目标平台（musl、riscv64、FreeBSD 等）请见下方的[从源码安装](#从源码安装)或 [docs/INSTALL.md](docs/INSTALL.md)。

首次启动时会提示输入 [DeepSeek API key](https://platform.deepseek.com/api_keys)。密钥保存到 `~/.codesmith/config.toml`（同时兼容旧版 `~/.codesmith/config.toml`），在任意目录、IDE 终端和脚本中都能使用，不会触发系统密钥环弹窗。

也可以提前配置：

```bash
codesmith auth set --provider deepseek   # 保存到 ~/.codesmith/config.toml

codesmith auth status                    # 显示当前活跃的凭证来源
export DEEPSEEK_API_KEY="YOUR_KEY"      # 环境变量方式；需要在非交互式 shell 中使用请放入 ~/.zshenv
codesmith

codesmith doctor                          # 验证安装
```

> 轮换或移除密钥：`codesmith auth clear --provider deepseek`。

### 腾讯云 / CNB 远程优先路径

如果你想要一个长期在线、可从手机控制的工作区，推荐使用腾讯云原生路径：
CNB 镜像/源码，腾讯云 Lighthouse 香港实例，飞书/Lark 长连接桥接，
以及可选的 EdgeOne 公网 HTTPS 边缘。运行时 API 必须绑定在 localhost；
不要通过 EdgeOne 暴露 `/v1/*`。

先看 [docs/TENCENT_CLOUD_REMOTE_FIRST.md](docs/TENCENT_CLOUD_REMOTE_FIRST.md)，
再按 [docs/TENCENT_LIGHTHOUSE_HK.md](docs/TENCENT_LIGHTHOUSE_HK.md) 配置服务器。

### Auto 模式

使用 `codesmith --model auto` 或 `/model auto` 让 codesmith 自行决定每轮需要多少模型和推理能力。

Auto 模式同时控制两个设置：

- 模型：`deepseek-v4-flash` 或 `deepseek-v4-pro`
- 推理强度：`off`、`high` 或 `max`

在真实请求发出之前，应用会先用关闭推理的 `deepseek-v4-flash` 进行一次小型路由调用。路由器审视最新请求和最近的上下文，然后为真实请求选定具体的模型和推理强度。简短/简单的轮次保持在 Flash + 关闭推理；编码、调试、发布、架构、安全审查或模糊的多步骤任务可升级到 Pro 和/或更高推理强度。

`auto` 是 codesmith 本地行为。上游 API 永远不会收到 `model: "auto"`，它只会收到为当前轮次选定的具体模型和推理强度设置。TUI 会显示选定的路由，成本跟踪按实际运行的模型计费。如果路由调用失败或返回无效答案，应用会回退到本地启发式规则。子智能体会继承 auto 模式，除非你为它们指定了显式模型。

需要可重复基准测试、严格控制成本上限或特定提供商/模型映射时，请使用固定模型或固定推理强度。

### Linux ARM64（HarmonyOS 轻薄本、openEuler、Kylin、树莓派、Graviton 等）

从 v0.8.8 起，`npm i -g codesmith` 直接支持 glibc 系的 ARM64 Linux。你也可以从 [Releases 页面](https://github.com/Hmbown/CodeSmith/releases) 下载预编译二进制，放到 `PATH` 目录中。

### 中国大陆 / 镜像友好安装

如果在中国大陆访问 GitHub 或 npm 下载较慢，可以通过 Cargo 注册表镜像安装：

```toml
# ~/.cargo/config.toml
[source.crates-io]
replace-with = "tuna"

[source.tuna]
registry = "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/"
```

然后安装两个二进制（调度器在运行时会调用 TUI）：

```bash
cargo install codesmith-cli --locked   # 提供推荐入口 `codesmith`
cargo install codesmith-tui     --locked   # 提供交互式 TUI 伴随二进制
codesmith --version
```

也可以直接从 [GitHub Releases](https://github.com/Hmbown/CodeSmith/releases) 下载预编译二进制。`CODESMITH_RELEASE_BASE_URL` 可用于镜像后的 release 资产。

### Windows (Scoop)

[Scoop](https://scoop.sh) 是一个 Windows 软件包管理器。codesmith 已进入
Scoop main bucket，但该 manifest 独立更新，可能滞后于 GitHub/npm/Cargo
release。先运行 `scoop update`，安装后用 `codesmith --version` 核对版本：

```bash
scoop update
scoop install codesmith
codesmith --version
```

如果需要最新版本，请优先使用 npm 或直接下载 GitHub Release 资产。


<details id="install-from-source">
<summary>从源码安装</summary>

适用于任何 Tier-1 Rust 目标，包括 musl、riscv64、FreeBSD 以及尚无预编译包的 ARM64 发行版。

```bash
# Linux 构建依赖（Debian/Ubuntu/RHEL）：
#   sudo apt-get install -y build-essential pkg-config libdbus-1-dev
#   sudo dnf install -y gcc make pkgconf-pkg-config dbus-devel

git clone https://github.com/Hmbown/CodeSmith.git
cd CodeSmith

cargo install --path crates/cli --locked   # 需要 Rust 1.88+；提供 `codesmith`
cargo install --path crates/tui --locked   # 提供 `codesmith-tui`
```

两个二进制都需要安装。交叉编译和平台特定说明见 [docs/INSTALL.md](docs/INSTALL.md)。

</details>

### 其他模型提供方

```bash
# NVIDIA NIM
codesmith auth set --provider nvidia-nim --api-key "YOUR_NVIDIA_API_KEY"
codesmith --provider nvidia-nim

# AtlasCloud
codesmith auth set --provider atlascloud --api-key "YOUR_ATLASCLOUD_API_KEY"
codesmith --provider atlascloud

# Wanjie Ark
codesmith auth set --provider wanjie-ark --api-key "YOUR_WANJIE_API_KEY"
codesmith --provider wanjie-ark --model deepseek-reasoner

# OpenRouter
codesmith auth set --provider openrouter --api-key "YOUR_OPENROUTER_API_KEY"
codesmith --provider openrouter --model deepseek/deepseek-v4-pro
codesmith --provider openrouter --model arcee-ai/trinity-large-thinking
codesmith --provider openrouter --model qwen/qwen3.7-max

# Xiaomi MiMo
codesmith auth set --provider xiaomi-mimo --api-key "YOUR_XIAOMI_MIMO_API_KEY"
codesmith --provider xiaomi-mimo --model mimo-v2.5-pro

# Novita
codesmith auth set --provider novita --api-key "YOUR_NOVITA_API_KEY"
codesmith --provider novita --model deepseek/deepseek-v4-pro

# Fireworks
codesmith auth set --provider fireworks --api-key "YOUR_FIREWORKS_API_KEY"
codesmith --provider fireworks --model deepseek-v4-pro

# SiliconFlow
codesmith auth set --provider siliconflow --api-key "YOUR_SILICONFLOW_API_KEY"
codesmith --provider siliconflow --model deepseek-ai/DeepSeek-V4-Pro

# 通用 OpenAI 兼容端点
codesmith auth set --provider openai --api-key "YOUR_OPENAI_COMPATIBLE_API_KEY"
OPENAI_BASE_URL="https://openai-compatible.example/v4" codesmith --provider openai --model glm-5

# 自托管 SGLang
SGLANG_BASE_URL="http://localhost:30000/v1" codesmith --provider sglang --model deepseek-v4-flash

# 自托管 vLLM
VLLM_BASE_URL="http://localhost:8000/v1" codesmith --provider vllm --model deepseek-v4-flash

# 自托管 Ollama
ollama pull codesmith-coder:1.3b
codesmith --provider ollama --model codesmith-coder:1.3b
```

在 TUI 内，`/provider` 打开提供方选择器，`/model` 打开本地模型/思考模式
选择器。`/provider openrouter` 和 `/model <id>` 可直接切换；`/models` 会在
当前提供方支持模型列表时显式请求并列出 API 返回的实时模型。

---

## 版本说明

每个版本的具体变更见 [CHANGELOG.md](CHANGELOG.md)。README 只保留当前
安装方式、核心工作流、模型提供方配置、运行时接口和扩展入口。

---

## 使用方式

```bash
codesmith                                       # 交互式 TUI
codesmith "explain this function"              # 一次性提示
codesmith exec --auto --output-format stream-json "fix this bug" # 面向后端集成的 NDJSON 流
codesmith exec --resume <SESSION_ID> "follow up" # 继续非交互会话
codesmith --model deepseek-v4-flash "summarize" # 指定模型
codesmith --model auto "fix this bug"          # 自动选择模型 + 推理强度
codesmith --yolo                                # 自动批准工具
codesmith auth set --provider deepseek         # 保存 API key
codesmith doctor                                # 检查配置和连接
codesmith doctor --json                         # 机器可读诊断
codesmith setup --status                        # 只读安装状态
codesmith setup --tools --plugins               # 创建本地工具和插件目录
codesmith models                                # 列出可用 API 模型
codesmith sessions                              # 列出已保存会话
codesmith resume --last                         # 恢复最近会话
codesmith resume <SESSION_ID>                   # 按 UUID 恢复指定会话
codesmith fork <SESSION_ID>                     # 将已保存会话分叉为兄弟路径
codesmith serve --http                          # HTTP/SSE API 服务
codesmith serve --mobile                        # 局域网移动端控制页，默认启用 token 保护
codesmith serve --acp                           # Zed/自定义智能体的 ACP stdio 适配器
codesmith run pr <N>                            # 获取 PR 并预填审查提示
codesmith mcp list                              # 列出已配置 MCP 服务器
codesmith mcp validate                          # 校验 MCP 配置和连接
codesmith mcp-server                            # 启动 dispatcher MCP stdio 服务器
codesmith update                                # 检查并应用二进制更新
```

Docker 镜像发布在 GHCR 上：

```bash
docker volume create codesmith-home

docker run --rm -it \
  -e DEEPSEEK_API_KEY="$DEEPSEEK_API_KEY" \
  -v codesmith-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/hmbown/codesmith:latest
```

固定 tag、本地构建、volume 权限和非交互管道用法见 [docs/DOCKER.md](docs/DOCKER.md)。

### Zed / ACP

CodeSmith 可作为自定义 Agent Client Protocol 服务器运行，供 Zed 等编辑器通过 stdio 调用本地 ACP 智能体。在 Zed 中添加自定义智能体服务器：

```json
{
  "agent_servers": {
    "CodeSmith": {
      "type": "custom",
      "command": "codesmith",
      "args": ["serve", "--acp"],
      "env": {}
    }
  }
}
```

首个 ACP 切片支持通过现有 CodeSmith 配置/API 密钥创建新会话和提示响应。工具支持的编辑和检查点回放尚未通过 ACP 暴露。

### 常用快捷键

| 按键 | 功能 |
|---|---|
| `Tab` | 补全 `/` 或 `@`；运行中则把草稿排队；否则切换模式 |
| `Shift+Tab` | 切换推理强度：off → high → max |
| `F1` | 可搜索帮助面板 |
| `Esc` | 返回 / 关闭 |
| `Ctrl+K` | 命令面板 |
| `Ctrl+R` | 恢复旧会话 |
| `Alt+R` | 搜索提示历史和恢复草稿 |
| `Ctrl+S` | 暂存当前草稿（`/stash list`、`/stash pop` 恢复） |
| `@path` | 在输入框中附加文件或目录上下文 |
| `↑`（在输入框开头） | 选择附件行进行移除 |

完整快捷键目录：[docs/KEYBINDINGS.md](docs/KEYBINDINGS.md)。

---

## 模式

| 模式 | 行为 |
|---|---|
| **Plan** 🔍 | 只读调查；模型先探索并提出计划（`update_plan` + `checklist_write`），然后再做更改 |
| **Agent** 🤖 | 默认交互模式；多步工具调用带审批门禁 |
| **YOLO** ⚡ | 在可信工作区自动批准工具；仍会维护计划和清单以保持可见性 |

---

## 配置

用户配置：`~/.codesmith/config.toml`。项目覆盖：`<workspace>/.codesmith/config.toml`（（以下密钥被拒绝：`api_key`、`base_url`、`provider`、`mcp_config_path`）。完整选项见 [config.example.toml](config.example.toml)。

常用环境变量：

| 变量 | 用途 |
|---|---|
| `DEEPSEEK_API_KEY` | DeepSeek API key |
| `CODESMITH_BASE_URL` | API base URL |
| `CODESMITH_HTTP_HEADERS` | 可选模型请求头，例如 `X-Model-Provider-Id=your-model-provider` |
| `CODESMITH_MODEL` | 默认模型 |
| `CODESMITH_STREAM_IDLE_TIMEOUT_SECS` | 流式响应空闲超时秒数，默认 `300`，限制在 `1..=3600` |
| `CODESMITH_PROVIDER` | `deepseek`（默认）、`nvidia-nim`、`openai`、`atlascloud`、`wanjie-ark`、`volcengine`、`openrouter`、`xiaomi-mimo`、`novita`、`fireworks`、`siliconflow`、`moonshot`、`sglang`、`vllm`、`ollama` |
| `CODESMITH_PROFILE` | 配置 profile 名称 |
| `CODESMITH_MEMORY` | 设为 `on` 启用用户记忆 |
| `CODESMITH_ALLOW_INSECURE_HTTP=1` | 在可信网络上允许非本机 `http://` API base URL |
| `NVIDIA_API_KEY` / `OPENAI_API_KEY` / `ATLASCLOUD_API_KEY` / `WANJIE_ARK_API_KEY` / `VOLCENGINE_API_KEY` / `ARK_API_KEY` / `OPENROUTER_API_KEY` / `XIAOMI_MIMO_API_KEY` / `MIMO_API_KEY` / `NOVITA_API_KEY` / `FIREWORKS_API_KEY` / `SILICONFLOW_API_KEY` / `MOONSHOT_API_KEY` / `KIMI_API_KEY` / `SGLANG_API_KEY` / `VLLM_API_KEY` / `OLLAMA_API_KEY` | 提供商认证 |
| `OPENAI_BASE_URL` / `OPENAI_MODEL` | 通用 OpenAI 兼容端点和模型 ID |
| `ATLASCLOUD_BASE_URL` / `ATLASCLOUD_MODEL` | AtlasCloud 端点和模型覆盖 |
| `WANJIE_ARK_BASE_URL` / `WANJIE_ARK_MODEL` | Wanjie Ark 端点和模型覆盖 |
| `VOLCENGINE_BASE_URL` / `ARK_BASE_URL` / `VOLCENGINE_MODEL` / `ARK_MODEL` | Volcengine Ark 端点和模型覆盖 |
| `OPENROUTER_BASE_URL` | OpenRouter 端点覆盖 |
| `XIAOMI_MIMO_BASE_URL` / `MIMO_BASE_URL` / `XIAOMI_MIMO_MODEL` / `MIMO_MODEL` | Xiaomi MiMo 端点和模型覆盖 |
| `NOVITA_BASE_URL` | Novita 端点覆盖 |
| `FIREWORKS_BASE_URL` | Fireworks 端点覆盖 |
| `SILICONFLOW_BASE_URL` / `SILICONFLOW_MODEL` | SiliconFlow 端点和模型覆盖 |
| `SGLANG_BASE_URL` | 自托管 SGLang 端点 |
| `SGLANG_MODEL` | 自托管 SGLang 模型 ID |
| `VLLM_BASE_URL` | 自托管 vLLM 端点 |
| `VLLM_MODEL` | 自托管 vLLM 模型 ID |
| `OLLAMA_BASE_URL` | 自托管 Ollama 端点 |
| `OLLAMA_MODEL` | 自托管 Ollama 模型标签 |
| `NO_ANIMATIONS=1` | 启动时强制无障碍模式 |
| `SSL_CERT_FILE` | 企业代理的自定义 CA 包 |

`locale` 会控制界面语言，并作为模型自然语言的兜底设置；最新用户消息的语言优先级更高。也就是说，即使系统 locale 是英文，用户用中文提问时，V4 的 `reasoning_content` 和最终回复也应该使用中文。可在 `config.toml` 中设置 `locale`、使用 `/config locale zh-Hans`、或依赖 `LC_ALL`/`LANG`。详见 [docs/LOCALIZATION.md](docs/LOCALIZATION.md) 和 [docs/CONFIGURATION.md](docs/CONFIGURATION.md)。

### 切换为中文界面

如果界面是其他语言，可以在 TUI 内一键切换为简体中文：

1. 在 Composer 里输入 `/config`，按 Tab 或 Enter 打开配置面板。
2. 选择 **Edit locale**，在 `New:` 字段输入 `zh-Hans`，按 Enter 应用。

可选语言：`auto` | `en` | `zh-Hans` | `zh-Hant` | `hi` | `es-419`。

也可以在 `~/.codesmith/config.toml` 里直接设置 `locale = "zh-Hans"`，或通过 `LC_ALL` / `LANG` 环境变量自动选择：

```toml
# ~/.codesmith/config.toml
[tui]
locale = "zh-Hans"
```

或者通过环境变量（中文系统通常已自动生效）：

```bash
LANG=zh_CN.UTF-8 codesmith run
```

---


## 创建和安装技能

codesmith 从工作区目录（`.agents/skills` → `skills` → `.opencode/skills` → `.claude/skills` → `.cursor/skills` → `.codesmith/skills`）和全局目录（`~/.agents/skills` → `~/.claude/skills` → `~/.codesmith/skills` → `~/.codesmith/skills`）发现技能。每个技能是一个包含 `SKILL.md` 的目录：

```text
~/.codesmith/skills/my-skill/
└── SKILL.md
```

需要 YAML frontmatter：

```markdown
---
name: my-skill
description: 当 CodeSmith 需要遵循我的自定义工作流时使用这个技能。
---

# My Skill
这里写给智能体的指令。
```

常用命令：`/skills`（列出）、`/skill <name>`（激活）、`/skill new`（创建）、`/skill install github:<owner>/<repo>`（社区）、`/skill update` / `uninstall` / `trust`。社区技能直接从 GitHub 安装，无需后端服务。已安装技能在模型可见的会话上下文里列出；当任务匹配技能描述时，智能体可通过 `load_skill` 工具自动读取对应的 `SKILL.md`。

---

## 文档

| 文档 | 主题 |
|---|---|
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | 代码库内部结构 |
| [DESIGN_INTERNALS.md](docs/DESIGN_INTERNALS.md) | 深度设计：框架核心、provider 接缝、guardrail、扩展系统 |
| [CONFIGURATION.md](docs/CONFIGURATION.md) | 完整配置参考 |
| [MODES.md](docs/MODES.md) | Plan / Agent / YOLO 模式 |
| [MCP.md](docs/MCP.md) | Model Context Protocol 集成 |
| [RUNTIME_API.md](docs/RUNTIME_API.md) | HTTP/SSE API 服务和移动端控制页 |
| [INSTALL.md](docs/INSTALL.md) | 各平台安装指南 |
| [DOCKER.md](docs/DOCKER.md) | GHCR 镜像、volume 和 Docker 用法 |
| [MEMORY.md](docs/MEMORY.md) | 用户记忆功能指南 |
| [SUBAGENTS.md](docs/SUBAGENTS.md) | 子智能体角色分类与生命周期 |
| [KEYBINDINGS.md](docs/KEYBINDINGS.md) | 完整快捷键目录 |
| [RELEASE_RUNBOOK.md](docs/RELEASE_RUNBOOK.md) | 发布流程 |
| [LOCALIZATION.md](docs/LOCALIZATION.md) | UI 语言矩阵与切换 |
| [OPERATIONS_RUNBOOK.md](docs/OPERATIONS_RUNBOOK.md) | 运维和恢复 |

完整更新历史：[CHANGELOG.md](CHANGELOG.md)。

---

## 致谢

感谢[CodeWhale](https://github.com/Hmbown/CodeWhale)，本项目参考了它。

## 帮助与支持

- **使用问题**——先查阅[文档](#文档)；仍有疑问可[提交 issue](https://github.com/Hmbown/CodeSmith/issues/new/choose)。
- **Bug 与功能请求**——使用 [issue 模板](https://github.com/Hmbown/CodeSmith/issues/new/choose)。
- **安全漏洞**——请**勿**公开开 issue，按 [SECURITY.md](SECURITY.md) 流程私下报告。
- 完整分流指引见 [SUPPORT.md](SUPPORT.md)。

## 贡献

欢迎提交 pull request——请先查看 [CONTRIBUTING.md](CONTRIBUTING.md) 并留意[开放 issue](https://github.com/Hmbown/CodeSmith/issues) 中的好入门任务。


## 许可证

[MIT](LICENSE)

## Star 历史

[![Star History Chart](https://api.star-history.com/chart?repos=Hmbown/CodeSmith&type=date&legend=top-left)](https://www.star-history.com/?repos=Hmbown%2FCodeSmith&type=date&logscale=&legend=top-left)
