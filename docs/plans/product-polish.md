# 产品补全计划

状态：规划
定位：面向使用者可感知的能力缺口，见
[agent-capability-strengthening.md](agent-capability-strengthening.md)（工具层）
与 [context-engineering.md](context-engineering.md)（上下文层）。

## 1. 原生多模态输入（Image ContentBlock）

现状：`crates/agent/src/models.rs:74` 的消息模型无 Image 变体；视觉走独立的
`image_analyze` 工具（另配 vision model）+ `read_file` 的 OCR 提取。截图调试、
UI 还原类任务需模型先调工具再"转述"，信息损耗明显。

方案：

1. wire 模型增加 Image content block（source: base64 / file path），
   OpenAI-compatible `image_url` 格式序列化（rig 适配层透传）。
2. `/attach` 与 composer 拖放直接把图片挂到用户消息（保留现有文本引用模式为
   fallback，供不支持视觉的模型使用）。
3. provider 能力声明：`[providers.*]` 增加 `vision = true|false`；
   无视觉能力的模型收到图片时自动降级为 OCR + image_analyze 现有链路。
4. 验收：粘贴截图 → 直接随消息发送 → 模型在回复中引用图中细节（deepseek-vl
   或配置了 vision 的 provider）。

## 2. Git commit / PR 专用工具

现状：提交/推送只能 `exec_shell` shell-out（git 环境变量经 `tools/git_env.rs`
清洗）；GitHub 侧只有读工具 + `github_close_*`，无 PR 创建。审查审批也无法
看到结构化的 commit 意图。

方案：

1. `git_commit` 工具：接收 message + 可选 pathspec，内部走已清洗的 git 环境，
   pre-commit 校验（diff 统计回显），生成 conventional-commit 提示。
2. `git_push` / `create_pr`（走 `gh` CLI，与 `github_*` 工具同一证据模式：
   approval-gated + evidence-required）。
3. approval UI 展示结构化 diffstat 而非裸命令行。
4. 验收：一次"提交并发 PR"全程不落 exec_shell；审计日志记录 PR 元数据。

## 3. Notebook（.ipynb）支持

现状：全仓库无 notebook 相关代码。

方案：起步只做读写工具（`read_notebook` 按 cell 分块 + 行号稳定化、
`edit_notebook` 按 cell index 编辑），执行复用现有 RLM Python 会话
（`crates/agent-runtime/src/rlm/`）。不做完整 Jupyter 内核对接。

## 4. 后台 shell 任务重连

现状：重启后 live shell jobs 标记 stale（TOOL_SURFACE.md 已文档化）。

方案：任务元数据（cwd、命令、PTM/输出日志路径）已持久化的话，重启后提供
`exec_reattach`：只读 tail 输出 + 可选继续等待退出码；真正的进程 stdin 重连
仅在 PTY 日志仍被同机进程持有时可用。验收：`/jobs` 中 stale 任务可 tail。

## 5. 配置面精简

现状：`config.example.toml` 53KB，新用户认知负担大。

方案：

1. 分层为 `config.minimal.example.toml`（providers + mode + 一个 profile，
  < 100 行）与完整版；README 指向精简版起步。
2. `[features]` 默认值表单独一节，与 `docs/CONFIGURATION.md` 互链。
3. 启动时对"配置了但无效果"的键（如与当前 provider 不匹配的键）给一次性
   warning（config validate 已有基础）。

## 6. Provider 分层提示词

现状：系统提示词（`base.md` + 模式覆盖）深度绑定 DeepSeek V4 特性
（1M 窗口、prefix-cache 经济学、thinking 预算）；架构上 provider 可插拔，
但换 provider 后这些指引失真（例如对无 thinking 的模型讲 thinking 预算）。

方案：提示词组装器（`crates/agent-runtime/src/prompts.rs` 已分层：
taxonomy → base → personality → mode → approval）增加 provider 能力层——
按 provider 声明（context window、thinking、cache、vision）裁剪对应段落，
而非整篇 V4 专属指引。验收：mock provider 关闭 thinking 声明后，
系统提示中不再出现 thinking 预算段落。

## 7. 杂项

- `finance` 工具与 coding 主场景无关，考虑移出默认目录（特性开关）。
- TUI `is_simple` "caveman" 风格刚落地，观察一段时间后决定是否作为
  弱终端默认。
- `docs/GUIDE.md` 与 50 个 slash command 的帮助文本做一次交叉校对。
