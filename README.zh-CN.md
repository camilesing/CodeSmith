# CodeSmith


[English README](README.md)

````bash
➜  CodeSmith git:(main) ✗ codesmith
CodeSmith 0.9.0 (4fe017cfcc60) — GLM5.3 · AGENT mode · preset middle
workspace /Users/camile/Work/Rust/CodeSmith · /help lists commands (Tab completes, Shift+Tab cycles the mode) · ? shows the keyboard shortcuts · Ctrl+C cancels a turn
● idle │ middle │ 🧭 0% (48/200k) │ 🚥agent │ 🤖0 │ GLM5.3 │ 🧠high │ v0.9.0 (4fe017cfcc60)
❯ 这是什么项目？
这是 **CodeSmith** —— 一个面向开源权重模型的终端编码代理（terminal coding agent）。

核心定位：模型回答问题，代理完成任务
，CodeSmith 就是两者
之间的 harness。它是 CodeWhale 的分支，而 CodeWhale 又源自 deepseek
-tui，与 DeepSeek 公司无关联。

主要
特性：

- **成文宪法** —— 对相互冲突的权威做
排序（用户当前意图高于过时指令，验证高于自信）
- **审批门控
 + OS 沙箱** —— 会写入的工具调用需要
审批，执行环境被
沙箱隔离
- **回滚快照** —— 每一轮对话都留
有快照，编辑可回退
- **子代理** —— 最多
 20 个并发运行，支持并行工作
- **提供商中立** —— `rig-core` 只是自己
 `LlmClient`/`Tool` trait
背后可替换的一种实现，不绑定单一厂商

技术形态：
Rust 工作区，约 20 个 crate，分为**
框架组**（`protocol`、`tools`、`agent`、`config`、`secrets`、`extensions
`、`agent-runtime`）和**实现组**（`providers`、`tool-impls`、
`cli`、`app-server` 等）。框架 crate 在构建图上不允许
依赖实现组的 crate，由 `scripts/check-framework-deps.py` 在 CI 强制。

快速上手：

```bash
codesmith auth set --provider deepseek   # 或
 export CODESMITH_API_KEY
codesmith doctor                         # 检查配置与
连通性
codesmith                                # 交互式 REPL
```

注意：npm/Homebrew/Docker
等安装渠道还没上线
，目前需从源码构建（`cargo install --path crates/cli --locked`）。文档在 `docs/` 下 —— 入
门看 `GUIDE.md`，机制看 `HARNESS.md`
，crate 边界看 `ARCHITECTURE.md`。
● idle │ middle │ 🧭 8% (16k/200k) │ 🚥agent │ 🤖0 │ claude-sonnet-4-5 │ 🧠high │ v0.9.0 (4fe017cfcc60)
❯
````

## 安装

> [!WARNING]
> **安装渠道尚未就绪。** npm / Homebrew / Scoop / crates.io / Docker 的发布仍在准备中——这些命令（包括国内镜像方式）目前都不可用。在此之前请[从源码构建](docs/INSTALL_cn.md#7-从源码构建)：

```bash
git clone https://github.com/camilesing/CodeSmith.git && cd CodeSmith
cargo install --path crates/cli --locked   # 提供 `codesmith`
```

平台矩阵、国内镜像、校验和：[docs/INSTALL_cn.md](docs/INSTALL_cn.md)。

## 这是什么？

模型回答问题；智能体完成任务。CodeSmith 是两者之间的运行框架：一部宪法为每轮排出清晰的权威链（用户意图优先于陈旧指令，验证优先于自信），工具经审批门控并受操作系统级沙箱约束，每轮留下可回滚快照，最多 20 个子智能体并发运行。

完整原理：[docs/HARNESS.md](docs/HARNESS.md)。

## 快速开始

```bash
codesmith auth set --provider deepseek   # 或 export CODESMITH_API_KEY
codesmith doctor                         # 验证配置与连接
codesmith                                # 交互式 REPL
```

模式：**Plan**（只读）/ **Agent**（默认，带审批门控）/ **YOLO**（自动批准）——[docs/PRESETS_cn.md](docs/PRESETS_cn.md) · 其他提供商：[docs/PROVIDERS_cn.md](docs/PROVIDERS_cn.md)

## 文档

上手：[用户指南](docs/GUIDE_cn.md) · [预设与审批](docs/PRESETS_cn.md) · [技能](docs/SKILLS_cn.md) · [记忆](docs/MEMORY_cn.md) · [界面语言](docs/LOCALIZATION_cn.md) · [完整命令手册](docs/CLI.md)

配置：[配置参考](docs/CONFIGURATION_cn.md) · [提供商](docs/PROVIDERS_cn.md) · [安装](docs/INSTALL_cn.md) · [Docker](docs/DOCKER_cn.md)

原理与扩展：[运行框架](docs/HARNESS.md) · [架构](docs/ARCHITECTURE_cn.md) · [设计细节](docs/DESIGN_INTERNALS.md) · [沙箱](docs/SANDBOX_cn.md) · [子智能体](docs/SUBAGENTS_cn.md) · [MCP](docs/MCP_cn.md) · [钩子](docs/HOOKS_cn.md) · [运行时 API 与 Zed ACP](docs/RUNTIME_API.md) · [运维手册](docs/OPERATIONS_RUNBOOK_cn.md) · [发布流程](docs/RELEASE_RUNBOOK_cn.md) · [更新日志](CHANGELOG.md)

## 项目

CodeSmith 基于 [CodeWhale](https://github.com/Hmbown/CodeWhale)（其前身为 deepseek-tui）二次开发而来，感谢 Hunter Bown 与上游贡献者。

问题与 Bug：[提交 issue](https://github.com/camilesing/CodeSmith/issues/new/choose) · 参与贡献：[CONTRIBUTING.md](CONTRIBUTING.md) · 安全漏洞：[SECURITY.md](SECURITY.md)（勿公开开 issue）

> *本项目与 DeepSeek Inc. 无关。* · 许可证：[MIT](LICENSE)
