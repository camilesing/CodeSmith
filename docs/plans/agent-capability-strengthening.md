# Agent 核心能力补强

状态：已完成（2026-08-21 执行完毕）
来源：整体评审中"直接提升任务成功率"的四项缺口。相关方向见
[context-engineering.md](context-engineering.md)、
[product-polish.md](product-polish.md)。

## 背景与动机

评审发现的四个高频失败模式，均指向工具层能力缺口：

1. **无文件新鲜度追踪**：`edit_file` / `apply_patch` 编辑前不校验文件是否被读过、
   是否在读取后被外部修改。目前仅有压缩后的一句提醒
   (`crates/agent-runtime/src/compaction/attachment_reinject.rs:196`) 与提示词约束，
   模型仍可基于陈旧内容提交编辑，造成编辑冲突与幻觉编辑。
2. **无真实 tokenizer**：全链路使用 chars/3 启发式
   (`crates/agent-runtime/src/tools/large_output_router.rs:67` 的 `estimate_tokens`)，
   压缩触发、容量预检、缓存对齐预算全是近似值，中文内容偏差更大。
3. **grep 非 ripgrep**：`grep_files` (`crates/tool-impls/src/tools/search.rs`) 是
   纯 Rust 正则逐文件扫描，无多行模式，大仓库性能与覆盖率受限。
4. **`edit_file` 多匹配时静默全量替换**：`crates/tui/src/tools/file.rs:647` 直接
   `contents.replace(...)`，多处匹配时仅事后告警，无法指定替换第 N 处。

## Stage 1 — `edit_file` 精确替换（`replace_all` / `occurrence`）

文件：`crates/tui/src/tools/file.rs`

- 新增可选参数 `replace_all: bool` 与 `occurrence: integer`（1-based）。
- 行为变更：默认（两者都未传）且精确匹配多于 1 处时**报错**，提示改用
  `replace_all` 或 `occurrence`，不再静默全量替换。
- `replace_all=true` 全量替换；`occurrence=N` 只替换第 N 处，越界报错。
- 模糊匹配路径（indentation / punctuation 归一化）保持现状——本就要求唯一匹配。
- 验收：单匹配成功、多匹配默认报错、`replace_all` 全替换、`occurrence` 正常与
  越界、模糊路径回归，全部有单元测试。

## Stage 2 — 文件新鲜度校验（read-before-edit）

方案：工具包装器（`ToolSpec` 委托），不改 `ToolContext`——其构造点遍布生产与
测试代码，改动面过大。

- 新模块 `crates/agent-runtime/src/tools/freshness.rs`：
  - `FileFreshnessTracker`：`Arc<Mutex<HashMap<PathBuf, FileState>>>`，
    `FileState` 记录 last-read 时的 mtime + len。
  - `record_read(path)`：读取成功后记录。
  - `validate(path)`：编辑前校验——从未读过 → 报错引导先 `read_file`；
    读过但 mtime/len 变化 → 报错提示文件已被外部修改、需重读。
  - `record_write(path)`：写成功后更新记录，同一工具链内连续编辑不误伤。
- `FreshnessWrappedTool`：包装 `read_file`（成功后 record_read）、
  `edit_file` / `write_file`（已存在文件）/ `fim_edit`（执行前 validate、
  成功后 record_write）、`apply_patch`（解析 V4A patch 头
  `*** (Update|Add|Delete) File: <path>` 提取全部目标路径逐一校验）。
- 接线：`crates/tui/src/core/engine/tool_setup.rs` 构造 per-engine tracker
  （跨 turn 共享，经 EngineConfig 传递）并包装注册相应工具。
- 功能开关 `[features].file_freshness`，**默认开启**；包装器在 execute 时读
  `context.features` 运行时短路。Yolo 模式不豁免——这是正确性护栏而非安全限制。
- 验收：tracker 单元测试（未读 / 已读未变 / 外部修改 / 写后连续编辑）、
  包装器工具级测试、feature 关闭时直通。

## Stage 3 — `grep_files` 换用 ripgrep 引擎

文件：`crates/tool-impls/src/tools/search.rs`

- workspace 与 tool-impls 增加 `grep-regex` + `grep-searcher` 依赖
  （`ignore` 0.4 已是 workspace 依赖）。
- 用 `ignore::WalkBuilder` + `grep_regex::RegexMatcher` +
  `grep_searcher::Searcher` 重写匹配内核，**接口与输出格式不变**
  （include/exclude、context_lines、case_insensitive、max_results=100、
  10MB 文件上限、30s 超时）。
- 新增可选参数 `multiline: bool`（默认 false）。
- 验收：现有 grep 测试全部通过 + 新增 multiline 用例。

## Stage 4 — 集中式 TokenCounter（真实 tokenizer）

- 新模块 `crates/agent-runtime/src/tokenizer.rs`：
  - `TokenCounter::Heuristic`（chars/3，兜底）| `TokenCounter::Hf(tokenizers::Tokenizer)`。
  - agent-runtime 增加可选依赖 `tokenizers`（default-features=false）+
    feature `hf-tokenizer`；tui 默认启用。
  - 加载顺序：`[context].tokenizer_path` 指定的 tokenizer.json →
    加载失败或未配置则 Heuristic 兜底。不联网下载；文档给出从 HuggingFace
    获取 DeepSeek tokenizer.json 的命令。
- 替换调用点（无 tokenizer 时行为与现状完全一致）：
  `tools/large_output_router.rs` 的 `estimate_tokens`、`compaction/` 预算计算、
  `tools/truncate.rs` 阈值判断。
- 验收：加载失败降级测试、Heuristic 回归测试；配置项写入
  `config.example.toml` 与 `docs/CONFIGURATION.md`。

## 阶段状态

| Stage | 内容 | 状态 |
| --- | --- | --- |
| 1 | edit_file replace_all/occurrence | ✅ 完成（多匹配默认报错 + replace_all + occurrence；`crates/tui/src/tools/file.rs`） |
| 2 | 文件新鲜度校验 | ✅ 完成（`agent-runtime/src/tools/freshness.rs` + 包装器 + `[features].file_freshness` 默认开） |
| 3 | ripgrep 化 grep_files | ✅ 完成（`ignore` walker + `grep-regex` 引擎 + `multiline` 参数；.gitignore 真正生效） |
| 4 | TokenCounter | ✅ 完成（`agent-runtime/src/tokenizer.rs`，feature `hf-tokenizer`，`[context].tokenizer_path`；compaction/large-output 路由已接线） |
| 5 | fmt / clippy -D warnings / workspace test 全量验证 | ✅ 完成 |

### 执行备注（与原计划的偏差）

- Stage 3 未引入 `grep-searcher`：其 sink 的上下文行关联机制会显著增加复杂度，
  现有逐行上下文切片已充分测试；实际落地为 `ignore` walker（目录遍历 + gitignore）
  + `grep-regex`/`grep-matcher`（匹配引擎 + multiline）。
- Stage 4 的 TokenCounter 采用进程级 `OnceLock`（`tokenizer::set_default`）而非
  EngineConfig 字段：compaction 的估算函数是无数调用的自由函数，穿参改造面过大；
  单进程单计数器符合实际部署形态。容量控制器自适应阈值留在
  [context-engineering.md](context-engineering.md) §3。
