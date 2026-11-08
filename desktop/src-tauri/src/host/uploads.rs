//! `fileUploads/upload`, the raw-bytes upload route, and `session/attachment`
//! — the composer's attachment staging, bridged onto the runtime's turn
//! attachments (`POST /v1/threads/{id}/turns` `attachments[]`).
//!
//! Wire contracts (vendored `client-file-upload` + `api-remotes`): every
//! image (and every Blob-bodied file) posts to
//! `/api/session/uploadFileBinary?sessionId&name&mediaType` as
//! `application/octet-stream` — the page installs
//! `__CODESMITH_FILE_UPLOAD__.fetch` to carry that route to this bridge, so
//! no worker posts to the page origin; exact-byte callers use the
//! `fileUploads/upload` RPC (`{agentId, request:{data: base64, name?}}`).
//! An image staging answers `{receiptId, image:{attachmentId, mediaType,
//! bytes, width, height, name?}}` (width/height are REQUIRED integers — the
//! client validates them), a file staging `{receiptId, file:{attachmentId,
//! name, bytes}}`. Business failures are HTTP 200 with `{ok:false,
//! error:{code, message, details}}`: to this client a non-200 is a transport
//! failure, not an answer.
//!
//! `attachmentId` is `sha256:<hex>` of the bytes — the content address the
//! runtime's attachment store files them under — so `session/attachment`
//! (the client re-fetches image bytes with the full reference to render a
//! settled message) reads the durable store, and history renders across
//! bridge restarts.
//!
//! Receipts are session-bound and in-memory (upstream stages them in a
//! per-Session WeakMap the same way): a bridge restart drops staged,
//! not-yet-sent uploads, and they must be attached again. A prompt reads its
//! receipts and retires them only once the runtime accepted the prompt —
//! parked on the thread's queue or started — so a refused send (a refused
//! attachment) can simply be retried.
//!
//! Known limitations:
//! - no upload progress (the vendored hook transport drops `onProgress`;
//!   upstream's own hook path does too);
//! - image dimensions come from header parsing only — no EXIF orientation,
//!   no normalization scaling; bytes travel to the runtime untouched;
//! - a queued prompt's bytes already live in the runtime's store when its
//!   receipts retire, so a later queue edit/remove has nothing staged left
//!   to retire (upstream retires on remove because it stages until then);
//! - the staging caps mirror the runtime store's (10MB per file, 8 per
//!   message) so refusals surface at attach time, not at send time; a body
//!   past the bridge's 16MB request limit is refused by the transport (413)
//!   before any business check runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::workspace::OpError;

/// The runtime attachment store's per-file cap (`MAX_ATTACHMENT_BYTES`,
/// crates/agent-runtime/src/attachments.rs), mirrored so a too-large upload
/// fails at attach time with a business error, not at send time.
pub const MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024;

/// The runtime store's per-message attachment cap
/// (`MAX_ATTACHMENTS_PER_MESSAGE`), applied per session to staged receipts.
const MAX_STAGED_PER_SESSION: usize = 8;

const IMAGE_MEDIA_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Staged uploads by receipt id (`HostState::uploads`).
pub type Staging = Mutex<HashMap<String, StagedUpload>>;

/// One staged upload, awaiting the prompt that consumes it.
#[derive(Debug, Clone)]
pub struct StagedUpload {
    pub session_id: String,
    pub name: String,
    pub media_type: Option<String>,
    pub bytes: Vec<u8>,
    pub width: Option<u64>,
    pub height: Option<u64>,
}

/// The content-addressed id of some bytes: the form the runtime's store files
/// them under (`sha256:<hex>`).
pub fn attachment_id(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// The runtime's attachment store root, by `codesmith_config`'s own rule
/// (`resolve_state_dir("attachments")`): `CODESMITH_HOME` names the CodeSmith
/// root itself, else `~/.codesmith`. The runtime child inherits the bridge's
/// environment, so both sides resolve the same directory.
pub fn attachment_store_root() -> Option<PathBuf> {
    let root = match std::env::var("CODESMITH_HOME") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value.trim()),
        _ => dirs::home_dir()?.join(".codesmith"),
    };
    Some(root.join("attachments"))
}

fn op_error(code: &'static str, message: impl Into<String>) -> OpError {
    OpError {
        code,
        message: message.into(),
    }
}

fn arguments_invalid(message: impl Into<String>) -> OpError {
    op_error("gateway/arguments-invalid", message)
}

/// Stage `bytes` for `session_id` and mint the receipt. `media_type` Some →
/// image staging (validated media type, dimensions parsed from the header);
/// None → file staging. Returns the receipt id and the staged reference
/// (`{image:{…}}` or `{file:{…}}`).
async fn stage(
    staging: &Staging,
    session_id: &str,
    name: Option<&str>,
    media_type: Option<&str>,
    bytes: Vec<u8>,
) -> Result<(String, Value), OpError> {
    let ext = media_type.map_or("bin", extension_of);
    let name = sanitize_name(name, ext);
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(op_error(
            "fileUploads/attachment-too-large",
            format!(
                "{name} is {} bytes; the per-file limit is {MAX_UPLOAD_BYTES} bytes",
                bytes.len()
            ),
        ));
    }
    let (reference, width, height) = match media_type {
        Some(media_type) => {
            if !IMAGE_MEDIA_TYPES.contains(&media_type) {
                return Err(op_error(
                    "fileUploads/unsupported-media-type",
                    format!("{media_type} is not an accepted image media type"),
                ));
            }
            let (width, height) = parse_dimensions(&bytes, media_type).ok_or_else(|| {
                op_error(
                    "fileUploads/invalid-image",
                    format!("{name} is not decodable {media_type} image data"),
                )
            })?;
            let reference = json!({
                "image": {
                    "attachmentId": attachment_id(&bytes),
                    "mediaType": media_type,
                    "bytes": bytes.len(),
                    "width": width,
                    "height": height,
                    "name": name,
                }
            });
            (reference, Some(width), Some(height))
        }
        None => {
            let reference = json!({
                "file": {
                    "attachmentId": attachment_id(&bytes),
                    "name": name,
                    "bytes": bytes.len(),
                }
            });
            (reference, None, None)
        }
    };
    // Count and insert under one lock: two concurrent uploads must not both
    // pass the cap.
    let mut staged = staging.lock().await;
    let held = staged
        .values()
        .filter(|upload| upload.session_id == session_id)
        .count();
    if held >= MAX_STAGED_PER_SESSION {
        return Err(op_error(
            "fileUploads/too-many-staged",
            format!(
                "this conversation already holds {held} staged attachments (the per-message limit); send or remove some first"
            ),
        ));
    }
    let receipt_id = uuid::Uuid::new_v4().to_string();
    staged.insert(
        receipt_id.clone(),
        StagedUpload {
            session_id: session_id.to_string(),
            name,
            media_type: media_type.map(str::to_string),
            bytes,
            width,
            height,
        },
    );
    Ok((receipt_id, reference))
}

/// `fileUploads/upload` — the exact-byte RPC path (`{agentId, request:{data,
/// name?}}`). Blobs never take it (they post raw bytes to the binary route);
/// the staging is the same.
pub async fn upload_rpc(staging: &Staging, args: &Value) -> Result<Value, OpError> {
    let session_id = args
        .get("agentId")
        .and_then(Value::as_str)
        .ok_or_else(|| arguments_invalid("fileUploads/upload without agentId"))?;
    let request = args
        .get("request")
        .ok_or_else(|| arguments_invalid("fileUploads/upload without request"))?;
    let data = request
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| arguments_invalid("fileUploads/upload without request.data"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|err| arguments_invalid(format!("request.data is not canonical base64: {err}")))?;
    let name = request.get("name").and_then(Value::as_str);
    let (receipt_id, reference) = stage(staging, session_id, name, None, bytes).await?;
    Ok(json!({ "receiptId": receipt_id, "file": reference["file"] }))
}

/// The binary upload route's answer, in the envelope the vendored client
/// parses (`parseUploadResult`): always HTTP 200, business failures inside
/// `{ok:false, …}`. `query` is the raw query string.
pub async fn upload_binary(staging: &Staging, query: &str, bytes: &[u8]) -> Value {
    let Some(session_id) = query_param(query, "sessionId") else {
        return upload_failure(&arguments_invalid("upload without sessionId"));
    };
    let name = query_param(query, "name");
    let media_type = query_param(query, "mediaType");
    match stage(
        staging,
        &session_id,
        name.as_deref(),
        media_type.as_deref(),
        bytes.to_vec(),
    )
    .await
    {
        Ok((receipt_id, reference)) => {
            let mut value = Map::new();
            value.insert("receiptId".into(), json!(receipt_id));
            if let Value::Object(staged) = reference {
                value.extend(staged);
            }
            json!({ "ok": true, "value": value })
        }
        Err(err) => upload_failure(&err),
    }
}

fn upload_failure(err: &OpError) -> Value {
    json!({ "ok": false, "error": { "code": err.code, "message": err.message, "details": {} } })
}

/// The runtime turn attachments a prompt's `file` parts name, in order. Read
/// only: the receipts stay staged until [`retire_receipts`] runs after the
/// runtime accepted the turn. Every receipt is checked before any is used —
/// an unknown one (a bridge restart dropped it) or one staged for another
/// session fails the whole prompt loudly.
pub async fn resolve_prompt_attachments(
    staging: &Staging,
    session_id: &str,
    receipt_ids: &[String],
) -> Result<Vec<Value>, OpError> {
    let staged = staging.lock().await;
    receipt_ids
        .iter()
        .map(|receipt_id| match staged.get(receipt_id) {
            Some(upload) if upload.session_id == session_id => Ok(runtime_attachment(upload)),
            Some(_) => Err(op_error(
                "session/file-not-staged",
                format!("attachment receipt {receipt_id} belongs to another conversation"),
            )),
            None => Err(op_error(
                "session/file-not-staged",
                format!(
                    "attachment receipt {receipt_id} is not staged (uploads do not survive a desktop restart — attach the file again)"
                ),
            )),
        })
        .collect()
}

/// Retire the receipts an accepted prompt consumed (upstream retires a
/// receipt once the prompt binds it).
pub async fn retire_receipts(staging: &Staging, receipt_ids: &[String]) {
    let mut staged = staging.lock().await;
    for receipt_id in receipt_ids {
        staged.remove(receipt_id);
    }
}

/// One staged upload as a runtime `attachments[]` member (snake_case,
/// canonical base64).
fn runtime_attachment(upload: &StagedUpload) -> Value {
    json!({
        "data": base64::engine::general_purpose::STANDARD.encode(&upload.bytes),
        "name": upload.name,
        "media_type": upload.media_type,
        "width": upload.width,
        "height": upload.height,
    })
}

/// An inline prompt image (`{type:'image', mediaType, data, name?}` — the
/// composer sends one when an image draft was never staged) as a runtime
/// attachment: same media-type, size and decodability checks as a staged
/// image, with its dimensions parsed here so the settled message can render.
pub fn inline_image_attachment(
    media_type: Option<&str>,
    data: Option<&str>,
    name: Option<&str>,
) -> Result<Value, OpError> {
    let media_type = media_type
        .filter(|media_type| IMAGE_MEDIA_TYPES.contains(media_type))
        .ok_or_else(|| arguments_invalid("image prompt part without an accepted mediaType"))?;
    let data = data.ok_or_else(|| arguments_invalid("image prompt part without data"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|err| {
            arguments_invalid(format!(
                "image prompt part data is not canonical base64: {err}"
            ))
        })?;
    let name = sanitize_name(name, extension_of(media_type));
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(op_error(
            "fileUploads/attachment-too-large",
            format!(
                "{name} is {} bytes; the per-file limit is {MAX_UPLOAD_BYTES} bytes",
                bytes.len()
            ),
        ));
    }
    let (width, height) = parse_dimensions(&bytes, media_type).ok_or_else(|| {
        op_error(
            "fileUploads/invalid-image",
            format!("{name} is not decodable {media_type} image data"),
        )
    })?;
    Ok(json!({
        "data": data,
        "name": name,
        "media_type": media_type,
        "width": width,
        "height": height,
    }))
}

/// `session/attachment` — re-fetch one image's bytes for rendering. The
/// client holds the full reference (it rode the settled message); the bytes
/// live in the runtime's durable content-addressed store under
/// `<sha256>.<ext>` — `ext` is the attached file's own extension, so jpeg
/// tries both spellings. The answer echoes the reference with the base64
/// bytes.
pub async fn attachment(store_root: &Path, args: &Value) -> Result<Value, OpError> {
    let request = args.get("request").unwrap_or(args);
    let reference = request
        .get("attachment")
        .ok_or_else(|| arguments_invalid("session/attachment without attachment"))?;
    let id = reference
        .get("attachmentId")
        .and_then(Value::as_str)
        .ok_or_else(|| arguments_invalid("session/attachment without attachmentId"))?;
    let hex = id
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| {
            op_error(
                "fileUploads/unknown-attachment",
                format!("{id} is not an attachment store content address"),
            )
        })?;
    let media_type = reference
        .get("mediaType")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let extensions: &[&str] = match media_type {
        "image/png" => &["png"],
        "image/jpeg" => &["jpg", "jpeg"],
        "image/gif" => &["gif"],
        "image/webp" => &["webp"],
        other => {
            return Err(arguments_invalid(format!(
                "session/attachment serves images only, not {other:?}"
            )))
        }
    };
    for ext in extensions {
        let Ok(bytes) = tokio::fs::read(store_root.join(format!("{hex}.{ext}"))).await else {
            continue;
        };
        let claimed = reference.get("bytes").and_then(Value::as_u64);
        if claimed.is_some_and(|claimed| claimed != bytes.len() as u64) {
            return Err(op_error(
                "fileUploads/attachment-mismatch",
                format!(
                    "{id} stores {} bytes, not the {claimed:?} its reference claims",
                    bytes.len()
                ),
            ));
        }
        return Ok(json!({
            "attachment": reference,
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
        }));
    }
    Err(op_error(
        "fileUploads/unknown-attachment",
        format!("no stored {media_type} attachment for {id}"),
    ))
}

/// A display name the transcript can carry: the caller's name when it is a
/// usable leaf (separator-carrying and dot names are refused, not mangled
/// into a path), else `upload.<ext>`.
fn sanitize_name(name: Option<&str>, ext: &str) -> String {
    match name {
        Some(name)
            if !name.is_empty()
                && name != "."
                && name != ".."
                && !name.contains(['/', '\\', '\0']) =>
        {
            name.chars().take(255).collect()
        }
        _ => format!("upload.{ext}"),
    }
}

fn extension_of(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    }
}

/// One query parameter, percent-decoded (`+` reads as a space).
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(k) == key).then(|| percent_decode(v))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (escaped, bytes[i]) {
            (Some(byte), _) => {
                out.push(byte);
                i += 3;
            }
            (None, b'+') => {
                out.push(b' ');
                i += 1;
            }
            (None, byte) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn be_u16(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?).into())
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?).into())
}

fn le_u24(bytes: &[u8], at: usize) -> Option<u64> {
    let b = bytes.get(at..at + 3)?;
    Some(u64::from(b[0]) | (u64::from(b[1]) << 8) | (u64::from(b[2]) << 16))
}

/// An image's encoded pixel dimensions, from its header alone (no decode, no
/// image dependency): PNG IHDR, GIF logical screen descriptor, JPEG frame
/// header (SOFn segment scan), WEBP's first chunk (VP8 keyframe, VP8L, VP8X
/// canvas). `None` when the bytes are not the declared format — the caller
/// refuses them as invalid image data.
fn parse_dimensions(bytes: &[u8], media_type: &str) -> Option<(u64, u64)> {
    match media_type {
        "image/png" => {
            if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.get(12..16)? != b"IHDR" {
                return None;
            }
            let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
            let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
            Some((width.into(), height.into()))
        }
        "image/gif" => {
            if !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
                return None;
            }
            Some((le_u16(bytes, 6)?, le_u16(bytes, 8)?))
        }
        "image/jpeg" => {
            if !bytes.starts_with(b"\xFF\xD8") {
                return None;
            }
            // Walk the marker segments from after SOI to the first frame
            // header; `pos` always sits on a 0xFF marker prefix.
            let mut pos = 2usize;
            loop {
                if *bytes.get(pos)? != 0xFF {
                    return None;
                }
                let marker = *bytes.get(pos + 1)?;
                match marker {
                    // Fill bytes before a marker.
                    0xFF => pos += 1,
                    // Standalone markers carry no length.
                    0x01 | 0xD0..=0xD8 => pos += 2,
                    // EOI before any frame header: not an image.
                    0xD9 => return None,
                    // SOFn (excluding DHT C4, JPG C8, DAC CC): precision,
                    // then height and width, big-endian.
                    0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                        return Some((be_u16(bytes, pos + 7)?, be_u16(bytes, pos + 5)?));
                    }
                    _ => pos += 2 + usize::try_from(be_u16(bytes, pos + 2)?).ok()?,
                }
            }
        }
        "image/webp" => {
            if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WEBP" {
                return None;
            }
            // The first chunk: fourcc at 12, size at 16, data from 20.
            match bytes.get(12..16)? {
                // Lossy keyframe: 3-byte frame tag, the 9d 01 2a start code,
                // then 14-bit little-endian width and height.
                b"VP8 " => {
                    if bytes.get(23..26)? != [0x9D, 0x01, 0x2A] {
                        return None;
                    }
                    Some((le_u16(bytes, 26)? & 0x3FFF, le_u16(bytes, 28)? & 0x3FFF))
                }
                // Lossless: the 0x2f signature, then width-1 and height-1 as
                // 14-bit fields of one little-endian word.
                b"VP8L" => {
                    if *bytes.get(20)? != 0x2F {
                        return None;
                    }
                    let bits = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
                    Some((
                        u64::from(bits & 0x3FFF) + 1,
                        u64::from((bits >> 14) & 0x3FFF) + 1,
                    ))
                }
                // Extended: flags + 3 reserved bytes, then the canvas width-1
                // and height-1 as 24-bit little-endian values.
                b"VP8X" => Some((le_u24(bytes, 24)? + 1, le_u24(bytes, 27)? + 1)),
                _ => None,
            }
        }
        _ => None,
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

    fn webp(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(fourcc);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    }

    #[test]
    fn attachment_ids_are_the_store_content_address() {
        // sha256("abc"): the runtime's store files these bytes under the
        // same hex, so the id doubles as the store key.
        assert_eq!(
            attachment_id(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[tokio::test]
    async fn an_image_stages_with_its_dimensions_and_resolves_until_retired() {
        let staging = Staging::default();
        let image = png(3, 2);
        let answer = upload_binary(
            &staging,
            "sessionId=s1&name=shot%20two.png&mediaType=image%2Fpng",
            &image,
        )
        .await;
        assert_eq!(answer["ok"], json!(true), "{answer}");
        let value = &answer["value"];
        assert_eq!(value["image"]["width"], json!(3));
        assert_eq!(value["image"]["height"], json!(2));
        assert_eq!(value["image"]["mediaType"], json!("image/png"));
        assert_eq!(value["image"]["name"], json!("shot two.png"));
        assert_eq!(value["image"]["bytes"], json!(image.len()));
        assert_eq!(value["image"]["attachmentId"], json!(attachment_id(&image)));
        let receipt = value["receiptId"].as_str().unwrap().to_string();

        // Resolving is read-only: a refused send can be retried.
        for _ in 0..2 {
            let resolved =
                resolve_prompt_attachments(&staging, "s1", std::slice::from_ref(&receipt))
                    .await
                    .unwrap_or_else(|err| panic!("{}", err.message));
            assert_eq!(resolved[0]["media_type"], json!("image/png"));
            assert_eq!(resolved[0]["name"], json!("shot two.png"));
            assert_eq!(resolved[0]["width"], json!(3));
            assert_eq!(
                resolved[0]["data"],
                json!(base64::engine::general_purpose::STANDARD.encode(&image))
            );
        }
        retire_receipts(&staging, std::slice::from_ref(&receipt)).await;
        let Err(err) = resolve_prompt_attachments(&staging, "s1", &[receipt]).await else {
            panic!("a retired receipt must fail loudly, not resolve to nothing");
        };
        assert_eq!(err.code, "session/file-not-staged");
    }

    #[tokio::test]
    async fn a_file_stages_under_a_leaf_name() {
        let staging = Staging::default();
        let answer = upload_binary(&staging, "sessionId=s1&name=notes.txt", b"data").await;
        assert_eq!(answer["ok"], json!(true), "{answer}");
        assert_eq!(answer["value"]["file"]["name"], json!("notes.txt"));
        assert_eq!(answer["value"]["file"]["bytes"], json!(4));
        assert!(answer["value"].get("image").is_none());

        // A traversal-carrying name is refused into a fresh leaf.
        let answer = upload_binary(&staging, "sessionId=s1&name=..%2F..%2Fetc", b"x").await;
        assert_eq!(answer["value"]["file"]["name"], json!("upload.bin"));
    }

    #[tokio::test]
    async fn refusals_ride_ok_false_envelopes_and_stage_nothing() {
        let staging = Staging::default();
        let cases: [(&str, &[u8], &str); 4] = [
            ("name=x.png", b"x", "gateway/arguments-invalid"),
            (
                "sessionId=s1&mediaType=image%2Fheic",
                b"x",
                "fileUploads/unsupported-media-type",
            ),
            (
                "sessionId=s1&mediaType=image%2Fpng",
                b"not a png",
                "fileUploads/invalid-image",
            ),
            (
                "sessionId=s1&mediaType=image%2Fgif",
                b"GIF8",
                "fileUploads/invalid-image",
            ),
        ];
        for (query, bytes, code) in cases {
            let answer = upload_binary(&staging, query, bytes).await;
            assert_eq!(answer["ok"], json!(false), "{query}");
            assert_eq!(answer["error"]["code"], json!(code), "{query}");
            assert!(
                answer["error"]["details"].is_object(),
                "the client requires a details record"
            );
        }
        let oversized = vec![0u8; MAX_UPLOAD_BYTES + 1];
        let answer = upload_binary(&staging, "sessionId=s1&name=big.bin", &oversized).await;
        assert_eq!(
            answer["error"]["code"],
            json!("fileUploads/attachment-too-large")
        );
        assert!(staging.lock().await.is_empty());
    }

    #[tokio::test]
    async fn staging_is_capped_per_session() {
        let staging = Staging::default();
        for i in 0..MAX_STAGED_PER_SESSION {
            let answer = upload_binary(&staging, "sessionId=s1", format!("{i}").as_bytes()).await;
            assert_eq!(answer["ok"], json!(true), "staging {i} fits");
        }
        let over = upload_binary(&staging, "sessionId=s1", b"one too many").await;
        assert_eq!(over["error"]["code"], json!("fileUploads/too-many-staged"));
        let other = upload_binary(&staging, "sessionId=s2", b"fine").await;
        assert_eq!(
            other["ok"],
            json!(true),
            "another session keeps its own cap"
        );
    }

    #[tokio::test]
    async fn a_receipt_resolves_only_for_its_own_session() {
        let staging = Staging::default();
        let answer = upload_binary(&staging, "sessionId=s1&name=a.txt", b"a").await;
        let receipt = answer["value"]["receiptId"].as_str().unwrap().to_string();
        let Err(err) =
            resolve_prompt_attachments(&staging, "s2", std::slice::from_ref(&receipt)).await
        else {
            panic!("a foreign receipt must not resolve");
        };
        assert_eq!(err.code, "session/file-not-staged");
        assert!(
            staging.lock().await.contains_key(&receipt),
            "s1 still owns it"
        );
    }

    #[tokio::test]
    async fn the_rpc_path_stages_base64_as_a_file() {
        let staging = Staging::default();
        let args =
            json!({ "agentId": "s1", "request": { "data": "aGVsbG8=", "name": "hello.txt" } });
        let value = upload_rpc(&staging, &args)
            .await
            .unwrap_or_else(|err| panic!("{}", err.message));
        assert_eq!(value["file"]["name"], json!("hello.txt"));
        assert_eq!(value["file"]["bytes"], json!(5));
        assert!(value["receiptId"].as_str().is_some_and(|id| !id.is_empty()));
        let bad = json!({ "agentId": "s1", "request": { "data": "!!!" } });
        let Err(err) = upload_rpc(&staging, &bad).await else {
            panic!("non-base64 data must be refused");
        };
        assert_eq!(err.code, "gateway/arguments-invalid");
    }

    #[test]
    fn an_inline_image_part_becomes_a_runtime_attachment() {
        let data = base64::engine::general_purpose::STANDARD.encode(png(5, 4));
        let value = inline_image_attachment(Some("image/png"), Some(&data), None)
            .unwrap_or_else(|err| panic!("{}", err.message));
        assert_eq!(value["media_type"], json!("image/png"));
        assert_eq!(value["name"], json!("upload.png"));
        assert_eq!(
            (value["width"].clone(), value["height"].clone()),
            (json!(5), json!(4))
        );
        assert_eq!(value["data"], json!(data));
        let Err(err) = inline_image_attachment(Some("image/png"), Some("aGVsbG8="), None) else {
            panic!("non-PNG bytes under image/png must be refused");
        };
        assert_eq!(err.code, "fileUploads/invalid-image");
        assert!(inline_image_attachment(Some("image/heic"), Some(&data), None).is_err());
    }

    #[tokio::test]
    async fn session_attachment_reads_the_durable_store() {
        let root = std::env::temp_dir().join(format!("codesmith-uploads-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let image = png(2, 2);
        let id = attachment_id(&image);
        let hex = id.trim_start_matches("sha256:").to_string();
        // A jpeg stored under its `.jpeg` spelling must still be found.
        std::fs::write(root.join(format!("{hex}.jpeg")), &image).unwrap();
        let reference = json!({
            "attachmentId": id, "mediaType": "image/jpeg",
            "bytes": image.len(), "width": 2, "height": 2,
        });
        let args = json!({ "request": { "sessionId": "s1", "attachment": reference } });
        let value = attachment(&root, &args)
            .await
            .unwrap_or_else(|err| panic!("{}", err.message));
        assert_eq!(value["attachment"], reference, "the reference is echoed");
        assert_eq!(
            value["data"],
            json!(base64::engine::general_purpose::STANDARD.encode(&image))
        );

        let mut wrong = reference.clone();
        wrong["bytes"] = json!(1);
        let args = json!({ "request": { "sessionId": "s1", "attachment": wrong } });
        let Err(err) = attachment(&root, &args).await else {
            panic!("a byte-count mismatch must be refused");
        };
        assert_eq!(err.code, "fileUploads/attachment-mismatch");

        let mut missing = reference.clone();
        missing["mediaType"] = json!("image/png");
        let args = json!({ "request": { "sessionId": "s1", "attachment": missing } });
        let Err(err) = attachment(&root, &args).await else {
            panic!("an absent store file must be refused");
        };
        assert_eq!(err.code, "fileUploads/unknown-attachment");

        let args = json!({ "request": { "attachment": { "attachmentId": "../../etc/passwd", "mediaType": "image/png" } } });
        let Err(err) = attachment(&root, &args).await else {
            panic!("a non-address id must never reach the filesystem");
        };
        assert_eq!(err.code, "fileUploads/unknown-attachment");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dimensions_parse_from_every_accepted_format_header() {
        assert_eq!(
            parse_dimensions(&png(1920, 1080), "image/png"),
            Some((1920, 1080))
        );

        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&640u16.to_le_bytes());
        gif.extend_from_slice(&480u16.to_le_bytes());
        assert_eq!(parse_dimensions(&gif, "image/gif"), Some((640, 480)));

        // SOI, an APP0 segment, fill bytes, then SOF0 carrying 32 wide x 16 high.
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xFF];
        jpg.extend_from_slice(&[0xC0, 0x00, 0x11, 0x08]);
        jpg.extend_from_slice(&16u16.to_be_bytes());
        jpg.extend_from_slice(&32u16.to_be_bytes());
        jpg.extend_from_slice(&[0x03, 0x01, 0x22, 0x00]);
        assert_eq!(parse_dimensions(&jpg, "image/jpeg"), Some((32, 16)));
        // A DHT segment (C4) is not a frame header and is skipped.
        let mut jpg_dht = vec![0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x03, 0x00];
        jpg_dht.extend_from_slice(&jpg[8..]);
        assert_eq!(parse_dimensions(&jpg_dht, "image/jpeg"), Some((32, 16)));

        let mut lossy = vec![0x30, 0x01, 0x00, 0x9D, 0x01, 0x2A];
        lossy.extend_from_slice(&300u16.to_le_bytes());
        lossy.extend_from_slice(&200u16.to_le_bytes());
        assert_eq!(
            parse_dimensions(&webp(b"VP8 ", &lossy), "image/webp"),
            Some((300, 200))
        );

        let bits = 6u32 | (3u32 << 14);
        let mut lossless = vec![0x2F];
        lossless.extend_from_slice(&bits.to_le_bytes());
        assert_eq!(
            parse_dimensions(&webp(b"VP8L", &lossless), "image/webp"),
            Some((7, 4))
        );

        let extended = [0x10, 0, 0, 0, 9, 0, 0, 5, 0, 0];
        assert_eq!(
            parse_dimensions(&webp(b"VP8X", &extended), "image/webp"),
            Some((10, 6))
        );

        // Foreign or truncated bytes never parse.
        assert_eq!(parse_dimensions(&gif, "image/png"), None);
        assert_eq!(parse_dimensions(&png(1, 1), "image/jpeg"), None);
        assert_eq!(
            parse_dimensions(&[0xFF, 0xD8, 0xFF, 0xD9], "image/jpeg"),
            None
        );
        assert_eq!(parse_dimensions(b"RIFF", "image/webp"), None);
        assert_eq!(parse_dimensions(&png(1, 1)[..20], "image/png"), None);
    }

    #[test]
    fn query_params_are_percent_decoded() {
        let query = "sessionId=s%201&name=a%20b.png&mediaType=image%2Fpng&flag";
        assert_eq!(query_param(query, "sessionId").as_deref(), Some("s 1"));
        assert_eq!(query_param(query, "name").as_deref(), Some("a b.png"));
        assert_eq!(
            query_param(query, "mediaType").as_deref(),
            Some("image/png")
        );
        assert_eq!(query_param(query, "flag").as_deref(), Some(""));
        assert_eq!(query_param(query, "absent"), None);
        assert_eq!(query_param("a=b+c%zz", "a").as_deref(), Some("b c%zz"));
        assert_eq!(
            query_param("n=%E5%9B%BE.png", "n").as_deref(),
            Some("图.png")
        );
    }

    #[test]
    fn names_reduce_to_safe_leaves() {
        assert_eq!(sanitize_name(Some("shot.png"), "bin"), "shot.png");
        assert_eq!(sanitize_name(Some(".."), "png"), "upload.png");
        assert_eq!(sanitize_name(Some("a/b.png"), "png"), "upload.png");
        assert_eq!(sanitize_name(Some("a\\b.png"), "png"), "upload.png");
        assert_eq!(sanitize_name(Some(""), "txt"), "upload.txt");
        assert_eq!(sanitize_name(None, "txt"), "upload.txt");
    }
}
