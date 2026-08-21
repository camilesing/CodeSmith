# 品牌重塑：DeepSeek TUI → CodeSmith

从 **v0.8.41** 开始，本项目以新名称发布：`codesmith`。

本文档说明哪些变了、哪些没变，以及如何迁移。DeepSeek provider 集成没有任何
变化——变的只是本地 CLI / TUI 品牌。

## TL;DR

```bash
# 1. Uninstall the old wrapper or binaries.
npm uninstall -g deepseek-tui      # or cargo uninstall deepseek-tui-cli deepseek-tui
                                    # or brew uninstall deepseek-tui

# 2. Install under the new name.
npm install -g codesmith            # or cargo install codesmith-cli codesmith-tui --locked
                                    # or brew install deepseek-tui (Homebrew tap still
                                    #     uses the legacy name during the transition;
                                    #     it installs the new binaries underneath.)

# 3. Run with the new command.
codesmith doctor
codesmith
```

你的 `~/.deepseek/config.toml`、`~/.deepseek/sessions/`、`~/.deepseek/skills/`、
`~/.deepseek/tasks/` 和 `~/.deepseek/mcp.json` 均不受影响。现有的 `DEEPSEEK_*`
环境变量继续有效。

## 改名内容

| 面 | 之前 | 之后 |
|---|---|---|
| CLI 调度器二进制 | `deepseek` | `codesmith` |
| TUI 运行时二进制 | `deepseek-tui` | `codesmith-tui` |
| npm 包装器包 | `deepseek-tui` | `codesmith` |
| Crates.io crate | `deepseek-tui-cli` / `deepseek-tui` / `deepseek-*` | `codesmith-cli` / `codesmith-tui` / `codesmith-*` |
| 发布资产 | `deepseek-<platform>` / `deepseek-tui-<platform>` | `codesmith-<platform>` / `codesmith-tui-<platform>` |
| 校验和清单 | `deepseek-artifacts-sha256.txt` | `codesmith-artifacts-sha256.txt` |

## 未变内容

所有面向 DeepSeek provider API 的内容都保持原样：

- **环境变量**：`DEEPSEEK_API_KEY`、`DEEPSEEK_BASE_URL`、`DEEPSEEK_MODEL`、
  `DEEPSEEK_PROVIDER`、`DEEPSEEK_PROFILE`、`DEEPSEEK_YOLO`、`DEEPSEEK_LOG_LEVEL`，
  以及现有的 `DEEPSEEK_TUI_*` 运行时开关（`DEEPSEEK_TUI_BIN`、
  `DEEPSEEK_TUI_RELEASE_BASE_URL` 等）。它们为向后兼容而保留；重命名它们会
  破坏世界上每一份 shell rc。现在每个应用级 `DEEPSEEK_*` 开关也接受
  `CODESMITH_*` 名称（由 `codesmith_config::codesmith_env` 解析，顺序为
  `CODESMITH_` → `CODEWHALE_` → `DEEPSEEK_`）；CLI 门面会以 `CODESMITH_*`
  名称转发参数，`.env.example` 中也有展示。
- **模型 ID**：`deepseek-v4-pro`、`deepseek-v4-flash`，以及旧别名
  `deepseek-chat` 和 `deepseek-reasoner`。
- **主机**：`api.deepseek.com`（全球）和 `api.deepseeki.com`（中国回落）。
- **配置目录**：`~/.deepseek/`。重命名它会使每个现有安装的已保存 API key、
  会话、skills、MCP 配置和审计日志失效。
- **GitHub 仓库 URL**：`https://github.com/Hmbown/CodeSmith`。旧的
  `Hmbown/DeepSeek-TUI` URL 在过渡期重定向到这里。
- **Homebrew tap 与 formula**（`Hmbown/homebrew-deepseek-tui`）：过渡期间
  仍以旧名称安装。tap 的 formula 将在后续跟进切换到新名称。
- **Docker 镜像**：`ghcr.io/hmbown/codesmith`。

## 弃用垫片（v0.8.x 期间）

为了让现有的 shell 别名、脚本和 CI 在改名期间继续工作，v0.8.41 及后续 v0.8.x
发布附带**弃用垫片（deprecation shim）**：

- 一个 `deepseek` 二进制，向 stderr 打印一行警告并把 argv 转发给
  `codesmith`。
- 一个 `deepseek-tui` 二进制，对 `codesmith-tui` 做同样的事。
- 一个没有 `bin` 的 `deepseek-tui@0.8.x` `npm` 包，其 postinstall 会打印
  清晰的改名通知。

这些垫片将在 **v0.9.0** 中移除。请在那之前完成迁移。

## 实际迁移

### npm

```bash
npm uninstall -g deepseek-tui
npm install -g codesmith
```

### Cargo

```bash
cargo uninstall deepseek-tui-cli deepseek-tui 2>/dev/null || true
cargo install codesmith-cli codesmith-tui --locked
```

或在本地检出中：

```bash
cargo install --path crates/cli --locked --force
cargo install --path crates/tui --locked --force
```

### Homebrew

过渡期间 tap formula 仍安装 `deepseek-tui`。现有的
`brew install deepseek-tui` 调用会继续工作，并在旧 formula 名称下安装新的
二进制。formula 和 tap 仓库将自行跟进改名。

### 手动 / GitHub Releases

`v0.8.41` 的 Releases 同时附上规范的 `codesmith-*` / `codesmith-tui-*` 资产
和旧版 `deepseek-*` / `deepseek-tui-*` 垫片资产。v0.8.40 上现有的
`deepseek update` 调用仍然可用；它们会落到弃用垫片上，随后提示安装
`codesmith`。

第二份校验和清单 `deepseek-artifacts-sha256.txt` 作为
`codesmith-artifacts-sha256.txt` 的别名附上，以便 v0.8.40 中硬编码的查找
仍然能通过校验。

## 为什么改名

对于同一个终端编程智能体和更长期的产品方向，CodeSmith 是一个更短、对终端
更友好的名字：一个面向开源与开放权重编程模型、以 DeepSeek 为先的智能体
终端。项目名、命令名、包名、发布资产、Docker 镜像和 CNB 镜像都迁至
CodeSmith；官方 DeepSeek provider、模型 ID、环境变量以及 `~/.deepseek/`
配置面仍保持一等地位。

## 报告改名相关问题

如果你的安装在迁移过程中损坏，请在
<https://github.com/Hmbown/CodeSmith/issues> 打开一个 issue，并附上：

- `codesmith --version` 的输出（如果你仍在垫片上，则用 `deepseek --version`）。
- 你使用的安装路径（npm、cargo、brew、手动）。
- 你运行的确切命令和完整的错误输出。

我们会优先处理迁移回归。
