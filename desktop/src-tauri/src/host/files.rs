//! The `workspaceFiles` remote — the vendored files tree and document
//! preview tabs, served off the session's workspace root.
//!
//! Upstream's host owns the same surface over its own filesystem; here the
//! bridge reads the workspace directly (the root comes from the runtime
//! thread record). `list` speaks workspace paths, `read`/`readBytes`/`stat`
//! name files by their absolute path — the two vocabularies the wire types
//! document — and every target must resolve INSIDE the session's workspace
//! (`workspace-file/outside-workspace` otherwise, the tree's own fence).
//!
//! `readBytes` answers multipart/form-data — one `metadata` field (the
//! `{ok, value}` envelope with `data: null` and an attachments manifest) plus
//! the bytes as a binary part — because the client's strict result codec
//! only accepts a real `Uint8Array`, planted at the manifest's path by the
//! page's carrier (`desktop/ui/src/transport.ts`).
//!
//! `changes` (the watch feed) is a 1s stat poll over the opaque version
//! token (`mtime:size`): a real invalidation feed with upstream's frame
//! vocabulary, at poll granularity.
//!
//! Known limitations: no symlink following on the final component of a read
//! (upstream's own gate; the tree lists a symlink by its destination's
//! type); the office renderer's host conversion RPC stays unimplemented and
//! the office preview renders its unavailable state; `baseFile`-relative
//! resolution is not implemented (the vendored preview addresses files by
//! absolute path).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{rpc_error, rpc_ok, HostState};

/// Largest page one `read` serves (the client asks for line windows).
const MAX_PAGE_LINES: usize = 2_000;
/// Largest directory the tree lists per call.
const MAX_LIST_ENTRIES: usize = 1_000;
/// Largest byte window one `readBytes` serves.
const MAX_WINDOW_BYTES: u64 = 2 * 1024 * 1024;
/// Largest complete file one `readBytes` serves without a window.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// A workspace-files failure: the wire's error code plus its message.
#[derive(Debug)]
struct FileError {
    code: &'static str,
    message: String,
}

impl FileError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The session id every method addresses: the generated client packs each
/// method's args as one object keyed by parameter wire names
/// (`{workspaceFileScopeId, path, range}`), where `workspaceFileScopeId` is
/// the typert lookup parameter's wire name carrying the Session identity.
fn session_arg(args: &Value) -> Option<String> {
    args.get("workspaceFileScopeId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The `path` every method names the file or directory by.
fn path_arg(args: &Value) -> Option<String> {
    args.get("path").and_then(Value::as_str).map(str::to_string)
}

/// The session's workspace root, from the runtime thread record.
async fn workspace_root(host: &HostState, session_id: &str) -> Result<String, FileError> {
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await
        .map_err(|err| FileError::new("workspace-file/not-found", format!("{err:#}")))?;
    let root = detail
        .pointer("/thread/workspace")
        .and_then(Value::as_str)
        .filter(|root| !root.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| host.cwd.to_string_lossy().into_owned());
    Ok(root)
}

/// Resolve one method's target: an absolute path as-is (when it lies inside
/// the root), else the root-relative join. Every target must land inside the
/// workspace — the tree's own fence.
fn resolve_target(root: &Path, path: &str) -> Result<PathBuf, FileError> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    // Lexical containment: the tree never escapes the root through `..`
    // (a symlink that escapes is a runtime fact only a canonicalize would
    // catch; the read path's no-follow gate keeps content reads inside).
    let normalized = normalize(&candidate);
    if !normalized.starts_with(root) {
        return Err(FileError::new(
            "workspace-file/outside-workspace",
            format!("{path} resolves outside the workspace"),
        ));
    }
    Ok(normalized)
}

/// Collapse `.` and `..` lexically (no filesystem access).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The opaque freshness token: mtime nanos plus size.
fn version_of(metadata: &std::fs::Metadata) -> String {
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{mtime}:{}", metadata.len())
}

/// The stat fields every file answer carries.
fn stat_fields(target: &Path, metadata: &std::fs::Metadata) -> Value {
    json!({
        "absolutePath": target.to_string_lossy().replace('\\', "/"),
        "version": version_of(metadata),
        "bytes": metadata.len(),
    })
}

/// `workspaceFileScope` — the lookup the client resolves per session.
pub async fn scope(host: &HostState, args: &Value) -> Value {
    let Some(session_id) = session_arg(args) else {
        return rpc_error(
            "gateway/arguments-invalid",
            "workspaceFileScope without sessionId",
        );
    };
    match workspace_root(host, &session_id).await {
        Ok(root) => rpc_ok(json!({ "sessionId": session_id, "workspaceRoot": root })),
        Err(err) => rpc_error(err.code, err.message),
    }
}

/// `workspaceFiles/list` — one directory's direct children.
pub async fn list(host: &HostState, args: &Value) -> Value {
    match list_inner(host, args).await {
        Ok(value) => rpc_ok(value),
        Err(err) => rpc_error(err.code, err.message),
    }
}

async fn list_inner(host: &HostState, args: &Value) -> Result<Value, FileError> {
    let session_id = session_arg(args)
        .ok_or_else(|| FileError::new("gateway/arguments-invalid", "list without session"))?;
    let path = path_arg(args)
        .ok_or_else(|| FileError::new("gateway/arguments-invalid", "list without path"))?;
    let root = workspace_root(host, &session_id).await?;
    let root = Path::new(&root);
    let target = resolve_target(root, &path)?;
    let metadata = std::fs::metadata(&target)
        .map_err(|_| FileError::new("workspace-file/not-found", format!("{path} not found")))?;
    if !metadata.is_dir() {
        return Err(FileError::new(
            "workspace-file/not-directory",
            format!("{path} is not a directory"),
        ));
    }
    let mut entries: Vec<Value> = Vec::new();
    let mut truncated = false;
    let mut names: Vec<String> = std::fs::read_dir(&target)
        .map_err(|err| FileError::new("workspace-file/not-found", format!("list {path}: {err}")))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        if entries.len() >= MAX_LIST_ENTRIES {
            truncated = true;
            break;
        }
        let child = target.join(&name);
        let child_type = match std::fs::metadata(&child) {
            Ok(metadata) if metadata.is_dir() => "directory",
            Ok(metadata) if metadata.is_file() => "file",
            Ok(_) => "other",
            Err(_) => continue,
        };
        let mut entry = json!({ "name": name, "type": child_type });
        if child_type == "file" {
            if let Ok(metadata) = std::fs::metadata(&child) {
                entry["size"] = json!(metadata.len());
            }
        }
        entries.push(entry);
    }
    // The listed directory's workspace path: empty for the root itself.
    let display = target
        .strip_prefix(root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    Ok(json!({ "path": display, "entries": entries, "truncated": truncated }))
}

/// `workspaceFiles/read` — one page of a text file.
pub async fn read(host: &HostState, args: &Value) -> Value {
    match read_inner(host, args).await {
        Ok(value) => rpc_ok(value),
        Err(err) => rpc_error(err.code, err.message),
    }
}

async fn read_inner(host: &HostState, args: &Value) -> Result<Value, FileError> {
    let session_id = session_arg(args)
        .ok_or_else(|| FileError::new("gateway/arguments-invalid", "read without session"))?;
    let path = path_arg(args)
        .ok_or_else(|| FileError::new("gateway/arguments-invalid", "read without path"))?;
    let (target, metadata) = locate(host, &session_id, &path).await?;
    if !metadata.is_file() {
        return Err(FileError::new(
            "workspace-file/not-found",
            format!("{path} is not a file"),
        ));
    }
    let offset = args
        .pointer("/range/offset")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let limit = args
        .pointer("/range/limit")
        .and_then(Value::as_u64)
        .map(|limit| (limit as usize).min(MAX_PAGE_LINES))
        .unwrap_or(MAX_PAGE_LINES);

    // Blocking file read; the caller runs on the async runtime, so park it.
    let target_cl = target.clone();
    let (page, total_lines, eof) = tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&target_cl).unwrap_or_default();
        let content = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = content.split('\n').collect();
        // A trailing '\n' terminates the last line rather than starting an
        // empty one (the wire contract's line model).
        let total = if content.ends_with('\n') {
            lines.len().saturating_sub(1)
        } else {
            lines.len()
        };
        let start = offset.saturating_sub(1);
        if start >= total {
            return (String::new(), total, true);
        }
        let end = (start + limit).min(total);
        // eof: the page includes the file's last line.
        (lines[start..end].join("\n"), total, end >= total)
    })
    .await
    .map_err(|err| FileError::new("gateway/internal", format!("read join: {err}")))?;

    // How many lines the served page holds (0 when the offset lies past the
    // last line; 1 for a single empty line — the contract's own edge).
    let lines = if offset.saturating_sub(1) >= total_lines {
        0
    } else if page.is_empty() {
        1
    } else {
        page.split('\n').count()
    };
    let mut value = json!({
        "offset": offset,
        "text": page,
        "lines": lines,
        "eof": eof,
    });
    for (key, item) in stat_fields(&target, &metadata).as_object().unwrap() {
        value[key] = item.clone();
    }
    Ok(value)
}

/// `workspaceFiles/stat`.
pub async fn stat(host: &HostState, args: &Value) -> Value {
    let result = async {
        let session_id = session_arg(args)
            .ok_or_else(|| FileError::new("gateway/arguments-invalid", "stat without session"))?;
        let path = path_arg(args)
            .ok_or_else(|| FileError::new("gateway/arguments-invalid", "stat without path"))?;
        let (target, metadata) = locate(host, &session_id, &path).await?;
        Ok::<_, FileError>(stat_fields(&target, &metadata))
    }
    .await;
    match result {
        Ok(value) => rpc_ok(value),
        Err(err) => rpc_error(err.code, err.message),
    }
}

/// `workspaceFiles/readBytes` — the metadata envelope plus the bytes part
/// (the caller wraps them into the multipart response).
pub async fn read_bytes(host: &HostState, args: &Value) -> Result<(Value, Vec<u8>), Value> {
    let inner = async {
        let session_id = session_arg(args).ok_or_else(|| {
            FileError::new("gateway/arguments-invalid", "readBytes without session")
        })?;
        let path = path_arg(args)
            .ok_or_else(|| FileError::new("gateway/arguments-invalid", "readBytes without path"))?;
        let (target, metadata) = locate(host, &session_id, &path).await?;
        if !metadata.is_file() {
            return Err(FileError::new(
                "workspace-file/not-found",
                format!("{path} is not a file"),
            ));
        }
        let window = args.pointer("/options/range");
        let (offset, length) = match window {
            Some(range) => (
                range.get("offset").and_then(Value::as_u64).unwrap_or(0),
                range
                    .get("length")
                    .and_then(Value::as_u64)
                    .unwrap_or(MAX_WINDOW_BYTES)
                    .min(MAX_WINDOW_BYTES),
            ),
            None => {
                if metadata.len() > MAX_FILE_BYTES {
                    return Err(FileError::new(
                        "workspace-file/oversized",
                        format!("{path} is {} bytes over the {} cap", metadata.len(), MAX_FILE_BYTES),
                    ));
                }
                (0, metadata.len())
            }
        };
        let target_clone = target.clone();
        let data = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::fs::File::open(&target_clone)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut buffer = vec![0u8; length as usize];
            let mut read = 0usize;
            while read < buffer.len() {
                match file.read(&mut buffer[read..])? {
                    0 => break,
                    n => read += n,
                }
            }
            buffer.truncate(read);
            Ok::<_, std::io::Error>(buffer)
        })
        .await
        .map_err(|err| FileError::new("gateway/internal", format!("readBytes join: {err}")))?
        .map_err(|err| FileError::new("workspace-file/not-found", format!("read {path}: {err}")))?;
        let eof = offset + data.len() as u64 >= metadata.len();
        let mut value = json!({
            "offset": offset,
            "data": Value::Null,
            "eof": eof,
        });
        for (key, item) in stat_fields(&target, &metadata).as_object().unwrap() {
            value[key] = item.clone();
        }
        Ok::<_, FileError>((value, data))
    }
    .await;
    inner.map_err(|err| rpc_error(err.code, err.message))
}

/// Resolve and stat one file target (shared by read/stat/readBytes).
async fn locate(
    host: &HostState,
    session_id: &str,
    path: &str,
) -> Result<(PathBuf, std::fs::Metadata), FileError> {
    let root = workspace_root(host, session_id).await?;
    let target = resolve_target(Path::new(&root), path)?;
    let stat_target = target.clone();
    let metadata = tokio::task::spawn_blocking(move || std::fs::symlink_metadata(&stat_target))
        .await
        .map_err(|err| FileError::new("gateway/internal", format!("stat join: {err}")))?
        .map_err(|_| FileError::new("workspace-file/not-found", format!("{path} not found")))?;
    // The read gate: no following the final component's symlink.
    if metadata.file_type().is_symlink() {
        return Err(FileError::new(
            "workspace-file/not-found",
            format!("{path} is a symlink"),
        ));
    }
    Ok((target, metadata))
}

/// `workspaceFiles/changes` — the watch feed: `ready`, then a 1s stat poll
/// pushing `change` frames when the version token moves or the target goes.
pub async fn changes(
    host: &HostState,
    args: &Value,
    tx: &tokio::sync::mpsc::Sender<Value>,
) -> Result<(), Value> {
    let session_id = session_arg(args).ok_or_else(|| {
        super::stream_failure("gateway/arguments-invalid", "changes without session")
    })?;
    let path = path_arg(args)
        .ok_or_else(|| super::stream_failure("gateway/arguments-invalid", "changes without path"))?;
    let root = workspace_root(host, &session_id)
        .await
        .map_err(|err| super::stream_failure(err.code, err.message))?;
    let target = resolve_target(Path::new(&root), &path)
        .map_err(|err| super::stream_failure(err.code, err.message))?;
    if tx.send(json!({ "kind": "ready" })).await.is_err() {
        return Ok(());
    }
    let mut last = stat_version(&target);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            _ = tx.closed() => return Ok(()),
        }
        let current = stat_version(&target);
        if current != last {
            let change = match &current {
                Some(version) => json!({
                    "kind": "change",
                    "change": {
                        "absolutePath": target.to_string_lossy().replace('\\', "/"),
                        "version": version,
                    },
                }),
                None => json!({
                    "kind": "change",
                    "change": {
                        "absolutePath": target.to_string_lossy().replace('\\', "/"),
                        "absent": true,
                    },
                }),
            };
            if tx.send(change).await.is_err() {
                return Ok(());
            }
            last = current;
        }
    }
}

/// The target's current version token, or `None` when it is gone.
fn stat_version(target: &Path) -> Option<String> {
    std::fs::symlink_metadata(target).ok().map(|metadata| version_of(&metadata))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_read_the_generated_clients_object_keyed_shape() {
        // The gateway packs each Remote call's args as one object keyed by
        // parameter wire names (`typert.remote-client.js`): the scope lookup
        // rides `workspaceFileScopeId`, the target `path`, and the page/window
        // under `range` / `options.range` — never a positional array.
        let args = json!({
            "workspaceFileScopeId": "thr_1",
            "path": "src/a.rs",
            "range": { "offset": 2, "limit": 10 },
        });
        assert_eq!(session_arg(&args).as_deref(), Some("thr_1"));
        assert_eq!(path_arg(&args).as_deref(), Some("src/a.rs"));
        assert_eq!(args.pointer("/range/offset").and_then(Value::as_u64), Some(2));
        assert_eq!(args.pointer("/range/limit").and_then(Value::as_u64), Some(10));

        let args = json!({
            "workspaceFileScopeId": "thr_1",
            "path": "bin/blob.bin",
            "options": { "range": { "offset": 4, "length": 16 } },
        });
        let window = args.pointer("/options/range").unwrap();
        assert_eq!(window.get("offset").and_then(Value::as_u64), Some(4));
        assert_eq!(window.get("length").and_then(Value::as_u64), Some(16));

        // The shapes the old reader accepted (bare session id, positional
        // array) are not what the wire sends; they must not parse.
        assert!(session_arg(&json!("thr_1")).is_none());
        assert!(session_arg(&json!(["thr_1", "src/a.rs"])).is_none());
        assert!(path_arg(&json!(["thr_1", "src/a.rs"])).is_none());
    }

    #[test]
    fn targets_resolve_inside_the_workspace_only() {
        let root = Path::new("/w/proj");
        assert_eq!(
            resolve_target(root, "src/a.rs").unwrap(),
            PathBuf::from("/w/proj/src/a.rs")
        );
        assert_eq!(
            resolve_target(root, "/w/proj/b.rs").unwrap(),
            PathBuf::from("/w/proj/b.rs")
        );
        // Lexical escapes stay inside (collapsed), true escapes refuse.
        assert_eq!(
            resolve_target(root, "src/../../proj/c.rs").unwrap(),
            PathBuf::from("/w/proj/c.rs")
        );
        assert!(resolve_target(root, "../outside.rs").is_err());
        assert!(resolve_target(root, "/etc/passwd").is_err());
    }

    #[test]
    fn a_trailing_newline_terminates_rather_than_starts_a_line() {
        // The wire's line model, on the read path's own arithmetic.
        let content = "one\ntwo\nthree\n";
        let lines: Vec<&str> = content.split('\n').collect();
        let total = if content.ends_with('\n') {
            lines.len() - 1
        } else {
            lines.len()
        };
        assert_eq!(total, 3, "a trailing newline starts no fourth line");
        assert_eq!(lines[0..2].join("\n"), "one\ntwo");
        // An offset past the last line serves an empty, EOF page.
        let start = 5usize.saturating_sub(1);
        assert!(start >= total);
    }
}
