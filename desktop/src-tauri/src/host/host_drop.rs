//! Host-path drops: the OS drag-drop carried onto the page's composer.
//!
//! The vendored composer cites dropped, non-image files and directories as
//! `@path` references through the shell bridge (`__CODESMITH_HOST_PATHS__`,
//! upstream's Electron preload answers it with `webUtils.getPathForFile`).
//! A webview `File` carries no host path, and with the native drag-drop
//! enabled (Tauri's default) the DOM never sees a file drop at all — wry's
//! `WryWebView` claims `performDragOperation` itself. So this module owns the
//! other half: the window's `DragDrop` event names absolute paths, this
//! module describes each one to the page (`window.__codesmithHostDrop`, an
//! eval call), and the page synthesizes the composer's own paste intake with
//! those paths (see `desktop/ui/src/host-drop.ts`).
//!
//! Images (magic-sniffed png/jpeg/gif/webp, ≤ the upload cap) additionally
//! stage their bytes here for one fetch each — the page builds a real `File`
//! from them so the composer's existing upload pipeline carries the image to
//! the turn attachments. Everything else is cited by path without its bytes
//! ever being read.
//!
//! Known limitations:
//! - a file whose stat fails at drop time (it vanished mid-drag) is skipped
//!   silently — the drop delivers every other member;
//! - an image past the per-file upload cap, or a drop past the total staged
//!   budget, carries no bytes and degrades to a path citation (the composer
//!   never sees it as an image);
//! - drops need an open, idle composer: the page ignores the payload when no
//!   visible `[data-composer-input]` accepts it, and there is no drag-hover
//!   highlight (upstream renders one from DOM drag events, which never fire
//!   here);
//! - the paperclip picker and a real paste stay path-less (no host path
//!   exists for them); they upload as before.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::uploads::MAX_UPLOAD_BYTES;

/// One-shot staged drop bytes by id (`HostState::drop_bytes`). The map is
/// cleared by every new drop — a drop supersedes the previous one — so an
/// unfetched payload never outlives the next gesture. Shared between the
/// window's drag-drop handler (the producer) and the bridge route (the
/// consumer), which live before/under different owners in `lib.rs`.
pub type DropStaging = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Total bytes one drop may stage: the composer's per-message upload cap
/// (`MAX_STAGED_PER_SESSION` files of `MAX_UPLOAD_BYTES` each) — a bigger
/// drop would only error at attach time, so its later members cite by path.
const MAX_DROP_STAGED_BYTES: usize = 8 * MAX_UPLOAD_BYTES;

/// Describe a dropped batch to the page. Directories and files carry their
/// absolute path and display name; an in-cap image carries its media type and
/// a one-shot `bytesId` the page fetches from `/api/host/dropBytes/{id}`.
/// Blocking (metadata, sniffing, image reads); production callers run it on
/// `spawn_blocking`, tests call it inline.
pub fn build_payload(paths: &[PathBuf], staging: &DropStaging) -> Value {
    let mut staged = staging.lock().expect("drop staging poisoned");
    staged.clear();
    let mut staged_total = 0usize;
    let mut files = Vec::new();
    for path in paths {
        let Some(entry) = describe(path, &mut staged, &mut staged_total) else {
            continue;
        };
        files.push(entry);
    }
    json!({ "files": files })
}

/// One dropped path as a `__codesmithHostDrop` entry, or `None` to skip it
/// (it vanished mid-drag — there is nothing honest to cite).
fn describe(
    path: &Path,
    staged: &mut HashMap<String, Vec<u8>>,
    staged_total: &mut usize,
) -> Option<Value> {
    let metadata = std::fs::metadata(path).ok()?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    if metadata.is_dir() {
        return Some(
            json!({ "path": path.display().to_string(), "name": name, "isDirectory": true }),
        );
    }
    // Sniff the media type from the header so an image is only an image when
    // its bytes say so — a renamed `.png` cites by path instead of failing
    // the composer's upload.
    let media_type = sniff_media_type(path);
    let mut entry =
        json!({ "path": path.display().to_string(), "name": name, "isDirectory": false });
    if let Some(media_type) = media_type
        .filter(|_| metadata.len() as usize <= MAX_UPLOAD_BYTES)
        .filter(|_| *staged_total + metadata.len() as usize <= MAX_DROP_STAGED_BYTES)
    {
        if let Ok(bytes) = std::fs::read(path) {
            let id = uuid::Uuid::new_v4().to_string();
            *staged_total += bytes.len();
            staged.insert(id.clone(), bytes);
            entry["mediaType"] = json!(media_type);
            entry["bytesId"] = json!(id);
        }
    }
    Some(entry)
}

/// The composer's accepted raster set (vendored `isImageMediaType`), decided
/// by magic bytes alone: png, jpeg, gif, webp.
fn sniff_media_type(path: &Path) -> Option<&'static str> {
    let mut header = [0u8; 12];
    let read = std::fs::File::open(path)
        .and_then(|mut file| {
            use std::io::Read;
            file.read(&mut header)
        })
        .ok()?;
    let header = &header[..read];
    if header.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if header.starts_with(b"\xFF\xD8\xFF") {
        Some("image/jpeg")
    } else if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if header.starts_with(b"RIFF") && header.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

/// Take one staged drop's bytes (the route serves each id exactly once; a
/// replayed fetch is a gone-by-then drop).
pub fn take_bytes(staging: &DropStaging, id: &str) -> Option<Vec<u8>> {
    staging.lock().expect("drop staging poisoned").remove(id)
}

/// The eval script that hands one payload to the page. The page installs
/// `__codesmithHostDrop` before the client boots; a page without it (still
/// loading) no-ops the call — the user drops again.
pub fn eval_script(payload: &Value) -> String {
    // serde_json output is JSON; as a JS literal only U+2028/U+2029 would
    // need escaping inside string bodies.
    let literal = payload
        .to_string()
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    format!("window.__codesmithHostDrop&&window.__codesmithHostDrop({literal})")
}

/// The eval script announcing a drag gesture's hover state. The DOM never
/// sees native drag events under wry, so the page synthesizes the
/// `dragenter`/`dragleave` the vendored overlay listens for from this call
/// (see `desktop/ui/src/host-drop.ts`).
pub fn hover_script(active: bool) -> String {
    format!("window.__codesmithHostDragHover&&window.__codesmithHostDragHover({active})")
}

/// Deliver one dropped batch: describe it off the async runtime, then hand
/// it to the page. An eval failure (the window closed mid-delivery) is
/// reported, not swallowed.
pub async fn deliver(webview: &tauri::WebviewWindow, staging: &DropStaging, paths: Vec<PathBuf>) {
    let staging = Arc::clone(staging);
    let payload =
        match tauri::async_runtime::spawn_blocking(move || build_payload(&paths, &staging)).await {
            Ok(payload) => payload,
            Err(err) => {
                eprintln!("codesmith desktop: drop description failed: {err}");
                return;
            }
        };
    if let Err(err) = webview.eval(eval_script(&payload)) {
        eprintln!("codesmith desktop: drop delivery failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    /// A temp dir carrying one case per drop shape; returns (dir, paths).
    fn fixture() -> (PathBuf, Vec<PathBuf>) {
        let dir = std::env::temp_dir().join(format!("codesmith-drop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("proj")).unwrap();
        std::fs::write(dir.join("notes.md"), b"# notes").unwrap();
        std::fs::write(dir.join("shot.png"), png(3, 2)).unwrap();
        std::fs::write(dir.join("renamed.png"), b"definitely not a png").unwrap();
        let oversized = dir.join("big.png");
        std::fs::write(&oversized, png(1, 1)).unwrap();
        let mut bytes = std::fs::read(&oversized).unwrap();
        bytes.resize(MAX_UPLOAD_BYTES + 1, 0);
        std::fs::write(&oversized, bytes).unwrap();
        (
            dir.clone(),
            vec![
                dir.join("proj"),
                dir.join("notes.md"),
                dir.join("shot.png"),
                dir.join("renamed.png"),
                dir.join("big.png"),
            ],
        )
    }

    #[test]
    fn a_drop_cites_paths_and_stages_only_in_cap_images() {
        let (dir, paths) = fixture();
        let staging: DropStaging = Default::default();
        let payload = build_payload(&paths, &staging);
        let files = payload["files"].as_array().expect("files array");

        let proj = &files[0];
        assert_eq!(proj["isDirectory"], json!(true));
        assert_eq!(proj["name"], json!("proj"));
        assert_eq!(proj["path"], json!(dir.join("proj").display().to_string()));
        assert!(proj.get("bytesId").is_none());

        let notes = &files[1];
        assert_eq!(notes["isDirectory"], json!(false));
        assert_eq!(notes["name"], json!("notes.md"));
        // A non-image is never read: no media type, no bytes.
        assert!(notes.get("mediaType").is_none());
        assert!(notes.get("bytesId").is_none());

        let shot = &files[2];
        assert_eq!(shot["mediaType"], json!("image/png"));
        let shot_id = shot["bytesId"].as_str().expect("in-cap image stages bytes");
        assert_eq!(
            staging.lock().unwrap().get(shot_id).map(Vec::len),
            Some(png(3, 2).len()),
            "the staged bytes are the file's own"
        );

        // A renamed non-image under a .png name cites by path.
        assert!(files[3].get("mediaType").is_none());
        assert!(files[3].get("bytesId").is_none());
        // An image past the upload cap degrades to a citation.
        assert!(files[4].get("mediaType").is_none());
        assert!(files[4].get("bytesId").is_none());

        // A vanished member is skipped, not cited as a ghost.
        let (gone, kept) = (dir.join("gone.md"), dir.join("notes.md"));
        let payload = build_payload(&[gone, kept], &staging);
        let files = payload["files"].as_array().unwrap();
        assert_eq!(files.len(), 1, "{payload}");
        assert_eq!(files[0]["name"], json!("notes.md"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_drop_supersedes_the_previous_staging() {
        let (dir, paths) = fixture();
        let staging: DropStaging = Default::default();
        build_payload(&paths, &staging);
        let first: Vec<String> = staging.lock().unwrap().keys().cloned().collect();
        assert!(!first.is_empty(), "the image staged its bytes");
        build_payload(&[dir.join("notes.md")], &staging);
        assert!(
            staging.lock().unwrap().is_empty(),
            "the second drop cleared the first's bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staged_bytes_serve_exactly_once() {
        let staging: DropStaging = Default::default();
        staging
            .lock()
            .unwrap()
            .insert("id".into(), b"png-bytes".to_vec());
        assert_eq!(
            take_bytes(&staging, "id").as_deref(),
            Some(b"png-bytes".as_ref())
        );
        assert!(
            take_bytes(&staging, "id").is_none(),
            "a replayed fetch answers nothing: the id is spent"
        );
        assert!(take_bytes(&staging, "other").is_none());
    }

    #[test]
    fn the_eval_script_embeds_the_payload_and_escapes_line_separators() {
        let payload = json!({ "files": [{
            "path": "/tmp/a\u{2028}b\"c\\d.md", "name": "b\"c\\d.md", "isDirectory": false,
        }]});
        let script = eval_script(&payload);
        assert!(script.starts_with("window.__codesmithHostDrop&&window.__codesmithHostDrop("));
        assert!(script.ends_with(")"));
        // The raw separator must not survive into the literal.
        assert!(!script.contains('\u{2028}'));
        assert!(script.contains("\\u2028"));
        // Quotes and backslashes ride serde's JSON escaping.
        assert!(script.contains("\\\"c\\\\d.md"));
    }
}
