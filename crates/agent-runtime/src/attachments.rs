//! Content-addressed attachment store (TODO-50): durable identity for media
//! attachments under `~/.codesmith/attachments/<sha256>.<ext>`.
//!
//! Predecessor this replaces: the path-based intake in `crates/cli`
//! (`/attach`, `@path`) handed the transcript the user's *original* file path,
//! so a file moved or deleted after the turn broke resume and every later
//! re-send of the message. The store copies the bytes once — a second attach
//! of identical content is a no-op (dedup by hash) — and the transcript's
//! `ImageSource::File` points at the store path, which lives as long as the
//! CodeSmith home does. The hash in the file name *is* the durable identity;
//! no new `ImageSource` variant and no session-format change are needed.
//!
//! # Intake limits (loud rejection — round-4 §2.6 trigger)
//!
//! [`MAX_ATTACHMENT_BYTES`] per file and [`MAX_ATTACHMENTS_PER_MESSAGE`] per
//! message. A violation is an [`AttachmentError`] the caller surfaces to the
//! user verbatim and records as an excluded context reference — never a
//! silent drop, never a silent downscale.
//!
//! # Known limitations
//!
//! - Images only ([`WIRE_IMAGE_EXTENSIONS`]); nothing else has a wire form.
//! - No garbage collection: the store only grows. Dedup keeps repeated
//!   attachments from multiplying it, but nothing prunes orphans.
//! - Blocking filesystem I/O. [`AttachmentStore::ingest`] is synchronous;
//!   callers on the Tokio runtime run it under `spawn_blocking` (the CLI send
//!   path does). The REPL's `/attach` only performs metadata checks inline.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use codesmith_agent::models::WIRE_IMAGE_EXTENSIONS;
use sha2::{Digest, Sha256};

/// Per-file byte cap. Comfortably under every targeted vision provider's
/// request limit while still fitting a full-resolution screenshot.
pub const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;
/// Per-message attachment cap. Bounds both the request size and the
/// post-compaction re-send cost.
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 8;

/// One attachment admitted into the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAttachment {
    /// Content-addressed path inside the store — what the transcript carries.
    pub path: PathBuf,
    /// Lowercase hex SHA-256 of the file bytes.
    pub sha256: String,
    pub bytes: u64,
    /// MIME type derived from the extension (`image/png`, …).
    pub media_type: String,
    /// `true` when identical content was already in the store (nothing was
    /// written).
    pub deduplicated: bool,
}

impl StoredAttachment {
    /// One-line description for the session's context-reference record.
    #[must_use]
    pub fn detail(&self) -> String {
        let dedup = if self.deduplicated {
            " (deduplicated)"
        } else {
            ""
        };
        format!(
            "sha256:{} {} bytes {}{dedup}",
            self.sha256, self.bytes, self.media_type
        )
    }
}

/// Why an attachment was refused or could not be stored. Every variant's
/// `Display` is the user-facing message — print it as-is.
#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    #[error("attachment not found or not a regular file: {}", path.display())]
    NotAFile { path: PathBuf },
    #[error(
        "unsupported attachment type .{ext} — providers can only carry {supported} on the wire \
         (video has no wire representation; text files go in the message as @path)"
    )]
    UnsupportedType { ext: String, supported: String },
    #[error(
        "attachment {} is {bytes} bytes; the per-file limit is {limit} bytes",
        path.display()
    )]
    TooLarge {
        path: PathBuf,
        bytes: u64,
        limit: u64,
    },
    #[error("too many attachments for one message ({count}); the limit is {limit}")]
    TooMany { count: usize, limit: usize },
    #[error("attachment store unavailable: {0}")]
    Unavailable(String),
    #[error("attachment store I/O failed for {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// MIME type for a wire-representable image extension (lowercase).
#[must_use]
pub fn media_type_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Filesystem-backed content-addressed store.
#[derive(Debug, Clone)]
pub struct AttachmentStore {
    root: PathBuf,
}

impl AttachmentStore {
    /// Store rooted at an explicit directory (tests, embedders).
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The user store: `~/.codesmith/attachments/` (honours `CODESMITH_HOME`).
    pub fn default_location() -> Result<Self, AttachmentError> {
        codesmith_config::resolve_state_dir("attachments")
            .map(Self::new)
            .map_err(|err| AttachmentError::Unavailable(err.to_string()))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validate `count` attachments for one message against
    /// [`MAX_ATTACHMENTS_PER_MESSAGE`].
    pub fn check_message_count(count: usize) -> Result<(), AttachmentError> {
        if count > MAX_ATTACHMENTS_PER_MESSAGE {
            return Err(AttachmentError::TooMany {
                count,
                limit: MAX_ATTACHMENTS_PER_MESSAGE,
            });
        }
        Ok(())
    }

    /// Metadata-only admission check (type, existence, size) — cheap enough
    /// for the REPL to run inline at `/attach` time so the user hears a
    /// refusal immediately rather than at send time. Returns the lowercase
    /// extension and byte size on success.
    pub fn precheck(source: &Path) -> Result<(String, u64), AttachmentError> {
        let metadata = fs::metadata(source).map_err(|_| AttachmentError::NotAFile {
            path: source.to_path_buf(),
        })?;
        if !metadata.is_file() {
            return Err(AttachmentError::NotAFile {
                path: source.to_path_buf(),
            });
        }
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !WIRE_IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            return Err(AttachmentError::UnsupportedType {
                ext,
                supported: WIRE_IMAGE_EXTENSIONS.join("/"),
            });
        }
        let bytes = metadata.len();
        if bytes > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentError::TooLarge {
                path: source.to_path_buf(),
                bytes,
                limit: MAX_ATTACHMENT_BYTES,
            });
        }
        Ok((ext, bytes))
    }

    /// Admit one file: precheck, hash, and copy it into the store unless
    /// identical content is already there. Blocking I/O — see the module's
    /// known limitations for where this may run.
    pub fn ingest(&self, source: &Path) -> Result<StoredAttachment, AttachmentError> {
        let (ext, bytes) = Self::precheck(source)?;
        let media_type = media_type_for_extension(&ext)
            .ok_or_else(|| AttachmentError::UnsupportedType {
                ext: ext.clone(),
                supported: WIRE_IMAGE_EXTENSIONS.join("/"),
            })?
            .to_string();
        let data = fs::read(source).map_err(|source_err| AttachmentError::Io {
            path: source.to_path_buf(),
            source: source_err,
        })?;
        let sha256 = hex_sha256(&data);
        let path = self.root.join(format!("{sha256}.{ext}"));

        if path.is_file() {
            return Ok(StoredAttachment {
                path,
                sha256,
                bytes,
                media_type,
                deduplicated: true,
            });
        }

        fs::create_dir_all(&self.root).map_err(|source_err| AttachmentError::Io {
            path: self.root.clone(),
            source: source_err,
        })?;
        // Write-then-rename so a crash mid-copy never leaves a half file
        // under the content-addressed name.
        let tmp = self
            .root
            .join(format!(".{sha256}.{ext}.{}.tmp", std::process::id()));
        let write = (|| -> std::io::Result<()> {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(&data)?;
            file.sync_all()?;
            fs::rename(&tmp, &path)
        })();
        if let Err(source_err) = write {
            let _ = fs::remove_file(&tmp);
            return Err(AttachmentError::Io {
                path: path.clone(),
                source: source_err,
            });
        }

        Ok(StoredAttachment {
            path,
            sha256,
            bytes,
            media_type,
            deduplicated: false,
        })
    }
}

fn hex_sha256(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).expect("write fixture");
        path
    }

    #[test]
    fn ingest_copies_under_content_hash_and_reports_media_type() {
        let src_dir = tempdir().expect("src");
        let store_dir = tempdir().expect("store");
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let src = write_file(src_dir.path(), "shot.PNG", b"\x89PNG fake");

        let stored = store.ingest(&src).expect("ingest");

        assert_eq!(stored.media_type, "image/png");
        assert_eq!(stored.bytes, 9);
        assert!(!stored.deduplicated);
        assert_eq!(stored.sha256.len(), 64);
        // Lowercased extension, hash-named, inside the store root.
        assert_eq!(
            stored.path,
            store_dir.path().join(format!("{}.png", stored.sha256))
        );
        assert_eq!(fs::read(&stored.path).unwrap(), b"\x89PNG fake");
        // No temp file left behind.
        let leftovers: Vec<_> = fs::read_dir(store_dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn identical_content_is_deduplicated_regardless_of_source_name() {
        let src_dir = tempdir().expect("src");
        let store_dir = tempdir().expect("store");
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let a = write_file(src_dir.path(), "a.png", b"same bytes");
        let b = write_file(src_dir.path(), "b.png", b"same bytes");

        let first = store.ingest(&a).expect("first");
        let second = store.ingest(&b).expect("second");

        assert_eq!(first.path, second.path);
        assert!(!first.deduplicated);
        assert!(second.deduplicated);
        assert_eq!(fs::read_dir(store_dir.path()).unwrap().count(), 1);
        assert!(second.detail().contains("(deduplicated)"));
    }

    #[test]
    fn oversized_file_is_refused_before_reading() {
        let src_dir = tempdir().expect("src");
        let store_dir = tempdir().expect("store");
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let big = src_dir.path().join("big.png");
        let file = fs::File::create(&big).unwrap();
        file.set_len(MAX_ATTACHMENT_BYTES + 1).unwrap();

        let err = store.ingest(&big).expect_err("must refuse");
        assert!(matches!(err, AttachmentError::TooLarge { .. }), "{err}");
        assert!(err.to_string().contains("per-file limit"));
        // Nothing was written to the store.
        assert!(!store_dir.path().exists() || fs::read_dir(store_dir.path()).unwrap().count() == 0);
    }

    #[test]
    fn unsupported_type_and_missing_file_are_refused_loudly() {
        let src_dir = tempdir().expect("src");
        let store = AttachmentStore::new(src_dir.path().join("store"));
        let mov = write_file(src_dir.path(), "clip.mov", b"video");

        let err = store.ingest(&mov).expect_err("video has no wire form");
        assert!(
            matches!(err, AttachmentError::UnsupportedType { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("png"));

        let err = store
            .ingest(&src_dir.path().join("nope.png"))
            .expect_err("missing file");
        assert!(matches!(err, AttachmentError::NotAFile { .. }), "{err}");
    }

    #[test]
    fn message_count_cap_is_enforced() {
        AttachmentStore::check_message_count(MAX_ATTACHMENTS_PER_MESSAGE)
            .expect("at the cap is fine");
        let err = AttachmentStore::check_message_count(MAX_ATTACHMENTS_PER_MESSAGE + 1)
            .expect_err("over the cap");
        assert!(matches!(err, AttachmentError::TooMany { .. }));
        assert!(err.to_string().contains("too many attachments"));
    }

    #[test]
    fn precheck_accepts_wire_types_and_reports_size() {
        let src_dir = tempdir().expect("src");
        let jpg = write_file(src_dir.path(), "photo.JPEG", b"jpegdata");
        let (ext, bytes) = AttachmentStore::precheck(&jpg).expect("ok");
        assert_eq!(ext, "jpeg");
        assert_eq!(bytes, 8);
        assert_eq!(media_type_for_extension(&ext), Some("image/jpeg"));
    }
}
