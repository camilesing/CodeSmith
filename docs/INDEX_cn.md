# 代码索引

CodeSmith 为每个工作区维护一个**持久化代码索引**，使 agent 无需在每次提问时重新扫描即可在大型仓库中导航。基于它提供了两个对模型可见的工具：

- **`symbol_search`** — 对符号定义（函数、方法、结构体、枚举、trait、类、接口、类型别名、常量、宏、模块）进行不区分大小写的子串搜索，支持可选的 `kind` 和 `file_glob` 过滤器。用于回答“X 在哪里定义？”。
- **`find_references`** — 返回精确符号名的定义及其所有词法出现（导入、调用点、类型使用）。用于回答“X 在哪里被使用？”。

分工：索引负责定义/引用导航；`grep_files` 仍然是任意内容匹配的工具（“哪些行包含这个字符串/正则？”）。在大型仓库中，基于索引的工具比全树 grep 快几个数量级。

两个工具都会在输出中报告索引新鲜度（`stale_files`），以便模型了解结果的时效性。

## 配置（`[index]`）

所有配置均为可选——不写该表即表示*启用*，并使用内置的 `tree-sitter` 符号后端，支持 rust、python、javascript、typescript 和 go。每项能力都可单独开关：

```toml
[index]
enabled = true              # master switch
refresh_budget_ms = 2000    # per-query incremental refresh budget

[index.files]               # file inventory cache (list_files surface)
enabled = true

[index.symbols]             # symbol index capability
enabled = true
backend = "tree-sitter"     # backend registry id

[index.symbols.languages]   # per-language switches, absent = enabled
rust = true
python = true
typescript = true
javascript = true
go = true

[index.semantic]            # reserved: embedding-based semantic search.
enabled = false             # No built-in backend yet — leave disabled.
backend = "none"
```

环境变量覆盖（在配置解析时应用，括号内为旧版别名）：

- `CODESMITH_INDEX_ENABLED`（`CODESMITH_INDEX_ENABLED`）— `true`/`false`
- `CODESMITH_INDEX_SYMBOLS_BACKEND`（`CODESMITH_INDEX_SYMBOLS_BACKEND`）—
  后端 id

未知的后端 id 会快速失败，并在错误消息中列出已注册的 id。

## 工作原理

- **存储**：SQLite，位于 `~/.codesmith/index/<workspace-hash>/index.db`。
  不会向你的仓库内写入任何内容。schema 版本不匹配时会自动删除并重建
  数据库——索引是派生数据，始终能自我修复。
- **新鲜度**：惰性且增量。每次查询先将工作区遍历（遵循 `.gitignore`）
  与每个文件已存储的 `mtime`+`size` 做对比；脏文件在限时预算（默认
  2 秒）内重新解析，删除项被清除，超出预算的部分以 `stale_files` 报告，
  同时由一个低优先级后台任务完成剩余工作。没有文件监视器。
- **提取**：内置 `tree-sitter` 后端用相应语言的语法解析每个文件，并提取
  定义和词法名称出现。超过 10 MB 的文件仅保留清单信息。
- **引用是词法级的**：出现位置是代码位置中的名称匹配（标识符/类型名），
  而不是完全解析后的符号。无关作用域中偶尔出现的同名符号也可能出现；
  工具已在描述中说明了这一点，模型可通过阅读列出的位置加以验证。

## 可插拔后端

后端选择遵循 provider 注册表模式：实现将 `Arc<dyn IndexBackendFactory>`
注册到 `IndexBackendRegistry` 中，再由 TOML 的 `backend = "…"` 键选择
其一。内置后端有 `tree-sitter`（受特性门控；TUI 会启用它）和 `none`
（空操作占位符；若为已启用的能力选择它会验证失败）。关于从下游 crate
注册自定义后端的完整示例，参见 `codesmith-index` 的 crate 文档；设计
规范位于 `docs/superpowers/specs/2026-08-19-code-index-design.md`。

## 当前限制

- **引用基于名称**（词法级），不做作用域解析。
- **Worktree**：索引绑定到工作区根目录；进入 worktree 时沿用主工作区的
  索引（v1 中不会重新索引 worktree 下的文件）。
- **后台线程**（运行时线程）在 v1 中不使用索引；只有主会话的回合可以使
  用 `symbol_search` / `find_references`。
- **语义搜索**（`[index.semantic]`）是预留的扩展点：trait、配置节和存储
  占位符已就位，但尚未编译任何后端。
