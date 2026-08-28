//! Value types shared across the index subsystem.
//!
//! All paths stored or returned by the index are **workspace-relative** with
//! forward slashes, so the store stays portable across hosts and the
//! agent-facing tools can render stable paths.

use std::time::Duration;

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
    JavaScript,
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
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileQuery {
    /// Optional glob on the workspace-relative path.
    pub glob: Option<String>,
    /// Optional extension filter (without the dot, e.g. `rs`).
    pub extension: Option<String>,
    /// Maximum results.
    pub limit: usize,
}

impl FileQuery {
    /// Default-bounded query (limit [`DEFAULT_FILE_LIMIT`]).
    #[must_use]
    pub fn default_bounded() -> Self {
        Self {
            limit: DEFAULT_FILE_LIMIT,
            ..Self::default()
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

/// Path glob matcher used by query filters. Supports `*` (any run inside a
/// segment), `?` (one char), and `**` (any number of whole segments).
/// Operates on `char`s, never byte slices, so non-ASCII paths are safe.
#[must_use]
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let segs: Vec<&str> = path.split('/').collect();
    match_segments(&pat, &segs)
}

fn match_segments(pat: &[&str], segs: &[&str]) -> bool {
    match pat.split_first() {
        None => segs.is_empty(),
        Some((&"**", rest)) => (0..=segs.len()).any(|skip| match_segments(rest, &segs[skip..])),
        Some((p, rest)) => match segs.split_first() {
            Some((s, stail)) => segment_match(p, s) && match_segments(rest, stail),
            None => false,
        },
    }
}

fn segment_match(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = text.chars().collect();
    chars_match(&p, &s)
}

fn chars_match(p: &[char], s: &[char]) -> bool {
    match (p.split_first(), s.split_first()) {
        (None, None) => true,
        (Some((&'*', ptail)), _) => {
            chars_match(ptail, s) || (!s.is_empty() && chars_match(p, &s[1..]))
        }
        (Some((&'?', ptail)), Some((_, stail))) => chars_match(ptail, stail),
        (Some((&pc, ptail)), Some((&sc, stail))) if pc == sc => chars_match(ptail, stail),
        _ => false,
    }
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
}
