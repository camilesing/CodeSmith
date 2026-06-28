//! Skill directory helpers.
//!
//! Extracted from `crates/tui/src/skills/mod.rs`.

use std::path::PathBuf;

#[allow(dead_code)]
#[must_use]
pub fn default_skills_dir() -> PathBuf {
    dirs::home_dir().map_or_else(
        || PathBuf::from("/tmp/codesmith/skills"),
        |p| p.join(".codesmith").join("skills"),
    )
}
