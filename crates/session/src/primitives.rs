//! Shared, dependency-free primitives of the session data plane — atomic
//! writes, hashing, and token estimation. Extracted from
//! `codesmith-agent-runtime`'s `utils` and `tokenizer` during the round-9
//! split so `codesmith-session` owns no edge back into the engine crate;
//! `codesmith-agent-runtime` re-exports them for call-site compatibility.

use std::path::Path;

use sha2::{Digest, Sha256};

/// Atomically write `contents` to `path` using a temporary file + fsync + rename.
///
/// 1. Creates a `NamedTempFile` in the same directory as `path` (same filesystem).
/// 2. Writes `contents` to the temp file.
/// 3. Calls `sync_all()` on the temp file for durability.
/// 4. Atomically renames (persists) the temp file over `path`.
///
/// On filesystems that support it (`ext4`, `apfs`, `ntfs`), the rename is
/// atomic — a concurrent reader sees either the old content or the new, never
/// a partial write. `sync_all` ensures the data is on stable storage before
/// the metadata change so an OS crash mid-rename doesn't lose data.
///
/// # Errors
/// Returns `io::Error` if the parent directory cannot be determined, the temp
/// file cannot be created, the write fails, or the rename fails.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("path has no parent directory: {}", path.display()),
        )
    })?;
    // Use parent directory so the rename is on the same filesystem.
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    // `NamedTempFile` is created 0600 on Unix, so persisting over an existing
    // target would silently reset its permissions (a 0644 file drifts to 0600
    // after one atomic write). Copy the target's mode onto the temp file
    // before the rename; a missing target keeps the default 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = tmp
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(
                    meta.permissions().mode() & 0o7777,
                ));
        }
    }
    std::io::Write::write_all(&mut tmp, contents)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    // Make the rename itself durable (best-effort where dir fsync is
    // unsupported).
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Compute the SHA-256 hex digest of a byte slice.
///
/// Single source for the prompt-cache / inspect / rlm paths (ROADMAP §A
/// slice 42) — previously byte-duplicated across `prefix_cache`,
/// `prompt_zones`, `tools/handle`, `rlm/session`, and `prompt_inspect`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// CJK-aware heuristic token estimate — the same arithmetic as the engine
/// tokenizer's `TokenCounter::Heuristic` (one token per CJK codepoint, one
/// per four remaining characters), duplicated here so the session crate's
/// budgets match the engine's without depending on its tokenizer registry.
#[must_use]
pub fn estimate_text_tokens_conservative(text: &str) -> usize {
    let mut cjk_tokens = 0usize;
    let mut other_chars = 0usize;
    for ch in text.chars() {
        if is_cjk(ch) {
            cjk_tokens += 1;
        } else {
            other_chars += 1;
        }
    }
    cjk_tokens + other_chars.div_ceil(4)
}

/// Whether `ch` belongs to a block that tokenizes at roughly one token
/// per character under real BPE tokenizers.
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3000..=0x303F   // CJK symbols and punctuation
        | 0x3040..=0x309F // Hiragana
        | 0x30A0..=0x30FF // Katakana
        | 0x3400..=0x4DBF // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0xFF00..=0xFFEF // Halfwidth and Fullwidth Forms
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_writes_content_and_replaces() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        write_atomic(&path, b"one").expect("write");
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        write_atomic(&path, b"two").expect("replace");
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }

    #[test]
    fn sha256_hex_is_stable() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn token_estimate_matches_the_engine_heuristic() {
        // Same arithmetic as the engine tokenizer's heuristic: 12 ASCII
        // chars → 3 tokens; one CJK char per codepoint.
        assert_eq!(estimate_text_tokens_conservative("abcdefghijkl"), 3);
        assert_eq!(estimate_text_tokens_conservative("代码"), 2);
        assert_eq!(estimate_text_tokens_conservative(""), 0);
    }
}
