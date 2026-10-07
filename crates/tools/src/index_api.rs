//! Framework-side definition of the per-workspace index seam.
//!
//! [`IndexServiceApi`] — plus its value vocabulary ([`Symbol`], [`FileEntry`],
//! queries, budgets) and the reserved [`SemanticIndexApi`] — is the
//! query/management surface the kernel injects into the tool context. The
//! definition lives in the framework layer so `codesmith-agent-runtime`
//! consumes the trait without depending on any index implementation crate,
//! the same seam shape as `LlmClient` (defined in `codesmith-agent`,
//! implemented in `codesmith-providers`). `codesmith-index` is the provider:
//! it implements the traits and re-exports every item from this module, so
//! existing `codesmith_index::…` paths keep resolving.
//!
//! Moved from `codesmith-index` (`types.rs` and the service traits of
//! `backend.rs`); this module does not include the backend SPI
//! (`IndexBackend` / `IndexBackendFactory` / `Extraction`) — backend
//! implementers are implementation-side and depend on `codesmith-index`
//! directly.
//!
//! All paths stored or returned by the index are **workspace-relative** with
//! forward slashes, so the store stays portable across hosts and the
//! agent-facing tools can render stable paths.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Default result cap for symbol searches (`symbol_search` `limit`).
pub const DEFAULT_SYMBOL_LIMIT: usize = 50;

/// Default result cap for file listings (`list_files` `limit`).
pub const DEFAULT_FILE_LIMIT: usize = 50;

/// Programming languages the index knows how to extract symbols from.
///
/// The set intentionally mirrors the grammars compiled behind the
/// `tree-sitter` feature; languages without a grammar still participate in
/// the file inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
    Python,
    // The serde keys of the two multi-word variants are pinned to
    // `as_str()` (`javascript`/`typescript`), not the snake_case spelling
    // (`java_script`/`type_script`): the store column, config tables, and
    // diagnostics all use `as_str()`, and the JSON channel must
    // round-trip with them.
    #[serde(rename = "javascript")]
    JavaScript,
    #[serde(rename = "typescript")]
    TypeScript,
    Go,
}

impl Language {
    /// Detect a language from a file extension. Returns `None` for files the
    /// index does not parse (they still get a file-inventory row).
    #[must_use]
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "rs" => Some(Self::Rust),
            "py" | "pyi" => Some(Self::Python),
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "ts" | "mts" | "cts" | "tsx" => Some(Self::TypeScript),
            "go" => Some(Self::Go),
            _ => None,
        }
    }

    /// Stable string key used in config tables, the store, and diagnostics.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Go => "go",
        }
    }

    /// All languages the index can parse, in stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::Rust,
            Self::Python,
            Self::JavaScript,
            Self::TypeScript,
            Self::Go,
        ]
    }
}

/// A 1-based position span inside a file. Lines are 1-based; columns are
/// 1-based byte offsets within the line (converted by backends from parser
/// points). Tools mostly surface `line`; columns are advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// Symbol categories extracted by backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Interface,
    Class,
    TypeAlias,
    Constant,
    Macro,
    Module,
    Field,
}

impl SymbolKind {
    /// Parse a kind from tool input or config, accepting the common
    /// hyphenated / spaced spellings (house `parse()` idiom).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' ', '.'], "_")
            .as_str()
        {
            "function" | "func" | "fn" => Some(Self::Function),
            "method" => Some(Self::Method),
            "struct" => Some(Self::Struct),
            "enum" => Some(Self::Enum),
            "trait" => Some(Self::Trait),
            "interface" => Some(Self::Interface),
            "class" => Some(Self::Class),
            "type_alias" | "typealias" | "type" | "typedef" => Some(Self::TypeAlias),
            "constant" | "const" => Some(Self::Constant),
            "macro" => Some(Self::Macro),
            "module" | "mod" | "namespace" => Some(Self::Module),
            "field" | "property" | "prop" => Some(Self::Field),
            _ => None,
        }
    }

    /// Stable string key used in tool input and the store.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Class => "class",
            Self::TypeAlias => "type_alias",
            Self::Constant => "constant",
            Self::Macro => "macro",
            Self::Module => "module",
            Self::Field => "field",
        }
    }

    /// All kinds, in stable order (for schema enums and docs).
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::Function,
            Self::Method,
            Self::Struct,
            Self::Enum,
            Self::Trait,
            Self::Interface,
            Self::Class,
            Self::TypeAlias,
            Self::Constant,
            Self::Macro,
            Self::Module,
            Self::Field,
        ]
    }
}

/// A symbol definition extracted from one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    /// Declared name (e.g. `ToolRegistry`).
    pub name: String,
    /// What kind of thing this is.
    pub kind: SymbolKind,
    /// Enclosing symbol name (e.g. the impl/type a method belongs to).
    pub container: Option<String>,
    /// Workspace-relative path.
    pub path: String,
    /// Span of the definition.
    pub location: Location,
    /// Best-effort signature line (e.g. `fn build(&self) -> Result<Tool>`).
    pub signature: Option<String>,
}

/// Whether an occurrence is the definition site or a reference site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceRole {
    Definition,
    Reference,
}

/// A name appearance in a file. References are **lexical** (name-based) in
/// this cycle — no cross-file semantic resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub name: String,
    pub role: OccurrenceRole,
    /// Workspace-relative path.
    pub path: String,
    /// 1-based line of the occurrence.
    pub line: u32,
}

/// One file's inventory row: path plus the metadata used for lazy
/// incremental freshness checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Workspace-relative path, forward slashes.
    pub path: String,
    /// Modification time in milliseconds since the Unix epoch.
    pub mtime_ms: i64,
    /// File size in bytes.
    pub size: u64,
    /// Parsed language, if any (files without a grammar stay `None`).
    pub language: Option<Language>,
}

/// Point-in-time counters describing the index for a workspace.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexStats {
    pub files: u64,
    pub symbols: u64,
    /// Files known (or discovered) to be out of date after the last refresh
    /// that exceeded its budget. Zero means fully fresh.
    pub stale_files: u64,
    /// When the last refresh completed, if ever.
    pub last_refresh: Option<DateTime<Utc>>,
    /// Backend id that produced the symbol data (e.g. `tree-sitter`).
    pub backend: String,
}

/// Query shape for `search_symbols` / the `symbol_search` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct SymbolQuery {
    /// Case-insensitive substring of the symbol name (required, non-empty).
    pub query: String,
    /// Optional kind filter.
    pub kind: Option<SymbolKind>,
    /// Optional glob filter on the workspace-relative path
    /// (e.g. `crates/tui/**/*.rs`).
    pub file_glob: Option<String>,
    /// Maximum results. Callers should pass [`DEFAULT_SYMBOL_LIMIT`] when
    /// the user did not choose one.
    pub limit: usize,
}

impl Default for SymbolQuery {
    fn default() -> Self {
        Self {
            query: String::new(),
            kind: None,
            file_glob: None,
            limit: DEFAULT_SYMBOL_LIMIT,
        }
    }
}

/// Query shape for `list_files`.
#[derive(Debug, Clone, Deserialize)]
pub struct FileQuery {
    /// Optional glob on the workspace-relative path.
    pub glob: Option<String>,
    /// Optional extension filter (without the dot, e.g. `rs`).
    pub extension: Option<String>,
    /// Maximum results.
    pub limit: usize,
}

impl Default for FileQuery {
    /// Manual like `SymbolQuery`'s: a derived `Default` would produce
    /// `limit: 0`, which the store's `list_files` treats as "stop after
    /// the first matching row".
    fn default() -> Self {
        Self {
            glob: None,
            extension: None,
            limit: DEFAULT_FILE_LIMIT,
        }
    }
}

/// Bound on the lazy incremental refresh a single query may trigger.
/// Files beyond the budget are left stale and counted in
/// [`IndexStats::stale_files`] instead of blocking the agent turn.
#[derive(Debug, Clone, Copy)]
pub struct RefreshBudget {
    /// Maximum number of files to (re-)extract in one refresh.
    pub max_files: usize,
    /// Wall-clock ceiling for one refresh.
    pub max_duration: Duration,
}

impl Default for RefreshBudget {
    fn default() -> Self {
        Self {
            max_files: 256,
            max_duration: Duration::from_millis(2_000),
        }
    }
}

/// Precompiled glob: the pattern is split and char-collected once per
/// query, then matched per candidate row. Providers evaluate one glob
/// against up to hundreds of rows (sometimes while holding the store
/// mutex) — re-splitting the pattern per row is pure waste.
#[derive(Debug, Clone)]
pub struct GlobMatcher {
    segments: Vec<Vec<char>>,
}

impl GlobMatcher {
    #[must_use]
    pub fn new(pattern: &str) -> Self {
        Self {
            segments: pattern.split('/').map(|s| s.chars().collect()).collect(),
        }
    }

    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        let segs: Vec<Vec<char>> = path.split('/').map(|s| s.chars().collect()).collect();
        match_segments(&self.segments, &segs)
    }
}

/// Path glob matcher used by query filters. Supports `*` (any run inside a
/// segment), `?` (one char), and `**` (any number of whole segments).
/// Operates on `char`s, never byte slices, so non-ASCII paths are safe.
/// Thin wrapper over [`GlobMatcher`]; precompile when matching many paths
/// against one pattern.
#[must_use]
pub fn glob_match(pattern: &str, path: &str) -> bool {
    GlobMatcher::new(pattern).matches(path)
}

fn match_segments(pat: &[Vec<char>], segs: &[Vec<char>]) -> bool {
    // Bottom-up DP over (pattern index, segment index): `dp[i][j]` asks
    // whether `pat[i..]` matches `segs[j..]`. The naive recursion retries
    // `**` at every split point (O(segs^k) for k double-stars) — model
    // input reaches this via the unvalidated `symbol_search` `file_glob`
    // param, evaluated per candidate, so adversarial globs must not be
    // able to burn unbounded CPU. Each grid cell is evaluated once:
    // O(pattern segments × path segments).
    let (m, n) = (pat.len(), segs.len());
    let is_double_star = |i: usize| pat[i].len() == 2 && pat[i][0] == '*' && pat[i][1] == '*';
    let mut dp = vec![vec![false; n + 1]; m + 1];
    dp[m][n] = true;
    for i in (0..m).rev() {
        for j in (0..=n).rev() {
            dp[i][j] = if is_double_star(i) {
                // Consume no segment, or consume one more and stay on the
                // double-star (it may absorb any number of segments).
                dp[i + 1][j] || (j < n && dp[i][j + 1])
            } else {
                j < n && chars_match(&pat[i], &segs[j]) && dp[i + 1][j + 1]
            };
        }
    }
    dp[0][0]
}

fn chars_match(p: &[char], s: &[char]) -> bool {
    // Greedy single-segment wildcard match (`*` and `?` only — `*` never
    // crosses segments). Classic last-star backtracking: on a mismatch,
    // retry from one character further into the text instead of recursing
    // into both branches, so the `*a*a*a*a*a*b`-shaped patterns that make
    // the two-branch recursion exponential cost at most O(len(p) × len(s)).
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = si;
            pi += 1;
        } else if star != usize::MAX {
            // The star swallows one more character; resume after it.
            pi = star + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Outcome of a lazy incremental refresh: what the refresh did plus the
/// resulting stats (including any `stale_files` left over by the budget).
#[derive(Debug, Clone, Serialize)]
pub struct RefreshOutcome {
    pub stats: IndexStats,
    /// Files (re-)extracted during this refresh.
    pub refreshed_files: usize,
    /// Wall-clock duration of the refresh in milliseconds.
    pub duration_ms: u64,
}

/// Query / management surface for the per-workspace index. Injected into
/// `ToolContext` as `Option<Arc<dyn IndexServiceApi>>` (mirrors
/// `LspManagerApi`); `None` means the index is disabled or the context is a
/// test that does not need one.
#[async_trait]
pub trait IndexServiceApi: Send + Sync {
    /// Case-insensitive substring search over symbol definitions.
    async fn search_symbols(&self, query: SymbolQuery) -> Result<Vec<Symbol>>;

    /// Definitions whose name case-insensitively equals `name`.
    async fn find_definition(&self, name: &str) -> Result<Vec<Symbol>>;

    /// Lexical occurrences (definitions + references) of `name`.
    async fn find_references(&self, name: &str) -> Result<Vec<Occurrence>>;

    /// File inventory listing (path/metadata), filtered by glob/extension.
    async fn list_files(&self, query: FileQuery) -> Result<Vec<FileEntry>>;

    /// Lazy incremental freshness pass bounded by `budget`. Query methods
    /// call this internally; exposing it lets a host command force a
    /// stronger refresh.
    async fn refresh(&self, budget: RefreshBudget) -> Result<RefreshOutcome>;

    /// Cached counters; does not touch the filesystem.
    fn stats(&self) -> IndexStats;
}

/// One semantic (embedding) search hit. Reserved with the seam — not
/// produced by any built-in backend this cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticHit {
    /// Workspace-relative path of the hit.
    pub path: String,
    /// 1-based line of the best-matching chunk.
    pub line: u32,
    /// Similarity score in `[0, 1]` (higher is better).
    pub score: f32,
}

/// Reserved seam for embedding-based search. The trait, the
/// `[index.semantic]` config section, and an `embeddings` store placeholder
/// exist so a future backend lands without touching the orchestration
/// layer. No built-in implementation this cycle.
#[async_trait]
pub trait SemanticIndexApi: Send + Sync {
    /// (Re-)embed the given files.
    async fn upsert(&self, files: &[PathBuf]) -> Result<()>;

    /// Top-`k` chunks similar to the natural-language query.
    async fn search(&self, query: &str, k: usize) -> Result<Vec<SemanticHit>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn language_detection_covers_supported_extensions() {
        let cases = [
            ("main.rs", Language::Rust),
            ("lib.py", Language::Python),
            ("stub.pyi", Language::Python),
            ("app.js", Language::JavaScript),
            ("app.mjs", Language::JavaScript),
            ("widget.jsx", Language::JavaScript),
            ("main.ts", Language::TypeScript),
            ("comp.tsx", Language::TypeScript),
            ("main.go", Language::Go),
        ];
        for (file, lang) in cases {
            assert_eq!(Language::from_path(Path::new(file)), Some(lang), "{file}");
        }
        assert_eq!(Language::from_path(Path::new("README.md")), None);
        assert_eq!(Language::from_path(Path::new("noext")), None);
    }

    #[test]
    fn symbol_kind_parse_accepts_aliases() {
        assert_eq!(SymbolKind::parse("function"), Some(SymbolKind::Function));
        assert_eq!(SymbolKind::parse("Func"), Some(SymbolKind::Function));
        assert_eq!(SymbolKind::parse("type-alias"), Some(SymbolKind::TypeAlias));
        assert_eq!(SymbolKind::parse("Type"), Some(SymbolKind::TypeAlias));
        assert_eq!(SymbolKind::parse("property"), Some(SymbolKind::Field));
        assert_eq!(SymbolKind::parse("nope"), None);
        for kind in SymbolKind::all() {
            assert_eq!(SymbolKind::parse(kind.as_str()), Some(*kind));
        }
    }

    #[test]
    fn glob_match_star_doublestar_and_question() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(
            !glob_match("*.rs", "src/main.rs"),
            "star does not cross segments"
        );
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(glob_match("**/*.rs", "a/b/c/main.rs"));
        assert!(
            glob_match("**/*.rs", "main.rs"),
            "'**' may match zero segments"
        );
        assert!(glob_match("crates/tui/**/*.rs", "crates/tui/src/a/b.rs"));
        assert!(glob_match("mod?.rs", "mod1.rs"));
        assert!(!glob_match("mod?.rs", "mod10.rs"));
        assert!(!glob_match("src/*.rs", "src/sub/main.rs"));
    }

    #[test]
    fn glob_match_is_char_safe_for_non_ascii() {
        // Regression class of #249: byte-index slicing on non-ASCII names.
        assert!(glob_match("*.rs", "中文模块.rs"));
        assert!(glob_match("源/**", "源/子/文件.rs"));
        assert!(glob_match("文?.rs", "文件.rs"));
    }

    #[test]
    fn glob_match_adversarial_patterns_terminate() {
        // Exponential-backtracking regression (review round 5): the naive
        // two-branch recursion does not finish these in human timescales —
        // `chars_match`'s `*` handling and `match_segments`' `**` split
        // enumeration. The DP/greedy implementation must terminate fast
        // and answer correctly.
        let pat = "*a*a*a*a*a*a*a*a*b";
        let text: String = "a".repeat(64);
        assert!(!glob_match(pat, &text));
        let deep = "a/".repeat(24);
        let deep = deep.trim_end_matches('/');
        assert!(!glob_match("**/**/**/**/**/**/zzz.rs", deep));
        // Same shapes, positive controls — the bound must not change the
        // semantics, only the cost.
        assert!(glob_match("*a*a*a*b", "xa-xa-xa-b"));
        assert!(glob_match("**/**/*.rs", "x/y/z/a.rs"));
        assert!(glob_match("**", "any/deep/path/here"));
        assert!(glob_match("*", ""));
    }
}
