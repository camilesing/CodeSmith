//! Media attachment intake for the REPL (TODO-50) and the stable storage
//! location for pasted images.
//!
//! Intake is the CLI-side consumer of the content-addressed store
//! (`codesmith_agent_runtime::attachments`): every image a message carries —
//! `/attach`ed or `@path`-mentioned — is admitted through
//! [`ingest_for_message`] at send time, which copies it into
//! `~/.codesmith/attachments/<sha256>.<ext>` and hands the engine the store
//! path. The same call produces the session's context-reference record for
//! each image, admitted (`included: true`, with hash/size/type) or refused
//! (`included: false`, with the reason) — so the record says exactly what
//! reached the model and what was turned away.
//!
//! Known limitations: `/attach` runs only the metadata-level
//! [`AttachmentStore::precheck`] inline (the REPL loop is on the runtime; the
//! hash + copy is blocking I/O and happens under `spawn_blocking` here), so a
//! file that changes between `/attach` and send is admitted on its send-time
//! bytes.

use std::path::{Path, PathBuf};

use codesmith_agent_runtime::attachments::{AttachmentStore, StoredAttachment};
use codesmith_agent_runtime::session_store::{
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

/// One image slated for the next message, tagged with how the user named it.
#[derive(Debug, Clone)]
pub struct PendingImage {
    pub path: PathBuf,
    pub source: ContextReferenceSource,
}

/// Result of admitting one message's images into the store.
#[derive(Debug, Default)]
pub struct IntakeOutcome {
    /// Store paths to hand the engine (`Op::SendMessage.image_paths`), in
    /// input order.
    pub accepted: Vec<PathBuf>,
    /// Session record for every image — admitted or refused.
    pub references: Vec<SessionContextReference>,
    /// User-facing refusal lines; the caller prints them.
    pub rejections: Vec<String>,
}

/// Admit `images` for the message that will land at `message_index`: the
/// per-message cap first (images beyond it are refused, not silently
/// dropped), then each file through [`AttachmentStore::ingest`]. Blocking
/// I/O — run under `spawn_blocking` on the runtime ([`ingest_for_message`]).
pub fn ingest_into(
    store: &AttachmentStore,
    images: &[PendingImage],
    message_index: usize,
) -> IntakeOutcome {
    let mut outcome = IntakeOutcome::default();
    for (position, image) in images.iter().enumerate() {
        let admitted = AttachmentStore::check_message_count(position + 1)
            .and_then(|()| store.ingest(&image.path));
        match admitted {
            Ok(stored) => {
                outcome.references.push(admitted_reference(
                    &image.path,
                    &stored,
                    image.source,
                    message_index,
                ));
                outcome.accepted.push(stored.path);
            }
            Err(err) => {
                outcome.rejections.push(format!(
                    "attachment refused: {err} ({})",
                    image.path.display()
                ));
                outcome.references.push(rejected_reference(
                    &image.path,
                    image.source,
                    message_index,
                    &err.to_string(),
                ));
            }
        }
    }
    outcome
}

/// [`ingest_into`] against the user store, off the runtime thread. A store
/// that cannot be resolved refuses every image loudly (recorded) rather than
/// handing the engine the original paths.
pub async fn ingest_for_message(images: Vec<PendingImage>, message_index: usize) -> IntakeOutcome {
    if images.is_empty() {
        return IntakeOutcome::default();
    }
    let store = match AttachmentStore::default_location() {
        Ok(store) => store,
        Err(err) => return refuse_all(&images, message_index, &err.to_string()),
    };
    // Keep the attempt list for the join-failure path: the outcome must
    // record a session reference for every attempted image (path, source,
    // reason) on ALL refusal paths — an empty `references` vec silently
    // loses the trace of what was turned away.
    let attempted = images.clone();
    match tokio::task::spawn_blocking(move || ingest_into(&store, &images, message_index)).await {
        Ok(outcome) => outcome,
        Err(join_err) => refuse_all(
            &attempted,
            message_index,
            &format!("intake worker failed: {join_err}; no images were sent"),
        ),
    }
}

fn refuse_all(
    images: &[PendingImage],
    message_index: usize,
    reason: &str,
) -> IntakeOutcome {
    let mut outcome = IntakeOutcome::default();
    for image in images {
        outcome.rejections.push(format!(
            "attachment refused: {reason} ({})",
            image.path.display()
        ));
        outcome.references.push(rejected_reference(
            &image.path,
            image.source,
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
            badge: "image".to_string(),
            label: file_label(original),
            target: stored.path.display().to_string(),
            included: true,
            expanded: false,
            detail: Some(stored.detail()),
        },
    }
}

/// Record an image that was refused (cap, type, size, missing file) so the
/// session file keeps the trace — `included: false` with the reason.
pub fn rejected_reference(
    original: &Path,
    source: ContextReferenceSource,
    message_index: usize,
    reason: &str,
) -> SessionContextReference {
    SessionContextReference {
        message_index,
        reference: ContextReference {
            kind: ContextReferenceKind::MediaAttachment,
            source,
            badge: "image".to_string(),
            label: file_label(original),
            target: original.display().to_string(),
            included: false,
            expanded: false,
            detail: Some(format!("rejected: {reason}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent_runtime::attachments::MAX_ATTACHMENTS_PER_MESSAGE;
    use tempfile::tempdir;

    fn png(dir: &Path, name: &str, bytes: &[u8]) -> PendingImage {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        PendingImage {
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
            png(src.path(), "a.png", b"aaa"),
            PendingImage {
                source: ContextReferenceSource::AtMention,
                ..png(src.path(), "b.png", b"bbb")
            },
        ];

        let outcome = ingest_into(&store, &images, 7);

        assert_eq!(outcome.accepted.len(), 2);
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
    fn ingest_refuses_images_beyond_the_per_message_cap_and_records_each() {
        let src = tempdir().unwrap();
        let store_dir = tempdir().unwrap();
        let store = AttachmentStore::new(store_dir.path().to_path_buf());
        let images: Vec<PendingImage> = (0..MAX_ATTACHMENTS_PER_MESSAGE + 2)
            .map(|i| {
                png(
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
            PendingImage {
                path: src.path().join("gone.png"),
                source: ContextReferenceSource::AtMention,
            },
            png(src.path(), "ok.png", b"ok"),
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
        assert!(
            r.reference
                .detail
                .as_deref()
                .unwrap()
                .contains("per-file limit")
        );
    }
}
