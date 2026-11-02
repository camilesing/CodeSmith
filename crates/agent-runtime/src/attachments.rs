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
//! Any file type is admitted except audio/video (no consumer in this
//! build). How an admitted attachment reaches the model is
//! [`AttachmentKind`]: wire images ride native image blocks, text inlines
//! into the message (bounded by [`MAX_INLINE_TEXT_BYTES`]), every other
//! binary attaches as a durable path reference the model reads with
//! `read_file` / shell tools on demand.
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
//! - Audio/video files are refused at the gate: no consumer exists in this
//!   build (no transcription, no video wire form) — see
//!   [`AUDIO_VIDEO_EXTENSIONS`]. Everything else is admitted.
//! - Text attachments inline up to [`MAX_INLINE_TEXT_BYTES`]; a larger text
//!   file degrades to a path reference, and PDFs are path references by
//!   choice (`read_file` already extracts them — not duplicated here).
//! - No garbage collection: the store only grows. Dedup keeps repeated
//!   attachments from multiplying it, but nothing prunes orphans.
//! - Blocking filesystem I/O. [`AttachmentStore::ingest`] is synchronous;
//!   callers on the Tokio runtime run it under `spawn_blocking` (the CLI send
//!   path does). The REPL's `/attach` only performs metadata checks inline.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Per-file byte cap. Comfortably under every targeted vision provider's
/// request limit while still fitting a full-resolution screenshot.
pub const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;
/// Per-message attachment cap. Bounds both the request size and the
/// post-compaction re-send cost.
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 8;
/// Largest text attachment whose content is inlined into the message (the
/// consumer reads this at intake). A larger text file degrades to a path
/// reference so one attachment cannot crowd out the context window.
pub const MAX_INLINE_TEXT_BYTES: u64 = 64 * 1024;

/// Extensions refused at the gate: audio/video have no consumer in this
/// build (no transcription, no video wire form), so admitting them would
/// only store bytes the model can never be shown.
pub const AUDIO_VIDEO_EXTENSIONS: &[&str] = &[
    // audio
    "mp3", "wav", "ogg", "flac", "aac", "m4a", "opus", "wma", // video
    "mp4", "m4v", "mov", "avi", "mkv", "webm", "mpg", "mpeg", "wmv", "3gp",
];

/// Extensions whose content is inlined into the message text (up to
/// [`MAX_INLINE_TEXT_BYTES`]). Everything non-image that is not on this list
/// — and not audio/video — attaches as a durable path reference.
pub const TEXT_INLINE_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "rst", "log", "json", "jsonl", "jsonc", "toml", "yaml", "yml", "csv",
    "tsv", "xml", "html", "htm", "css", "ini", "cfg", "conf", "env", "diff", "patch", "js", "mjs",
    "cjs", "ts", "tsx", "jsx", "py", "pyi", "rs", "go", "c", "h", "cc", "cpp", "hpp", "java", "kt",
    "swift", "rb", "php", "sh", "bash", "zsh", "fish", "ps1", "sql", "lua", "vim", "r", "m", "pl",
    "hs", "ex", "exs", "zig", "nim", "scala", "clj", "scm",
];

/// How an admitted attachment reaches the model. Derived from the extension
/// and the byte size at intake; the consumer (CLI media intake) dispatches
/// on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    /// A wire-representable image ([`WIRE_IMAGE_EXTENSIONS`](codesmith_agent::models::WIRE_IMAGE_EXTENSIONS))
    /// promoted to a native `Image` content block.
    WireImage,
    /// Text content inlined into the message (≤ [`MAX_INLINE_TEXT_BYTES`]).
    InlineText,
    /// Any other binary: the message carries the durable store path, the
    /// byte size, and the media type; the model reads it with `read_file` /
    /// shell tools on demand.
    PathReference,
}

impl AttachmentKind {
    /// Terminal badge for the session's context-reference record.
    #[must_use]
    pub fn badge(self) -> &'static str {
        match self {
            Self::WireImage => "image",
            Self::InlineText => "text",
            Self::PathReference => "file",
        }
    }
}

/// Classify an admitted extension at `bytes` size. Metadata-only: the kind
/// of a text file flips to [`AttachmentKind::PathReference`] once it exceeds
/// [`MAX_INLINE_TEXT_BYTES`].
#[must_use]
pub fn classify_attachment(ext: &str, bytes: u64) -> AttachmentKind {
    if matches!(media_type_for_extension(ext), m if m.starts_with("image/")) {
        AttachmentKind::WireImage
    } else if TEXT_INLINE_EXTENSIONS.contains(&ext) && bytes <= MAX_INLINE_TEXT_BYTES {
        AttachmentKind::InlineText
    } else {
        AttachmentKind::PathReference
    }
}

/// Per-call nonce for ingest temp-file names (see `ingest`): distinguishes
/// concurrent ingests within one process.
static TMP_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One attachment admitted into the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAttachment {
    /// Content-addressed path inside the store — what the transcript carries.
    pub path: PathBuf,
    /// Lowercase hex SHA-256 of the file bytes.
    pub sha256: String,
    pub bytes: u64,
    /// MIME type derived from the extension (`image/png`, …), falling back
    /// to `application/octet-stream` for unknown binary types.
    pub media_type: String,
    /// How this attachment reaches the model (wire image / inline text /
    /// path reference).
    pub kind: AttachmentKind,
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
        "unsupported attachment type .{ext} — audio/video have no consumer in this build; \
         every other file is admitted (images travel on the wire, text is inlined, other \
         binaries attach by path)"
    )]
    UnsupportedType { ext: String },
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

/// MIME type for an admitted extension (lowercase). Unknown binary types
/// fall back to `application/octet-stream` — a path-reference attachment
/// reports what it is, not `None`.
#[must_use]
pub fn media_type_for_extension(ext: &str) -> &'static str {
    match ext {
        // Wire images — the set the providers can serialize.
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        // Inlined text types (the inlining decision is
        // [`classify_attachment`]'s; the MIME here describes the file).
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" => "text/javascript",
        "json" | "jsonl" | "jsonc" => "application/json",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        _ if TEXT_INLINE_EXTENSIONS.contains(&ext) => "text/plain",
        _ => "application/octet-stream",
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
    /// refusal immediately rather than at send time. Any file type is
    /// admitted except audio/video ([`AUDIO_VIDEO_EXTENSIONS`]); the byte
    /// cap applies to all kinds alike. Returns the lowercase extension and
    /// byte size on success.
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
        if AUDIO_VIDEO_EXTENSIONS.contains(&ext.as_str()) {
            return Err(AttachmentError::UnsupportedType { ext });
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
        let media_type = media_type_for_extension(&ext).to_string();
        let kind = classify_attachment(&ext, bytes);
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
                kind,
                deduplicated: true,
            });
        }

        fs::create_dir_all(&self.root).map_err(|source_err| AttachmentError::Io {
            path: self.root.clone(),
            source: source_err,
        })?;
        // Write-then-rename so a crash mid-copy never leaves a half file
        // under the content-addressed name. The tmp name carries a
        // per-call nonce on top of the pid: concurrent `ingest` calls in
        // one process (the same screenshot attached in two parallel sends)
        // used to share the tmp file — truncating each other's partial
        // writes and racing the rename onto the dedup'd content address.
        let nonce = TMP_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = self.root.join(format!(
            ".{sha256}.{ext}.{}.{}.tmp",
            std::process::id(),
            nonce
        ));
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
            kind,
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
        assert_eq!(stored.kind, AttachmentKind::WireImage);
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
    fn arbitrary_binaries_are_admitted_as_path_references() {
        let src_dir = tempdir().expect("src");
        let store = AttachmentStore::new(src_dir.path().join("store"));
        let blob = write_file(src_dir.path(), "model.safetensors", b"\x00\x01binary");

        let stored = store.ingest(&blob).expect("arbitrary binary is admitted");

        assert_eq!(stored.kind, AttachmentKind::PathReference);
        assert_eq!(stored.kind.badge(), "file");
        assert_eq!(stored.media_type, "application/octet-stream");
        // A PDF is a path reference by choice: `read_file` already extracts.
        let pdf = write_file(src_dir.path(), "spec.pdf", b"%PDF-1.7");
        let stored = store.ingest(&pdf).expect("pdf is admitted");
        assert_eq!(stored.kind, AttachmentKind::PathReference);
        assert_eq!(stored.media_type, "application/pdf");
    }

    #[test]
    fn text_files_inline_up_to_the_cap_and_reference_beyond_it() {
        let src_dir = tempdir().expect("src");
        let store = AttachmentStore::new(src_dir.path().join("store"));

        let small = write_file(src_dir.path(), "notes.md", b"# notes");
        let stored = store.ingest(&small).expect("text is admitted");
        assert_eq!(stored.kind, AttachmentKind::InlineText);
        assert_eq!(stored.kind.badge(), "text");
        assert_eq!(stored.media_type, "text/markdown");

        // One byte over the inline cap: still admitted, by path reference.
        let big = src_dir.path().join("big.log");
        fs::write(&big, vec![b'x'; (MAX_INLINE_TEXT_BYTES + 1) as usize]).unwrap();
        let stored = store.ingest(&big).expect("oversize text is admitted");
        assert_eq!(stored.kind, AttachmentKind::PathReference);
        assert_eq!(stored.media_type, "text/plain");

        // The classifier is metadata-only, so the boundary is checkable
        // without touching the filesystem.
        assert_eq!(
            classify_attachment("md", MAX_INLINE_TEXT_BYTES),
            AttachmentKind::InlineText
        );
        assert_eq!(
            classify_attachment("md", MAX_INLINE_TEXT_BYTES + 1),
            AttachmentKind::PathReference
        );
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
    fn audio_and_video_are_refused_and_missing_files_are_loud() {
        let src_dir = tempdir().expect("src");
        let store = AttachmentStore::new(src_dir.path().join("store"));
        let mov = write_file(src_dir.path(), "clip.mov", b"video");
        let mp3 = write_file(src_dir.path(), "track.mp3", b"audio");

        for refused in [&mov, &mp3] {
            let err = store.ingest(refused).expect_err("no consumer exists");
            assert!(
                matches!(err, AttachmentError::UnsupportedType { .. }),
                "{err}"
            );
            assert!(err.to_string().contains("audio/video have no consumer"));
        }

        let err = store
            .ingest(&src_dir.path().join("nope.png"))
            .expect_err("missing file");
        assert!(matches!(err, AttachmentError::NotAFile { .. }));
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
    fn precheck_accepts_admitted_types_and_reports_size() {
        let src_dir = tempdir().expect("src");
        let jpg = write_file(src_dir.path(), "photo.JPEG", b"jpegdata");
        let (ext, bytes) = AttachmentStore::precheck(&jpg).expect("ok");
        assert_eq!(ext, "jpeg");
        assert_eq!(bytes, 8);
        assert_eq!(media_type_for_extension(&ext), "image/jpeg");

        let zip = write_file(src_dir.path(), "bundle.zip", b"zipdata");
        let (ext, bytes) = AttachmentStore::precheck(&zip).expect("binary is admitted");
        assert_eq!(ext, "zip");
        assert_eq!(bytes, 7);
        assert_eq!(media_type_for_extension(&ext), "application/zip");
    }
}
