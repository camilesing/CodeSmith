# Code Index

CodeSmith maintains a **persistent code index** per workspace so the agent
navigates large repositories without re-scanning them on every question.
Two model-visible tools ride on it:

- **`symbol_search`** — case-insensitive substring search over symbol
  definitions (functions, methods, structs, enums, traits, classes,
  interfaces, type aliases, constants, macros, modules), with optional
  `kind` and `file_glob` filters. Use it for "where is X defined?".
- **`find_references`** — definitions plus every lexical occurrence
  (imports, call sites, type usages) of an exact symbol name. Use it for
  "where is X used?".

Division of labor: the index answers definition/reference navigation;
`grep_files` stays the tool for arbitrary content matching ("which lines
contain this string/regex?"). On large repositories the indexed tools are
orders of magnitude faster than a full-tree grep.

Both tools report index freshness (`stale_files`) in their output so the
model knows how current the results are.

## Configuration (`[index]`)

Everything is optional — an absent table means *enabled* with the built-in
`tree-sitter` symbol backend for rust, python, javascript, typescript, and
go. Every capability is individually switchable:

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

Environment overrides (applied at resolution time, legacy aliases in
parentheses):

- `CODESMITH_INDEX_ENABLED` (`DEEPSEEK_INDEX_ENABLED`) — `true`/`false`
- `CODESMITH_INDEX_SYMBOLS_BACKEND` (`DEEPSEEK_INDEX_SYMBOLS_BACKEND`) —
  backend id

Unknown backend ids fail fast with a message listing the registered ids.

## How it works

- **Storage**: SQLite under `~/.codesmith/index/<workspace-hash>/index.db`.
  Nothing is written inside your repository. A schema-version mismatch
  drops and rebuilds the database automatically — the index is derived
  data and always self-heals.
- **Freshness**: lazy and incremental. Every query first diffs the
  workspace walk (respecting `.gitignore`) against the stored
  `mtime`+`size` per file; dirty files are re-parsed within a wall-clock
  budget (default 2s), deletions are purged, and whatever exceeds the
  budget is reported as `stale_files` while a low-priority background task
  finishes the job. There is no file watcher.
- **Extraction**: the built-in `tree-sitter` backend parses each file with
  its language grammar and extracts definitions plus lexical name
  occurrences. Files over 10 MB stay inventory-only.
- **References are lexical**: occurrences are name matches in code
  position (identifiers/type names), not fully-resolved symbols. A rare
  same-name symbol in an unrelated scope can appear; the tools say so in
  their descriptions so the model verifies by reading the listed
  locations.

## Pluggable backends

Backend selection follows the provider-registry pattern: implementations
register an `Arc<dyn IndexBackendFactory>` into an
`IndexBackendRegistry` and the TOML `backend = "…"` key selects one. The
built-ins are `tree-sitter` (feature-gated; the TUI enables it) and `none`
(a no-op placeholder that fails validation if selected for an enabled
capability). See `codesmith-index`'s crate docs for a worked example of
registering a custom backend from a downstream crate, and the design spec
at `docs/superpowers/specs/2026-08-19-code-index-design.md`.

## Current limitations

- **References are name-based** (lexical), not scope-resolved.
- **Worktrees**: the index is bound to the workspace root; entering a
  worktree keeps the main-workspace index (files under the worktree are
  not re-indexed in v1).
- **Background threads** (runtime threads) run without the index in v1;
  only the main session's turns get `symbol_search` / `find_references`.
- **Semantic search** (`[index.semantic]`) is a reserved seam: the trait,
  config section, and store placeholder exist, but no backend is compiled
  yet.
