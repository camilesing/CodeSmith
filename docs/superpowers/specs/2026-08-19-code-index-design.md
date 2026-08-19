# Code Index System — Design

Date: 2026-08-19
Status: approved (plan review 2026-08-19)
Branch: `feat/code-index`

## Problem

On large repositories the agent locates code exclusively through
`grep_files` (hand-written recursive walk + `regex`, full scan per call) and
`file_search` (`ignore` walk + fuzzy name match). Both are stateless: every
query re-walks the tree, and symbol navigation (where is this defined? who
calls this?) degenerates into repeated content greps. This design adds a
**persistent per-workspace code index** with a pluggable backend seam.

Scope decisions (confirmed with maintainer):

- Capabilities: **symbol index** (tree-sitter), **file inventory cache**,
  and a **reserved semantic interface** (no implementation this cycle).
  No FTS content index.
- Plugin form: **registry + TOML selection** (mirrors `ProviderRegistry`).
  No dylib / subprocess / MCP extension for backends in this cycle.
- Storage: SQLite under `~/.codesmith/index/<workspace-hash>/`.
- Freshness: **lazy incremental validation** (mtime+size diff on query),
  no file watcher.

## Architecture

```
config.toml [index] ──→ tui Config (parse / merge / env / validate)
                              │
                              ▼
              IndexBackendRegistry (mirrors ProviderRegistry:
                upsert / resolve, Arc<dyn IndexBackendFactory>)
                              │
                              ▼
                IndexService (orchestration)      SQLite Store
                lazy refresh + query API     ~/.codesmith/index/<ws-hash>/index.db
                              │
                              ▼
        ToolContext.index_service: Option<Arc<dyn IndexServiceApi>>
        (mirrors the lsp_manager injection pattern; None = disabled/test)
                              │
                              ▼
        Tools: symbol_search / find_references (crates/tool-impls)
```

### Layering

New crate `crates/index` (`codesmith-index`), following the
`codesmith-providers` convention: abstraction and built-in implementations
live in one crate; heavy dependencies are feature-gated so they stay out of
the agent-runtime kernel.

- default features: types, traits, registry, config, SQLite store,
  `ignore`-based walking — light, no grammar deps.
- feature `tree-sitter`: grammar crates (rust, python, javascript,
  typescript, go to start) + the built-in tree-sitter backend. Enabled by
  the host binary (`crates/tui`), not by `agent-runtime`.
- When the selected backend was not compiled in, the registry holds an
  `UncompiledBackendFactory` stub whose `build()` fails with a message
  naming the missing feature — same pattern as `codesmith-providers`.

Dependency additions to `[workspace.dependencies]`: `tree-sitter` +
per-language grammar crates (versions pinned at implementation time).
`rusqlite` and `ignore` reuse existing workspace versions.

## Core Abstractions

Three traits + one registry, each mirroring an established house pattern.

```rust
/// Plugin seam — mirrors ProviderFactory (crates/agent/src/provider/mod.rs).
pub trait IndexBackendFactory: Send + Sync {
    fn id(&self) -> &str;                                  // "tree-sitter" | "none" | extension
    fn capabilities(&self) -> &'static [IndexCapability];  // Symbols / Semantic
    fn build(&self, cfg: &IndexBackendConfig) -> Result<Arc<dyn IndexBackend>>;
}

/// Index backend: single-file extraction. The orchestration layer reads the
/// source; backends stay IO-free.
pub trait IndexBackend: Send + Sync {
    fn supported_languages(&self) -> &[Language];
    fn extract(&self, file: &Path, source: &str, lang: Language) -> Result<Extraction>;
}
// Extraction { symbols: Vec<Symbol>, occurrences: Vec<Occurrence> }

/// Query / management surface — injected into ToolContext, mirrors LspManagerApi.
#[async_trait]
pub trait IndexServiceApi: Send + Sync {
    async fn search_symbols(&self, q: &SymbolQuery) -> Result<Vec<Symbol>>;
    async fn find_definition(&self, name: &str) -> Result<Vec<Symbol>>;
    async fn find_references(&self, name: &str) -> Result<Vec<Occurrence>>;
    async fn list_files(&self, q: &FileQuery) -> Result<Vec<FileEntry>>;
    async fn refresh(&self, budget: RefreshBudget) -> Result<IndexStats>;
    fn stats(&self) -> IndexStats;
}

/// Mirrors ProviderRegistry: upsert semantics, resolve by id,
/// build() error names the registered ids.
pub struct IndexBackendRegistry { /* HashMap<String, Arc<dyn IndexBackendFactory>> */ }

/// Reserved semantic seam — trait + config section + store placeholder only.
#[async_trait]
pub trait SemanticIndexApi: Send + Sync { /* upsert / search */ }
```

Value types (workspace-relative paths everywhere):

- `Symbol { name, kind, container, location, signature }`
- `SymbolKind`: Function / Method / Struct / Enum / Trait / Class /
  Interface / TypeAlias / Constant / Macro / Module
- `Occurrence { name, role: Definition | Reference, location }`
- `FileEntry { path, mtime_ms, size, language }`
- `IndexStats { files, symbols, stale_files, last_refresh, backend }`

References are **name-based lexical occurrences** in this cycle (no full
semantic resolution); tool descriptions state that boundary explicitly.

## Configuration

Follows the tagged-enum / closed-enum-with-alias house idioms. Every
capability is individually switchable.

```toml
[index]
enabled = true                    # master switch (default true)
refresh_budget_ms = 2000          # incremental refresh budget per query

[index.files]                     # file inventory cache (built-in, no backend choice)
enabled = true

[index.symbols]
enabled = true
backend = "tree-sitter"           # registry id; unknown value fails validation
[index.symbols.languages]         # per-language switches
rust = true
python = true
typescript = true
javascript = true
go = true

[index.semantic]                  # reserved: enabled=true with backend="none"
enabled = false                   # fails validation with guidance
backend = "none"
```

- Config structs live in `codesmith-index::config`; `agent-runtime` reaches
  them through its existing dependency.
- TUI `Config` gains an `[index]` section: `merge_config` branch, env
  overrides (`CODESMITH_INDEX_ENABLED`, `CODESMITH_INDEX_SYMBOLS_BACKEND`,
  with legacy `DEEPSEEK_*` aliases per the ef9d70a3 convention), and
  `validate()` fast-fail for unknown backends (listing registered ids) and
  inconsistent semantic settings.

## Storage

SQLite per workspace: `~/.codesmith/index/<ws-hash>/index.db`, where
`ws-hash` is the first 16 hex chars of SHA-256 over the canonical
workspace path.

```sql
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
-- schema_version, workspace_path, created_at, backend ids
CREATE TABLE files(
  id INTEGER PRIMARY KEY, path TEXT UNIQUE, mtime_ms INTEGER, size INTEGER,
  language TEXT, symbol_count INTEGER
);
CREATE TABLE symbols(
  id INTEGER PRIMARY KEY,
  file_id INTEGER REFERENCES files(id) ON DELETE CASCADE,
  name TEXT, kind TEXT, container TEXT,
  line INTEGER, col INTEGER, end_line INTEGER, end_col INTEGER,
  signature TEXT
);
CREATE INDEX idx_symbols_name ON symbols(name);
CREATE INDEX idx_symbols_kind ON symbols(kind);
CREATE TABLE occurrences(
  id INTEGER PRIMARY KEY,
  file_id INTEGER REFERENCES files(id) ON DELETE CASCADE,
  name TEXT, role TEXT, line INTEGER, col INTEGER
);
CREATE INDEX idx_occurrences_name ON occurrences(name);
-- reserved: embeddings(file_id, chunk, vector BLOB) for a future semantic backend
```

**Schema version mismatch → drop and rebuild.** The index is derived data;
self-healing replaces migration. mtimes are validated by mtime+size only
(no content hashing); scenarios where git preserves mtimes across checkouts
are covered by a manual rebuild command rather than global re-hashing.

## Lazy Incremental Freshness

Every tool query first runs `refresh(budget)`:

1. `ignore::WalkBuilder` metadata walk (respects `.gitignore` — fixing
   grep_files' hand-rolled exclusion table drift).
2. Diff walk results against the `files` table on path/mtime/size.
3. Dirty / new files are re-extracted on `spawn_blocking`; deleted files
   are purged (cascades to symbols/occurrences).
4. Files beyond the budget are counted in `stale_files` and reported in
   query-result metadata so the model can perceive freshness.
5. First build on a large repository continues at low priority in the
   background; queries return the already-fresh portion immediately.

## Agent Integration

- `ToolContext.index_service: Option<Arc<dyn IndexServiceApi>>`
  (default `None`, lsp_manager-style doc comment; test contexts skip it).
- Host assembly (tui): at session start, build the `IndexService` from the
  registry per config; an **IndexManager keyed by workspace root**
  (`HashMap<PathBuf, Arc<IndexServiceApi>>`) selects the right index after
  `enter_worktree`; the turn dispatcher injects it into each ToolContext.
  When disabled, the tools are simply not registered (catalog stability
  within a session preserves KV prefix cache).
- Two new tools in `crates/tool-impls` (standard 7-step registration flow):
  - `symbol_search` — required case-insensitive substring `query`,
    optional `kind` / `file_glob` / `limit` (default 50). Returns matching
    symbol definitions.
  - `find_references` — `name` → definitions + occurrences, grouped by
    file.
  - Both: `ReadOnly` + `Sandboxable` capabilities, `spawn_blocking` +
    timeout + `cancel_token`. Added to `DEFAULT_ACTIVE_NATIVE_TOOLS`
    (`crates/agent-runtime/src/tools/tool_catalog.rs`); descriptions draw
    the division of labor against `grep_files` (definition/reference
    navigation vs arbitrary content matching) so models prefer the index
    on large repos.

## Implementation Phases

Each phase lands as an independent commit with tests.

- **Phase 0** — this design doc.
- **Phase 1** — `crates/index` skeleton: types / traits / registry /
  config / store, unit tests (registry upsert/resolve/error listing, store
  CRUD roundtrip, version-mismatch rebuild, TOML parsing incl. invalid
  values).
- **Phase 2** — built-ins & orchestration: file inventory walk,
  tree-sitter backend (feature-gated), `none` stub, `IndexService`
  (incremental correctness: editing one file re-parses only that file,
  deletion purges, budget truncation reports accurate stale counts).
- **Phase 3** — agent integration: ToolContext field, both tools,
  registry builder + tool_setup wiring + default-active list; tool tests
  against a stub service.
- **Phase 4** — config pipeline & host assembly: TUI Config/env/validate,
  `default_index_registry()` + uncompiled stubs, IndexManager injection,
  `config.example.toml` `[index]` block; e2e smoke (fixture repo →
  symbol_search hits definitions).
- **Phase 5** — docs: `docs/INDEX.md` user doc, ARCHITECTURE.md / README
  additions; optional `/index status|rebuild` TUI command.

## Verification

Per phase: `cargo test -p` the touched crates + `cargo clippy`; commit
messages cite actual test counts (house convention). After Phase 4,
dogfood on the CodeSmith repository itself.
