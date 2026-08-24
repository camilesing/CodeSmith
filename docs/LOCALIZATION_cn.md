# 本地化矩阵

状态日期：2026-08-24

本文档只跟踪 UI 本地化。它不改变模型输出语言或 provider 行为。除非另行添加原生媒体负载支持，媒体附件仍保持为本地路径文本引用。

## 来源审计

v0.7.6 的对齐（parity）检查使用了实时的 GitHub 源和 `/opt/homebrew/bin/gh`。

| 项目 | 引用 | 证据 | 结果 |
|---|---:|---|---|
| Codex CLI | `openai/codex@df966996a75333add031fca47b72655e9ee504fd` | `gh repo view openai/codex`；针对 `locale`、`i18n`、`l10n`、`translation`、`messages` 的递归代码树扫描；README 语言扫描 | 在审计的代码树中未发现已提交（checked-in）的 CLI UI 本地化注册表。应将 Codex CLI 的对齐视为英语优先的终端 UI 行为，而不是已发布 locale 标签的来源。 |
| opencode | `anomalyco/opencode@00bb9836a60f1dcdd0ce5078b05d12f749fdde66` | `packages/console/app/src/lib/language.ts`、`packages/app/src/context/language.tsx`、`packages/web/src/i18n/locales.ts`、`packages/app/src/i18n/parity.test.ts` | opencode 提供了应用/文档 locale 基础设施，包括语言检测、locale 标签、文档 locale 别名、阿拉伯语的 RTL 方向，以及针对特定键的对齐测试。 |

## 已发布的 locale

以下 locale 受 `settings.toml` 中的 `locale` 以及 `LANG` / `LC_ALL` 自动检测支持。语言支持已于 2026-08-24 收缩为此集合；此前发布的 `ja`、`pt-BR`、`vi` 回落到英语，计划中的全球南方 QA 矩阵已放弃。

| Locale | 显示名称 | 文字 | 方向 | 回落 | 审校状态 | 备注 |
|---|---|---|---|---|---|---|
| `en` | 英语 | Latin | LTR | `en` | 源字符串仍是权威来源。 | 英语始终可用。 |
| `zh-Hans` | 简体中文 | Hans | LTR | `en` | 已母语审校。 | `zh`、`zh-CN` 和 `zh-Hans` 都解析到这里。核心 TUI 框架文案及提示词侧的强化书挡。 |
| `zh-Hant` | 繁体中文 | Hant | LTR | `zh-Hans` | 已母语审校。 | `zh-TW`、`zh-HK`、`zh-MO` 和 `zh-Hant` 都解析到这里；除差异字符串外与简体共享翻译表。 |
| `hi` | 印地语 | Deva | LTR | `en` | 仅自动化 QA；建议母语审校。 | 界面框架文案全覆盖；窄宽度与截断测试覆盖天城文。含提示词侧强化书挡。 |
| `es-419` | 西班牙语（拉丁美洲） | Latin | LTR | `en` | 仅自动化 QA；建议母语审校。 | `es` 及区域变体都解析到这里。含提示词侧强化书挡。 |

选择方式：

```toml
locale = "auto"     # 默认；依次检查 LC_ALL、LC_MESSAGES、LANG
locale = "en"
locale = "zh-Hans"
locale = "zh-Hant"
locale = "hi"
locale = "es-419"
```

回落行为：

- 缺失或不支持的已配置 locale 会回落到英语。
- 当没有检测到受支持的环境 locale 时，`auto` 回落到英语。
- 解析出的 locale 会作为 V4 推理与回复的回落自然语言写入系统提示词。最新的
  用户消息优先级更高（包括 `reasoning_content`），因此即使解析出的 locale 是
  英语，中文对话轮也应保持中文。
- 对 `zh-Hans`、`hi` 和 `es-419`，系统提示词额外携带原生文字的强化书挡
  （前导 + 收尾），将 `reasoning_content` 与最终回复引导到该 locale 的语言；
  见 `crates/agent-runtime/src/prompts.rs`。

## 消息覆盖

注册表覆盖高可见度终端框架文案的稳定消息 ID：

- composer 占位符
- composer 历史搜索的标题、占位符、提示与无结果状态
- `/config` 的标题、过滤器占位符、无结果状态、过滤结果计数与底部提示
- 帮助浮层的标题、过滤器占位符、无结果状态、分区标签与底部提示
- 斜杠命令描述、按键绑定标签、onboarding 屏幕与右键菜单

尚未翻译的部分：

- 模型/系统提示词与个性化设定
- provider 或工具 schema
- 注册表之外的完整斜杠命令描述以及所有状态/提示（toast）/错误路径
- 本配置说明之外的 README/文档内容

## 译者备注

除非后续术语表明确变更，以下技术术语保持不变：`Plan`、`Agent`、`YOLO`、`/config`、`/mcp`、`@path`、`/attach`、`DeepSeek`、`MCP`、`CLI`、`TUI`，以及 `Enter`、`Esc`、`Tab`、`PgUp`、`PgDn` 等按键组合。

## QA 检查清单

在发布新 locale 之前：

1. 在 `crates/tui/src/localization.rs` 中补齐完整的消息覆盖。
2. 添加 locale 解析测试与缺失键测试。
3. 至少为 composer、帮助和 `/config` 添加窄宽度渲染覆盖。
4. 验证 CJK 宽度、组合附加符号与截断行为。
5. 记录母语/流利审校状态，或将该 locale 标记为仅经自动化 QA。
