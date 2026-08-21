# 本地化矩阵

状态日期：2026-04-29

本文档只跟踪 UI 本地化。它不改变模型输出语言、provider 行为或 DeepSeek 负载支持。除非另行添加原生媒体负载支持，媒体附件仍保持为本地路径文本引用。

## 来源审计

v0.7.6 的对齐（parity）检查使用了实时的 GitHub 源和 `/opt/homebrew/bin/gh`。

| 项目 | 引用 | 证据 | 结果 |
|---|---:|---|---|
| Codex CLI | `openai/codex@df966996a75333add031fca47b72655e9ee504fd` | `gh repo view openai/codex`；针对 `locale`、`i18n`、`l10n`、`translation`、`messages` 的递归代码树扫描；README 语言扫描 | 在审计的代码树中未发现已提交（checked-in）的 CLI UI 本地化注册表。应将 Codex CLI 的对齐视为英语优先的终端 UI 行为，而不是已发布 locale 标签的来源。 |
| opencode | `anomalyco/opencode@00bb9836a60f1dcdd0ce5078b05d12f749fdde66` | `packages/console/app/src/lib/language.ts`、`packages/app/src/context/language.tsx`、`packages/web/src/i18n/locales.ts`、`packages/app/src/i18n/parity.test.ts` | opencode 提供了应用/文档 locale 基础设施，包括语言检测、locale 标签、文档 locale 别名、阿拉伯语的 RTL 方向，以及针对特定键的对齐测试。 |

## v0.7.6 已发布的核心语言包

以下 locale 受 `settings.toml` 中的 `locale` 以及 `LANG` / `LC_ALL` 自动检测支持。

| Locale | 显示名称 | 文字 | 方向 | 回落 | 优先级 | v0.7.6 范围 | 备注 |
|---|---|---|---|---|---|---|---|
| `en` | 英语 | Latin | LTR | `en` | 基线 | 源字符串仍是权威来源。 | 英语始终可用。 |
| `ja` | 日语 | Jpan | LTR | `en` | v0.7.6 必备 | 核心 TUI 框架文案 | 覆盖 composer 占位符/历史搜索、帮助框架文案以及 `/config` 框架文案。 |
| `zh-Hans` | 简体中文 | Hans | LTR | `en` | v0.7.6 必备 | 核心 TUI 框架文案 | `zh`、`zh-CN` 和 `zh-Hans` 都解析到这里。不提供繁体中文。 |
| `pt-BR` | 葡萄牙语（巴西） | Latin | LTR | `en` | v0.7.6 必备 | 核心 TUI 框架文案 | `pt` 和 `pt-PT` 目前回落到巴西葡萄牙语；不单独提供欧洲葡萄牙语。 |
| `es-419` | 西班牙语（拉丁美洲） | Latin | LTR | `en` | v0.7.6 必备 | 核心 TUI 框架文案 | `es` 及区域变体都解析到这里。 |
| `vi` | 越南语 | Latin | LTR | `en` | v0.7.6 必备 | 核心 TUI 框架文案 | 界面框架文案已完整翻译，并通过自动化宽度测试。 |

选择方式：

```toml
locale = "auto"     # default; checks LC_ALL, LC_MESSAGES, then LANG
locale = "ja"
locale = "zh-Hans"
locale = "pt-BR"
locale = "es-419"
locale = "vi"
```

回落行为：

- 缺失或不支持的已配置 locale 会回落到英语。
- 当没有检测到受支持的环境 locale 时，`auto` 回落到英语。
- 解析出的 locale 会作为 V4 推理与回复的回落自然语言写入系统提示词。最新的
  用户消息优先级更高（包括 `reasoning_content`），因此即使解析出的 locale 是
  英语，中文对话轮也应保持中文。

## 计划中的全球南方 QA 矩阵

除非后续变更补齐了完整的消息覆盖和 QA 证据，否则这些不作为 v0.7.6 已发布的翻译来宣称。

| Locale | 显示名称 | 文字 | 方向 | 优先级 | 覆盖状态 | 回落 | QA 状态 | 布局风险 |
|---|---|---|---|---|---|---|---|---|
| `ar` | 阿拉伯语 | Arab | RTL | 后续跟进 | 已计划 | `en` | 仅自动化渲染器样例；正式发布前需母语审校 | RTL 排序、标点、按键组合混排 |
| `hi` | 印地语 | Deva | LTR | 后续跟进 | 已计划 | `en` | 仅自动化渲染器样例；正式发布前最好有母语审校 | 组合附加符号、光标宽度、截断 |
| `bn` | 孟加拉语 | Beng | LTR | 后续跟进 | 已计划 | `en` | 仅纳入矩阵；正式发布前需母语审校 | 组合附加符号、换行 |
| `id` | 印尼语 | Latin | LTR | 后续跟进 | 已计划 | `en` | 仅纳入矩阵；需自动化窄宽度快照和审校通过 | 标签比英文更长 |
| `sw` | 斯瓦希里语 | Latin | LTR | 后续跟进 | 已计划 | `en` | 仅纳入矩阵；正式发布前需母语或流利审校 | 翻译质量、命令描述更长 |
| `ha` | 豪萨语 | Latin | LTR | 后续跟进 | 已计划 | `en` | 仅纳入矩阵；正式发布前需母语或流利审校 | 变音符号与术语 |
| `yo` | 约鲁巴语 | Latin | LTR | 后续跟进 | 已计划 | `en` | 仅纳入矩阵；正式发布前需母语或流利审校 | 声调符号与术语 |
| `fil` | 菲律宾语/他加禄语 | Latin | LTR | 后续跟进 | 已计划 | `en` | 仅纳入矩阵；正式发布前需先补齐源字符串 | 术语一致性 |
| `fr` | 法语 | Latin | LTR | 后续跟进 | 已计划 | `en` | 仅纳入矩阵；正式发布前需审校通过 | 非洲各 locale 术语存在差异 |

## 消息覆盖

注册表的第一轮覆盖了高可见度终端框架文案的稳定消息 ID：

- composer 占位符
- composer 历史搜索的标题、占位符、提示与无结果状态
- `/config` 的标题、过滤器占位符、无结果状态、过滤结果计数与底部提示
- 帮助浮层的标题、过滤器占位符、无结果状态、分区标签与底部提示

v0.7.6 尚未翻译的部分：

- 模型/系统提示词与个性化设定
- provider 或工具 schema
- 完整的斜杠命令描述以及所有状态/提示（toast）/错误路径
- 本配置说明之外的 README/文档内容

## 译者备注

除非后续术语表明确变更，以下技术术语保持不变：`Plan`、`Agent`、`YOLO`、`/config`、`/mcp`、`@path`、`/attach`、`DeepSeek`、`MCP`、`CLI`、`TUI`，以及 `Enter`、`Esc`、`Tab`、`PgUp`、`PgDn` 等按键组合。

## QA 检查清单

在把计划中的 locale 提升为已发布状态之前：

1. 在 `crates/tui/src/localization.rs` 中补齐完整的消息覆盖。
2. 添加 locale 解析测试与缺失键测试。
3. 至少为 composer、帮助和 `/config` 添加窄宽度渲染覆盖。
4. 验证 CJK 宽度、RTL 标点、组合附加符号与截断行为。
5. 记录母语/流利审校状态，或将该 locale 标记为仅经自动化 QA。
