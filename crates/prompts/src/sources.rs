//! Prompt source types used by `EngineConfig`.
//!
//! Extracted from `crates/tui/src/prompts.rs`.

use std::path::PathBuf;

/// Source for an `EngineConfig.instructions` entry. Either a disk file (loaded
/// at render time, original semantics) or an inline string (content baked into
/// `EngineConfig`, no disk I/O at render time).
///
/// The inline variant is useful for embedders that compute instructions at
/// runtime (e.g. rendering a template with workspace-specific substitutions)
/// and don't want to stage the content to a disk file just to satisfy a path
/// API. Staging adds two problems the inline path avoids:
///
///   1. The disk file looks like editable config but gets overwritten on
///      every launch — confusing for users browsing the install dir.
///   2. Multi-engine setups need per-engine paths to avoid `rehydrate`
///      reading another session's instructions; with inline sources the
///      content lives in the per-engine `EngineConfig` and the race
///      surface goes away.
///
/// `From<PathBuf>` is provided so existing callers passing `Vec<PathBuf>` can
/// keep working with a `.into()` upgrade at the call site.
#[derive(Debug, Clone)]
pub enum InstructionSource {
    /// Load this file from disk at prompt-render time. Original behavior:
    /// missing files are skipped with a warning, oversized files are
    /// truncated to `INSTRUCTIONS_FILE_MAX_BYTES` with an `[…elided]`
    /// marker.
    File(PathBuf),
    /// Use the provided string directly. `name` becomes the
    /// `<instructions source="…">` attribute (typically a synthetic
    /// identifier like `embedded:my-template` or a logical path).
    Inline { name: String, content: String },
}

impl From<PathBuf> for InstructionSource {
    fn from(path: PathBuf) -> Self {
        InstructionSource::File(path)
    }
}

impl From<&PathBuf> for InstructionSource {
    fn from(path: &PathBuf) -> Self {
        InstructionSource::File(path.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAppendSource {
    Inline { name: String, content: String },
    File(PathBuf),
}

impl PromptAppendSource {
    pub fn inline(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Inline {
            name: name.into(),
            content: content.into(),
        }
    }

    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }
}

impl From<PathBuf> for PromptAppendSource {
    fn from(path: PathBuf) -> Self {
        Self::File(path)
    }
}

impl From<&PathBuf> for PromptAppendSource {
    fn from(path: &PathBuf) -> Self {
        Self::File(path.clone())
    }
}
