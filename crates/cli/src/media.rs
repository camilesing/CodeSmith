//! Media attachment intake for the REPL (TODO-50), the inline-encoded
//! attachment path of the HTTP turn API, and the stable storage location for
//! pasted images.
//!
//! Intake is the CLI-side consumer of the content-addressed store
//! (`codesmith_agent_runtime::attachments`): every file a message carries —
//! `/attach`ed or `@path`-mentioned — is admitted through
//! [`ingest_for_message`] at send time, which copies it into
//! `~/.codesmith/attachments/<sha256>.<ext>` and dispatches on
//! [`AttachmentKind`]: wire images hand the engine the store path (native
//! image blocks), inline text is folded into the outgoing message content
//! (returned as `IntakeOutcome::content_appendix`), and every other binary
//! appends a durable path + size + media-type reference line the model can
//! act on with `read_file` / shell tools. The same call produces the
//! session's context-reference record for each attachment, admitted
//! (`included: true`, with hash/size/type) or refused (`included: false`,
//! with the reason) — so the record says exactly what reached the model and
//! what was turned away. The HTTP turn API rides the same store through
//! [`ingest_encoded_attachments`] (base64 bytes, all-or-nothing, no
//! context-reference records — the evidence rides the message text).
//!
//! Known limitations: `/attach` runs only the metadata-level
//! [`AttachmentStore::precheck`] inline (the REPL loop is on the runtime; the
//! hash + copy is blocking I/O and happens under `spawn_blocking` here), so a
//! file that changes between `/attach` and send is admitted on its send-time
//! bytes. Inline text is re-read from the store path (not the original), so
//! what the message carries is the stored bytes.

use std::path::{Path, PathBuf};

use serde::Serialize;

use codesmith_agent_runtime::attachments::{
    AttachmentKind, AttachmentStore, StoredAttachment, classify_attachment,
};
use codesmith_session::store::{
    ContextReference, ContextReferenceKind, ContextReferenceSource, SessionContextReference,
};

/// Resolve the directory pasted images should land in. Prefers
/// `~/.codesmith/clipboard-images/` so the path is stable across workspaces
/// and matches the location described in user-facing docs; falls back to
/// `<workspace>/clipboard-images/` if the home dir is unavailable.
pub fn clipboard_images_dir(workspace: &Path) -> PathBuf {
    clipboard_images_dir_for_home(
        workspace,
        codesmith_config::codesmith_home().ok().as_deref(),
    )
}

fn clipboard_images_dir_for_home(workspace: &Path, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        return home.join(".codesmith").join("clipboard-images");
    }
    workspace.join("clipboard-images")
}

/// One file slated for the next message, tagged with how the user named it.
#[derive(Debug, Clone)]
pub struct PendingAttachment {
    pub path: PathBuf,
    pub source: ContextReferenceSource,
}

/// Result of admitting one message's attachments into the store.
#[derive(Debug, Default)]
pub struct IntakeOutcome {
    /// Store paths of wire-representable images to hand the engine
    /// (`Op::SendMessage.image_paths`), in input order.
    pub accepted: Vec<PathBuf>,
    /// Text folded into the outgoing message content: fenced content for
    /// inline text attachments, durable path + size + media-type reference
    /// lines for other binaries. Empty when every attachment was an image.
    pub content_appendix: String,
    /// Session record for every attachment — admitted or refused.
    pub references: Vec<SessionContextReference>,
    /// User-facing refusal lines; the caller prints them.
    pub rejections: Vec<String>,
}

/// Admit `attachments` for the message that will land at `message_index`:
/// the per-message cap first (files beyond it are refused, not silently
/// dropped), then each file through [`AttachmentStore::ingest`]. Blocking
/// I/O — run under `spawn_blocking` on the runtime
/// ([`ingest_for_message`]).
pub fn ingest_into(
    store: &AttachmentStore,
    attachments: &[PendingAttachment],
    message_index: usize,
) -> IntakeOutcome {
    let mut outcome = IntakeOutcome::default();
    let mut appendix = String::new();
    for (position, attachment) in attachments.iter().enumerate() {
        let admitted = AttachmentStore::check_message_count(position + 1)
            .and_then(|()| store.ingest(&attachment.path));
        match admitted {
            Ok(stored) => {
                outcome.references.push(admitted_reference(
                    &attachment.path,
                    &stored,
                    attachment.source,
                    message_index,
                ));
                dispatch_into(
                    &attachment.path,
                    &stored,
                    &mut outcome.accepted,
                    &mut appendix,
                );
            }
            Err(err) => {
                outcome.rejections.push(format!(
                    "attachment refused: {err} ({})",
                    attachment.path.display()
                ));
                outcome.references.push(rejected_reference(
                    &attachment.path,
                    attachment.source,
                    message_index,
                    &err.to_string(),
                ));
            }
        }
    }
    outcome.content_appendix = appendix;
    outcome
}

/// Route one admitted attachment by its kind: wire images to the engine's
/// image paths, everything else into the message-content appendix. The
/// appendix lines name the *original* file (the store path is a content
/// hash — the label is the only place the file's name survives).
fn dispatch_into(
    original: &Path,
    stored: &StoredAttachment,
    accepted: &mut Vec<PathBuf>,
    appendix: &mut String,
) {
    match stored.kind {
        AttachmentKind::WireImage => accepted.push(stored.path.clone()),
        AttachmentKind::InlineText => {
            let text = fs_read_utf8(&stored.path);
            match text {
                Some(text) => {
                    // A fence longer than any backtick run inside the
                    // content, so the block cannot be broken out of.
                    let fence = fence_for(&text);
                    if !appendix.is_empty() {
                        appendix.push_str("\n\n");
                    }
                    let _ = std::fmt::Write::write_fmt(
                        appendix,
                        format_args!(
                            "[Attached file: {} — {} bytes, {}, inlined]\n{fence}text\n{text}\n{fence}",
                            file_label(original),
                            stored.bytes,
                            stored.media_type,
                        ),
                    );
                }
                // Non-UTF-8 bytes despite a text-looking extension: say what
                // we stored instead of mangling it into the message.
                None => push_path_reference(original, stored, appendix),
            }
        }
        AttachmentKind::PathReference => push_path_reference(original, stored, appendix),
    }
}

/// A binary attachment's message line: original name, byte size, media
/// type, and the durable store path. The store path survives the original
/// moving or deleting; the model reads it with `read_file` / shell tools on
/// demand.
fn push_path_reference(original: &Path, stored: &StoredAttachment, appendix: &mut String) {
    if !appendix.is_empty() {
        appendix.push_str("\n\n");
    }
    let _ = std::fmt::Write::write_fmt(
        appendix,
        format_args!(
            "[Attached file: {} — {} bytes, {}, stored at {}]",
            file_label(original),
            stored.bytes,
            stored.media_type,
            stored.path.display(),
        ),
    );
}

fn fs_read_utf8(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
}

/// A Markdown fence that no backtick run in `text` can terminate.
fn fence_for(text: &str) -> String {
    let mut run = 0usize;
    let mut best = 0usize;
    for byte in text.bytes() {
        if byte == b'`' {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(best.max(3) + 1)
}

/// [`ingest_into`] against the user store, off the runtime thread. A store
/// that cannot be resolved refuses every attachment loudly (recorded)
/// rather than handing the engine the original paths.
pub async fn ingest_for_message(
    attachments: Vec<PendingAttachment>,
    message_index: usize,
) -> IntakeOutcome {
    if attachments.is_empty() {
        return IntakeOutcome::default();
    }
    let store = match AttachmentStore::default_location() {
        Ok(store) => store,
        Err(err) => return refuse_all(&attachments, message_index, &err.to_string()),
    };
    // Keep the attempt list for the join-failure path: the outcome must
    // record a session reference for every attempted attachment (path,
    // source, reason) on ALL refusal paths — an empty `references` vec
    // silently loses the trace of what was turned away.
    let attempted = attachments.clone();
    match tokio::task::spawn_blocking(move || ingest_into(&store, &attachments, message_index))
        .await
    {
        Ok(outcome) => outcome,
        Err(join_err) => refuse_all(
            &attempted,
            message_index,
            &format!("intake worker failed: {join_err}; no attachments were sent"),
        ),
    }
}

fn refuse_all(
    attachments: &[PendingAttachment],
    message_index: usize,
    reason: &str,
) -> IntakeOutcome {
    let mut outcome = IntakeOutcome::default();
    for attachment in attachments {
        outcome.rejections.push(format!(
            "attachment refused: {reason} ({})",
            attachment.path.display()
        ));
        outcome.references.push(rejected_reference(
            &attachment.path,
            attachment.source,
            message_index,
            reason,
        ));
    }
    outcome
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// One attachment delivered as inline bytes (the HTTP turn API): decoded
/// base64 plus the caller's declared metadata. `name` is a display name —
/// its extension, or the declared media type's, decides how the store
/// classifies the bytes.
#[derive(Debug, Clone)]
pub struct EncodedAttachment {
    pub name: Option<String>,
    pub media_type: Option<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub bytes: Vec<u8>,
}

/// Per-attachment record for the turn item's additive metadata: what the
/// transcript echoes so a client can render the settled message's
/// attachments (kind, content address, display fields). `camelCase` wire —
/// the client's attachment reference vocabulary (`attachmentId`,
/// `mediaType`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodedAttachmentRecord {
    /// `"image"` (wire image) or `"file"` (inlined text or path reference).
    pub kind: &'static str,
    /// `sha256:<hex>` — the store's content address, the same id the file
    /// is stored under.
    pub attachment_id: String,
    pub name: Option<String>,
    pub media_type: String,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
}

/// Outcome of admitting inline-encoded attachments: the same dispatch as the
/// REPL intake (wire images to the engine, everything else folded into the
/// message content), plus the per-attachment records for the item metadata.
#[derive(Debug, Default)]
pub struct EncodedIntake {
    /// Store paths of wire-representable images (`Op::SendMessage.image_paths`).
    pub image_paths: Vec<PathBuf>,
    /// Text appended to the outgoing message content (inline text fences,
    /// durable path reference lines).
    pub content_appendix: String,
    pub records: Vec<EncodedAttachmentRecord>,
}

/// Admit inline-encoded (HTTP turn API) attachments through the user store
/// (`~/.codesmith/attachments`). See [`ingest_encoded_into`] for the
/// semantics.
pub async fn ingest_encoded_attachments(
    encoded: Vec<EncodedAttachment>,
) -> Result<EncodedIntake, String> {
    let store = match AttachmentStore::default_location() {
        Ok(store) => store,
        Err(err) => {
            return Err(format!(
                "attachment store unavailable: {err}; no attachments were sent"
            ));
        }
    };
    ingest_encoded_into(&store, encoded).await
}

/// [`ingest_encoded_attachments`] against an explicit store (tests,
/// embedders). Unlike the REPL intake this is all-or-nothing — one refused
/// attachment fails the whole request with every refusal named, so an HTTP
/// caller never sends half its attachments — and writes no session
/// context-reference records (the evidence rides the message text itself:
/// appendix lines for files, the engine's media-attachment record for
/// images).
///
/// Blocking I/O (temp files, hashing, the store copy) — runs under
/// `spawn_blocking`, like [`ingest_for_message`].
pub async fn ingest_encoded_into(
    store: &AttachmentStore,
    encoded: Vec<EncodedAttachment>,
) -> Result<EncodedIntake, String> {
    if encoded.is_empty() {
        return Ok(EncodedIntake::default());
    }

    // Resolve names and extensions, and verify declared image bytes against
    // their magic before anything lands on disk — a wire-image extension
    // with non-image bytes would hand the provider garbage.
    let mut prepared = Vec::with_capacity(encoded.len());
    for attachment in &encoded {
        let ext = encoded_ext(attachment.name.as_deref(), attachment.media_type.as_deref());
        let label = encoded_label(attachment.name.as_deref(), &ext);
        if !image_magic_matches(&ext, &attachment.bytes) {
            return Err(format!(
                "attachment refused: {label} is not {ext} image data (magic mismatch); no attachments were sent"
            ));
        }
        prepared.push((attachment.clone(), ext, label));
    }

    // A private temp dir per message: the temp file keeps the display name,
    // so the appendix's label is the name the caller sent, and a leaf-only
    // name can never escape the dir.
    let dir = std::env::temp_dir().join(format!(
        "codesmith-encoded-{}",
        uuid::Uuid::new_v4().simple()
    ));
    // `spawn_blocking` needs 'static; the store is a cloned root path.
    let store = store.clone();
    let result = tokio::task::spawn_blocking(move || {
        if let Err(err) = std::fs::create_dir_all(&dir) {
            return Err(format!(
                "attachment intake failed: {err}; no attachments were sent"
            ));
        }
        let mut intake = EncodedIntake::default();
        let mut rejections: Vec<String> = Vec::new();
        for (position, (attachment, _ext, label)) in prepared.iter().enumerate() {
            let tmp = dir.join(label);
            if let Err(err) = std::fs::write(&tmp, &attachment.bytes) {
                rejections.push(format!("attachment refused: {err} ({label})"));
                continue;
            }
            let admitted = AttachmentStore::check_message_count(position + 1)
                .and_then(|()| store.ingest(&tmp));
            match admitted {
                Ok(stored) => {
                    dispatch_into(
                        &tmp,
                        &stored,
                        &mut intake.image_paths,
                        &mut intake.content_appendix,
                    );
                    let wire_image = matches!(stored.kind, AttachmentKind::WireImage);
                    intake.records.push(EncodedAttachmentRecord {
                        kind: if wire_image { "image" } else { "file" },
                        attachment_id: format!("sha256:{}", stored.sha256),
                        // The transcript's label — the same name the
                        // appendix line uses, so the record and the message
                        // text agree.
                        name: Some(label.clone()),
                        media_type: stored.media_type.clone(),
                        bytes: stored.bytes,
                        width: attachment.width.filter(|_| wire_image),
                        height: attachment.height.filter(|_| wire_image),
                    });
                }
                Err(err) => {
                    rejections.push(format!("attachment refused: {err} ({label})"));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        if !rejections.is_empty() {
            Err(rejections.join("; "))
        } else {
            Ok(intake)
        }
    })
    .await;
    match result {
        Ok(outcome) => outcome,
        Err(join_err) => Err(format!(
            "intake worker failed: {join_err}; no attachments were sent"
        )),
    }
}

/// The extension an encoded attachment classifies under: a declared wire
/// image media type wins (canonical extension), then the display name's
/// extension, then a generic binary fallback.
fn encoded_ext(name: Option<&str>, media_type: Option<&str>) -> String {
    if let Some(mime) = media_type {
        match mime {
            "image/png" => return "png".to_string(),
            "image/jpeg" => return "jpg".to_string(),
            "image/gif" => return "gif".to_string(),
            "image/webp" => return "webp".to_string(),
            _ => {}
        }
    }
    name.and_then(|n| Path::new(n).extension().and_then(|e| e.to_str()))
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_else(|| "bin".to_string())
}

/// The temp-file leaf name for an encoded attachment: the caller's display
/// name when it is a usable leaf (`.`/`..` and separator-carrying names are
/// refused, not silently mangled), else `attachment.<ext>`.
fn encoded_label(name: Option<&str>, ext: &str) -> String {
    match name {
        Some(name)
            if !name.is_empty()
                && name != "."
                && name != ".."
                && !name.contains(['/', '\\', '\0']) =>
        {
            name.to_string()
        }
        _ => format!("attachment.{ext}"),
    }
}

/// Whether `bytes` start with the magic of the image format `ext` names.
/// Non-image extensions admit anything — classification, not image-ness,
/// already decided they never reach the wire as images.
fn image_magic_matches(ext: &str, bytes: &[u8]) -> bool {
    match ext {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" | "jpeg" => bytes.starts_with(b"\xFF\xD8\xFF"),
        "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "webp" => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => true,
    }
}

fn admitted_reference(
    original: &Path,
    stored: &StoredAttachment,
    source: ContextReferenceSource,
    message_index: usize,
) -> SessionContextReference {
    SessionContextReference {
        message_index,
        reference: ContextReference {
            kind: ContextReferenceKind::MediaAttachment,
            source,
            badge: stored.kind.badge().to_string(),
            label: file_label(original),
            target: stored.path.display().to_string(),
            included: true,
            expanded: false,
            detail: Some(stored.detail()),
        },
    }
}

/// Record an attachment that was refused (cap, type, size, missing file) so
/// the session file keeps the trace — `included: false` with the reason.
pub fn rejected_reference(
    original: &Path,
    source: ContextReferenceSource,
    message_index: usize,
    reason: &str,
) -> SessionContextReference {
    let badge = extension_badge(original);
    SessionContextReference {
        message_index,
        reference: ContextReference {
            kind: ContextReferenceKind::MediaAttachment,
            source,
            badge: badge.to_string(),
            label: file_label(original),
            target: original.display().to_string(),
            included: false,
            expanded: false,
            detail: Some(format!("rejected: {reason}")),
        },
    }
}

/// Badge for a refused path — classified from the extension alone (the kind
/// never materialized); audio/video refusals badge as generic files.
fn extension_badge(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    classify_attachment(&ext, u64::MAX).badge()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent_runtime::attachments::MAX_ATTACHMENTS_PER_MESSAGE;
    use tempfile::tempdir;

    fn pending(dir: &Path, name: &str, bytes: &[u8]) -> PendingAttachment {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        PendingAttachment {
            path,
            source: ContextReferenceSource::Attachment,
        }
    }

    #[test]
    fn ingest_admits_images_as_store_paths_and_records_them() {
        let src = tempdir().unwrap();
        let store_dir = tempdir().unwrap();
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let images = vec![
            pending(src.path(), "a.png", b"aaa"),
            PendingAttachment {
                source: ContextReferenceSource::AtMention,
                ..pending(src.path(), "b.png", b"bbb")
            },
        ];

        let outcome = ingest_into(&store, &images, 7);

        assert_eq!(outcome.accepted.len(), 2);
        assert!(outcome.content_appendix.is_empty());
        assert!(outcome.rejections.is_empty());
        assert!(
            outcome
                .accepted
                .iter()
                .all(|p| p.starts_with(store_dir.path()))
        );
        assert_eq!(outcome.references.len(), 2);
        let first = &outcome.references[0];
        assert_eq!(first.message_index, 7);
        assert_eq!(first.reference.kind, ContextReferenceKind::MediaAttachment);
        assert_eq!(first.reference.source, ContextReferenceSource::Attachment);
        assert!(first.reference.included);
        assert_eq!(first.reference.label, "a.png");
        assert_eq!(first.reference.badge, "image");
        assert_eq!(
            first.reference.target,
            outcome.accepted[0].display().to_string()
        );
        assert!(
            first
                .reference
                .detail
                .as_deref()
                .unwrap()
                .starts_with("sha256:")
        );
        assert_eq!(
            outcome.references[1].reference.source,
            ContextReferenceSource::AtMention
        );
    }

    #[test]
    fn binary_attachments_become_path_references_in_the_content_appendix() {
        let src = tempdir().unwrap();
        let store_dir = tempdir().unwrap();
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let blob = pending(src.path(), "weights.bin", b"\x00\x01\x02binary");

        let outcome = ingest_into(&store, &[blob], 0);

        // A binary rides the message content, not the engine image paths.
        assert!(outcome.accepted.is_empty());
        assert_eq!(outcome.references.len(), 1);
        assert!(outcome.references[0].reference.included);
        assert_eq!(outcome.references[0].reference.badge, "file");
        let appendix = &outcome.content_appendix;
        assert!(
            appendix.starts_with("[Attached file: weights.bin — "),
            "the appendix names the original file: {appendix}"
        );
        assert!(appendix.contains("9 bytes"), "{appendix}");
        assert!(appendix.contains("application/octet-stream"), "{appendix}");
        assert!(
            appendix.contains(&format!("stored at {}", store_dir.path().display())),
            "the reference is the durable store path: {appendix}"
        );
    }

    #[test]
    fn text_attachments_inline_their_content_into_the_appendix() {
        let src = tempdir().unwrap();
        let store_dir = tempdir().unwrap();
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let notes = pending(src.path(), "notes.md", b"# Title\n\nbody with `ticks`");

        let outcome = ingest_into(&store, &[notes], 2);

        assert!(outcome.accepted.is_empty());
        assert_eq!(outcome.references[0].reference.badge, "text");
        let appendix = &outcome.content_appendix;
        assert!(appendix.contains("[Attached file: notes.md"), "{appendix}");
        assert!(appendix.contains("text/markdown"), "{appendix}");
        assert!(appendix.contains("# Title"), "{appendix}");
        assert!(appendix.contains("body with `ticks`"), "{appendix}");
        // The fence outlives the one-backtick run inside the content.
        assert!(appendix.contains("````text"), "{appendix}");
        assert!(appendix.trim_end().ends_with("````"), "{appendix}");
    }

    #[test]
    fn oversize_text_attaches_by_path_instead_of_inlining() {
        let src = tempdir().unwrap();
        let store_dir = tempdir().unwrap();
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let big_log = src.path().join("server.log");
        std::fs::write(&big_log, vec![b'x'; 200_000]).unwrap();
        let big = PendingAttachment {
            path: big_log,
            source: ContextReferenceSource::Attachment,
        };

        let outcome = ingest_into(&store, &[big], 0);

        assert!(outcome.accepted.is_empty());
        assert!(
            !outcome.content_appendix.contains("xxxx"),
            "{}",
            outcome.content_appendix
        );
        assert!(
            outcome.content_appendix.contains("server.log"),
            "{}",
            outcome.content_appendix
        );
        assert!(
            outcome
                .content_appendix
                .contains("bytes, text/plain, stored at"),
            "{}",
            outcome.content_appendix
        );
    }

    #[test]
    fn ingest_refuses_images_beyond_the_per_message_cap_and_records_each() {
        let src = tempdir().unwrap();
        let store_dir = tempdir().unwrap();
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let images: Vec<PendingAttachment> = (0..MAX_ATTACHMENTS_PER_MESSAGE + 2)
            .map(|i| {
                pending(
                    src.path(),
                    &format!("{i}.png"),
                    format!("img{i}").as_bytes(),
                )
            })
            .collect();

        let outcome = ingest_into(&store, &images, 0);

        assert_eq!(outcome.accepted.len(), MAX_ATTACHMENTS_PER_MESSAGE);
        assert_eq!(outcome.rejections.len(), 2);
        assert!(outcome.rejections[0].contains("too many attachments"));
        let refused: Vec<_> = outcome
            .references
            .iter()
            .filter(|r| !r.reference.included)
            .collect();
        assert_eq!(refused.len(), 2);
        assert!(
            refused[0]
                .reference
                .detail
                .as_deref()
                .unwrap()
                .starts_with("rejected: too many")
        );
    }

    #[test]
    fn ingest_refuses_a_missing_file_loudly_and_keeps_the_rest() {
        let src = tempdir().unwrap();
        let store_dir = tempdir().unwrap();
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let images = vec![
            PendingAttachment {
                path: src.path().join("gone.png"),
                source: ContextReferenceSource::AtMention,
            },
            pending(src.path(), "ok.png", b"ok"),
        ];

        let outcome = ingest_into(&store, &images, 3);

        assert_eq!(outcome.accepted.len(), 1);
        assert_eq!(outcome.rejections.len(), 1);
        assert!(outcome.rejections[0].contains("not found"));
        assert!(!outcome.references[0].reference.included);
        assert!(outcome.references[1].reference.included);
    }

    #[test]
    fn rejected_reference_keeps_original_path_and_reason() {
        let r = rejected_reference(
            Path::new("/tmp/huge.png"),
            ContextReferenceSource::Attachment,
            4,
            "attachment /tmp/huge.png is 99 bytes; the per-file limit is 1 bytes",
        );
        assert_eq!(r.message_index, 4);
        assert!(!r.reference.included);
        assert_eq!(r.reference.target, "/tmp/huge.png");
        assert_eq!(r.reference.label, "huge.png");
        assert_eq!(r.reference.badge, "image");
        assert!(
            r.reference
                .detail
                .as_deref()
                .unwrap()
                .contains("per-file limit")
        );

        let audio = rejected_reference(
            Path::new("/tmp/track.mp3"),
            ContextReferenceSource::Attachment,
            5,
            "unsupported attachment type .mp3 — audio/video have no consumer in this build",
        );
        assert_eq!(audio.reference.badge, "file");
    }

    /// The smallest bytes each wire-image magic check accepts.
    fn png_magic() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0; 16]);
        bytes
    }

    fn encoded(name: Option<&str>, media_type: Option<&str>, bytes: &[u8]) -> EncodedAttachment {
        EncodedAttachment {
            name: name.map(str::to_string),
            media_type: media_type.map(str::to_string),
            width: Some(3),
            height: Some(2),
            bytes: bytes.to_vec(),
        }
    }

    #[tokio::test]
    async fn encoded_images_take_the_wire_and_carry_their_records() {
        let store_dir = tempdir().unwrap();
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let intake = ingest_encoded_into(
            &store,
            vec![encoded(Some("shot.png"), Some("image/png"), &png_magic())],
        )
        .await
        .expect("a magic-matching PNG admits");

        assert_eq!(
            intake.image_paths.len(),
            1,
            "a wire image hands the engine its store path"
        );
        assert!(intake.image_paths[0].starts_with(store_dir.path()));
        assert!(
            intake.content_appendix.is_empty(),
            "images add no message text"
        );
        assert_eq!(intake.records.len(), 1);
        let record = &intake.records[0];
        assert_eq!(record.kind, "image");
        assert_eq!(record.name.as_deref(), Some("shot.png"));
        assert_eq!(record.media_type, "image/png");
        assert_eq!(record.bytes, png_magic().len() as u64);
        assert_eq!((record.width, record.height), (Some(3), Some(2)));
        assert_eq!(
            record.attachment_id,
            format!("sha256:{}", hex_of(&png_magic()))
        );
    }

    #[tokio::test]
    async fn encoded_files_fold_into_the_message_and_record_their_kind() {
        let store_dir = tempdir().unwrap();
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let intake = ingest_encoded_into(
            &store,
            vec![
                encoded(Some("notes.md"), None, b"# hello\nworld"),
                encoded(None, None, b"\x00\x01binary"),
            ],
        )
        .await
        .expect("text and binary files admit");

        assert!(intake.image_paths.is_empty());
        assert!(
            intake.content_appendix.contains("[Attached file: notes.md"),
            "the appendix labels the file by its display name: {}",
            intake.content_appendix
        );
        assert!(intake.content_appendix.contains("# hello"));
        assert!(
            intake.content_appendix.contains("stored at"),
            "a non-inline binary references its durable path: {}",
            intake.content_appendix
        );
        assert_eq!(intake.records.len(), 2);
        assert!(intake.records.iter().all(|record| record.kind == "file"));
        assert_eq!(intake.records[0].name.as_deref(), Some("notes.md"));
        assert_eq!(
            intake.records[1].name.as_deref(),
            Some("attachment.bin"),
            "a nameless binary gets the fallback leaf, not a temp path"
        );
        assert!(intake.records[0].width.is_none() && intake.records[0].height.is_none());
    }

    #[tokio::test]
    async fn encoded_intake_is_all_or_nothing() {
        let store_dir = tempdir().unwrap();
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        // The .mp3 is refused by the store's classification (audio has no
        // consumer); the whole request fails naming it, even though the PNG
        // alongside it would admit.
        let err = ingest_encoded_into(
            &store,
            vec![
                encoded(Some("shot.png"), Some("image/png"), &png_magic()),
                encoded(Some("voice.mp3"), None, b"id3"),
            ],
        )
        .await
        .expect_err("one refused attachment fails the request");
        assert!(err.contains("voice.mp3"), "{err}");
        assert!(err.contains(".mp3"), "{err}");
        // All-or-nothing governs what is SENT, not the store: an item that
        // admitted before a later refusal stays content-addressed in the
        // store (idempotent, reusable) — but the caller gets nothing usable.
    }

    #[tokio::test]
    async fn encoded_intake_refuses_image_magic_mismatches_before_touching_disk() {
        let store_dir = tempdir().unwrap();
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let err = ingest_encoded_into(
            &store,
            vec![encoded(
                Some("fake.png"),
                Some("image/png"),
                b"not a png at all",
            )],
        )
        .await
        .expect_err("declared image bytes that are not the image must refuse");
        assert!(err.contains("fake.png"), "{err}");
        assert!(err.contains("no attachments were sent"), "{err}");
        assert!(
            store_dir
                .path()
                .read_dir()
                .map(|mut it| it.next().is_none())
                .unwrap_or(false),
            "a magic mismatch never reaches the store"
        );
    }

    #[test]
    fn encoded_extensions_prefer_the_declared_media_type() {
        assert_eq!(encoded_ext(Some("photo.bin"), Some("image/png")), "png");
        assert_eq!(encoded_ext(Some("report.pdf"), None), "pdf");
        assert_eq!(encoded_ext(Some("no-ext"), None), "bin");
        assert_eq!(encoded_ext(None, Some("text/plain")), "bin");
        // A separator-carrying name is refused to the fallback label, so a
        // temp file can never escape its directory.
        assert_eq!(encoded_label(Some("../escape"), "png"), "attachment.png");
        assert_eq!(encoded_label(None, "pdf"), "attachment.pdf");
    }

    fn hex_of(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bytes);
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}
