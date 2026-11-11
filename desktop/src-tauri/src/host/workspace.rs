//! The desktop-owned Workspace registry, pin set, and manual session order.
//!
//! The runtime owns threads; the desktop owns how the sidebar groups them.
//! Workspaces are desktop registrations over host directories (upstream's
//! Workspace browser contract): `workspace/create` admits one existing
//! directory, `rename`/`delete` maintain the registration, and `insertBefore`
//! orders the registry. Sessions are grouped by the runtime thread's own
//! `workspace` path, so a session left in another directory still lands in
//! its directory's row; a thread whose directory has no registration is
//! projected as a recovered workspace (no write) and becomes durable the
//! first time a mutation names it.
//!
//! A pinned session leads its section, most recently pinned first (the
//! upstream registry contract); the manual order is the workspace view's
//! `sessionIds` order — sessions that were never dragged keep following the
//! runtime's recency order after the manually ordered ones, which is exactly
//! the merge the client's own reconcile applies (vendored `ui-workspace`
//! `tree.ts`).
//!
//! State persists at `~/.codesmith/desktop/workspace.json` (pins/order) and
//! `~/.codesmith/desktop/workspaces.json` (registry), loaded per operation.
//! A missing file is a fresh install (defaults); a corrupt file fails that
//! one operation loud instead of silently resetting the user's state.
//!
//! Wire contracts (vendored `api-workspace-controller`):
//! - `pinSession {sessionId}` / `unpinSession {sessionId}` →
//!   `{pinnedSessionIds}` (the complete set, MRU-first; unpin is an
//!   idempotent no-op for an unpinned id); an unknown session →
//!   `session/not-found`, an archived one → `gateway/bad-request`; every
//!   mutation is echoed to `workspace/follow` as
//!   `{type:'pinned', pinnedSessionIds}` so other connected clients reorder.
//! - `create {path}` → `{workspace, created}`; `rename {workspaceId, title}`
//!   → `{workspace}`; `delete {workspaceId}` → `{deleted:true}`;
//!   `insertBefore {workspaceId, beforeWorkspaceId?}` → `{workspaceIds}`.
//!   The directory must be absolute, existing, and a directory. Mutations
//!   echo the complete `workspace/follow` baseline as a `replace` increment.
//! - `insertSessionBefore {workspaceId, sessionId, beforeSessionId?}` → the
//!   updated workspace view; an unknown workspace → `workspace/not-found`,
//!   an unknown session or anchor → `workspace/move-invalid`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::HostState;

/// The persisted sidebar state: `{ "pinned": […], "order": […] }`.
#[derive(Clone)]
pub struct SidebarState {
    pinned: Vec<String>,
    order: Vec<String>,
    path: PathBuf,
}

/// One operation's client-visible failure: the RPC error code with its
/// message (`gateway/internal` for bridge-side trouble).
pub struct OpError {
    pub code: &'static str,
    pub message: String,
}

impl OpError {
    fn code(code: &'static str, message: impl Into<String>) -> Self {
        OpError {
            code,
            message: message.into(),
        }
    }
}

fn internal(err: anyhow::Error) -> OpError {
    OpError::code("gateway/internal", format!("{err:#}"))
}

impl SidebarState {
    /// Move one session to the front of the pin set (MRU-first contract).
    fn pin(&mut self, session_id: &str) {
        self.pinned.retain(|id| id != session_id);
        self.pinned.insert(0, session_id.to_string());
    }

    /// Drop one session from the pin set; an unpinned id is a no-op.
    fn unpin(&mut self, session_id: &str) {
        self.pinned.retain(|id| id != session_id);
    }

    /// The baseline's pin set: persisted pins whose thread still exists.
    fn live_pins(&self, live: &[String]) -> Vec<String> {
        self.pinned
            .iter()
            .filter(|id| live.contains(id))
            .cloned()
            .collect()
    }

    /// Move one live session before an anchor (or to the end without one).
    /// The splice runs over the merged order, so a drop onto a never-dragged
    /// session lands exactly there and the merged list becomes the manual
    /// order — after any drag the visible order is the saved order.
    fn insert_before(
        &mut self,
        live: &[String],
        session_id: &str,
        before: Option<&str>,
    ) -> Result<()> {
        if !live.iter().any(|id| id == session_id) {
            bail!("session {session_id} is not in the workspace");
        }
        let before = match before {
            Some(before) => {
                if !live.iter().any(|id| id == before) {
                    bail!("anchor session {before} is not in the workspace");
                }
                Some(before)
            }
            None => None,
        };
        if before == Some(session_id) {
            return Ok(()); // a drop onto itself leaves the order alone
        }
        let mut effective = ordered_session_ids(&self.order, live);
        effective.retain(|id| id != session_id);
        match before {
            Some(before) => {
                let at = effective
                    .iter()
                    .position(|id| id == before)
                    .with_context(|| format!("anchor session {before} vanished"))?;
                effective.insert(at, session_id.to_string());
            }
            None => effective.push(session_id.to_string()),
        }
        self.order = effective;
        Ok(())
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let body = json!({ "pinned": self.pinned, "order": self.order });
        std::fs::write(&self.path, serde_json::to_string_pretty(&body)?)
            .with_context(|| format!("write {}", self.path.display()))?;
        Ok(())
    }
}

/// The workspace view's session ids: the manual order first (ids still
/// live, in saved order), then never-dragged live sessions in runtime
/// recency order — the merge the client's own reconcile expects.
pub fn ordered_session_ids(order: &[String], live: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = order
        .iter()
        .filter(|id| live.contains(*id))
        .cloned()
        .collect();
    for id in live {
        if !order.contains(id) {
            ids.push(id.clone());
        }
    }
    ids
}

fn state_path(home: &Path) -> PathBuf {
    home.join("desktop").join("workspace.json")
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Load the sidebar state; a missing file is the fresh-install default.
fn load(home: &Path) -> Result<SidebarState> {
    let path = state_path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let parsed: Value =
                serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
            Ok(SidebarState {
                pinned: strings(&parsed, "pinned"),
                order: strings(&parsed, "order"),
                path,
            })
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(SidebarState {
            pinned: Vec::new(),
            order: Vec::new(),
            path,
        }),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

/// The view's merged session ids for callers that already hold the live
/// list. A corrupt state file logs loud and degrades to the runtime's
/// recency order rather than failing an unrelated flow (session creation).
pub fn ordered_live(home: &Path, live: &[String]) -> Vec<String> {
    match load(home) {
        Ok(state) => ordered_session_ids(&state.order, live),
        Err(err) => {
            eprintln!("codesmith desktop: sidebar order unreadable: {err:#}");
            live.to_vec()
        }
    }
}

/// The follow baseline's pin set: persisted pins that are still live.
pub fn pinned_live(home: &Path, live: &[String]) -> Vec<String> {
    match load(home) {
        Ok(state) => state.live_pins(live),
        Err(err) => {
            eprintln!("codesmith desktop: sidebar pins unreadable: {err:#}");
            Vec::new()
        }
    }
}

/// [`ordered_live`] parked off the async workers — the state file read is
/// blocking I/O and the view builders run on the runtime.
pub async fn ordered_live_parked(home: &Path, live: Vec<String>) -> Vec<String> {
    let home = home.to_path_buf();
    let fallback = live.clone();
    tokio::task::spawn_blocking(move || ordered_live(&home, &live))
        .await
        .unwrap_or_else(|err| {
            eprintln!("codesmith desktop: sidebar order read join failed: {err}");
            fallback
        })
}

/// [`pinned_live`] parked off the async workers.
pub async fn pinned_live_parked(home: &Path, live: Vec<String>) -> Vec<String> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || pinned_live(&home, &live))
        .await
        .unwrap_or_else(|err| {
            eprintln!("codesmith desktop: sidebar pins read join failed: {err}");
            Vec::new()
        })
}

/// [`load`] parked off the async workers (the mutation handlers run on the
/// runtime).
pub async fn load_parked(home: &Path) -> Result<SidebarState> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || load(&home))
        .await
        .map_err(|err| anyhow!("sidebar state read join failed: {err}"))?
}

/// [`SidebarState::save`] parked off the async workers.
pub async fn save_parked(state: &SidebarState) -> Result<()> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || state.save())
        .await
        .map_err(|err| anyhow!("sidebar state write join failed: {err}"))?
}

// === Workspace registry ===

/// One durable Workspace registration. `path` is canonical for entries added
/// through `workspace/create`; the implicit default keeps the bridge's own
/// working directory so sessions created before the registry existed still
/// group under it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct WorkspaceEntry {
    #[serde(rename = "workspaceId")]
    workspace_id: String,
    path: PathBuf,
    title: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
}

fn registry_path(home: &Path) -> PathBuf {
    home.join("desktop").join("workspaces.json")
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Lexically collapse `.` and `..` (no filesystem access) so the same
/// directory spelled two ways compares equal.
fn normalize_path(path: &Path) -> PathBuf {
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

fn paths_match(left: &Path, right: &Path) -> bool {
    normalize_path(left) == normalize_path(right)
}

/// One stable id per directory path: the bridge's default cwd keeps the
/// historical id (so a client's persisted selection survives), everything
/// else is a short FNV-1a digest of the canonical path.
fn workspace_id_for(path: &Path, default_path: &Path) -> String {
    if paths_match(path, default_path) {
        super::DEFAULT_WORKSPACE_ID.to_string()
    } else {
        format!(
            "ws_{:016x}",
            fnv1a(normalize_path(path).to_string_lossy().as_bytes())
        )
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn workspace_title(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn default_entry(cwd: &Path) -> WorkspaceEntry {
    let now = now_iso();
    WorkspaceEntry {
        workspace_id: super::DEFAULT_WORKSPACE_ID.to_string(),
        path: cwd.to_path_buf(),
        title: workspace_title(cwd),
        created_at: now.clone(),
        updated_at: now,
    }
}

/// Load the registry; a missing file is `None` (fresh install), an existing
/// empty list is `Some(vec![])` (the user removed every workspace).
fn load_registry(home: &Path) -> Result<Option<Vec<WorkspaceEntry>>> {
    let path = registry_path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let parsed: Value =
                serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
            let entries = parsed
                .get("workspaces")
                .and_then(Value::as_array)
                .with_context(|| format!("{} has no workspaces array", path.display()))?;
            let entries = entries
                .iter()
                .cloned()
                .map(serde_json::from_value)
                .collect::<Result<Vec<WorkspaceEntry>, _>>()
                .with_context(|| format!("parse {} entries", path.display()))?;
            Ok(Some(entries))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

/// Write the registry atomically: a rename either publishes the complete new
/// list or leaves the previous one in place.
fn save_registry(home: &Path, entries: &[WorkspaceEntry]) -> Result<()> {
    let path = registry_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let body = json!({ "workspaces": entries });
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&body)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))
}

async fn load_registry_parked(home: &Path) -> Result<Option<Vec<WorkspaceEntry>>> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || load_registry(&home))
        .await
        .map_err(|err| anyhow!("workspace registry read join failed: {err}"))?
}

async fn save_registry_parked(home: &Path, entries: Vec<WorkspaceEntry>) -> Result<()> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || save_registry(&home, &entries))
        .await
        .map_err(|err| anyhow!("workspace registry write join failed: {err}"))?
}

/// One runtime thread row: the workspace grouping key plus the lists the
/// follow baseline publishes.
#[derive(Debug, Deserialize)]
struct ThreadRow {
    id: String,
    workspace: PathBuf,
    #[serde(default)]
    archived: bool,
}

/// The complete `workspace/follow` baseline. `items` are view rows;
/// `archived` is the archived session set; `pinned` is the live pin set.
pub struct WorkspaceBaseline {
    pub items: Vec<Value>,
    pub archived: Vec<String>,
    pub pinned: Vec<String>,
}

/// The wire baseline value (the stream's `baseline` and `replace` payload).
pub fn baseline_value(baseline: &WorkspaceBaseline) -> Value {
    json!({
        "items": baseline.items,
        "archivedSessionIds": baseline.archived,
        "pinnedSessionIds": baseline.pinned,
    })
}

fn view_of(entry: &WorkspaceEntry, session_ids: &[String]) -> Value {
    json!({
        "workspaceId": entry.workspace_id,
        "path": entry.path.to_string_lossy(),
        "title": entry.title,
        "sessionIds": session_ids,
        "createdAt": entry.created_at,
        "updatedAt": entry.updated_at,
    })
}

async fn thread_rows(host: &HostState) -> Vec<ThreadRow> {
    match host
        .backend
        .get::<Vec<ThreadRow>>("/v1/threads?limit=200&include_archived=true")
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            // A follow stream without thread rows still renders the
            // registered workspaces; sessions fill in on the next read.
            eprintln!("codesmith desktop: workspace/follow thread rows: {err:#}");
            Vec::new()
        }
    }
}

/// Build the registry rows plus their session membership. Sessions group by
/// the runtime thread's own workspace path; a session in an unregistered
/// directory trails under the client's Ungrouped bucket.
pub async fn baseline(host: &HostState) -> WorkspaceBaseline {
    let rows = thread_rows(host).await;
    let entries = match load_registry_parked(&host.settings_home()).await {
        Ok(Some(entries)) => entries,
        Ok(None) => vec![default_entry(&host.cwd)],
        Err(err) => {
            // A corrupt registry logs loud; the sidebar degrades to the
            // default workspace instead of failing every follow stream.
            eprintln!("codesmith desktop: workspace registry unreadable: {err:#}");
            vec![default_entry(&host.cwd)]
        }
    };
    let live: Vec<String> = rows
        .iter()
        .filter(|row| !row.archived)
        .map(|row| row.id.clone())
        .collect();
    let archived: Vec<String> = rows
        .iter()
        .filter(|row| row.archived)
        .map(|row| row.id.clone())
        .collect();
    let home = host.settings_home();
    let merged = ordered_live_parked(&home, live.clone()).await;
    let workspaces: std::collections::HashMap<&str, &Path> = rows
        .iter()
        .map(|row| (row.id.as_str(), row.workspace.as_path()))
        .collect();
    let membership = |entry: &WorkspaceEntry| -> Vec<String> {
        merged
            .iter()
            .filter(|id| {
                workspaces
                    .get(id.as_str())
                    .is_some_and(|path| paths_match(path, &entry.path))
            })
            .cloned()
            .collect()
    };
    // The default entry is implicitly registered when the file is missing;
    // once a registry exists its rows stand alone (delete stays deleted).
    let items: Vec<Value> = entries
        .iter()
        .map(|entry| view_of(entry, &membership(entry)))
        .collect();
    let pinned = pinned_live_parked(&home, live).await;
    WorkspaceBaseline {
        items,
        archived,
        pinned,
    }
}

/// Push the complete baseline as a `replace` increment to every open
/// `workspace/follow` stream.
pub async fn push_replace(host: &HostState) {
    let baseline = baseline(host).await;
    host.push_live(
        "workspace/follow",
        json!({ "type": "replace", "value": baseline_value(&baseline) }),
    )
    .await;
}

/// One workspace view from the live baseline, by id.
pub async fn view_for(host: &HostState, workspace_id: &str) -> Option<Value> {
    baseline(host)
        .await
        .items
        .into_iter()
        .find(|item| item["workspaceId"].as_str() == Some(workspace_id))
}

/// Resolve one workspace id to the directory a new session should own.
pub async fn path_for_id(host: &HostState, workspace_id: &str) -> Option<PathBuf> {
    let home = host.settings_home();
    let entries = match load_registry_parked(&home).await {
        Ok(Some(entries)) => entries,
        Ok(None) => vec![default_entry(&host.cwd)],
        Err(err) => {
            eprintln!("codesmith desktop: workspace registry unreadable: {err:#}");
            vec![default_entry(&host.cwd)]
        }
    };
    entries
        .into_iter()
        .find(|entry| entry.workspace_id == workspace_id)
        .map(|entry| entry.path)
}

/// Load the registry for one mutation, seeding the implicit default when the
/// file is missing.
async fn registry_for_mutation(host: &HostState) -> Result<Vec<WorkspaceEntry>> {
    Ok(match load_registry_parked(&host.settings_home()).await? {
        Some(entries) => entries,
        None => vec![default_entry(&host.cwd)],
    })
}

fn workspace_entries_error(code: &'static str, message: impl Into<String>) -> OpError {
    OpError::code(code, message)
}

/// `workspace/create` — admit one existing directory. An already registered
/// directory (path-normalized) returns its row with `created:false`.
pub async fn create(host: &HostState, args: &Value) -> Result<Value, OpError> {
    let args = args.get("request").unwrap_or(args);
    let requested = args
        .get("path")
        .and_then(Value::as_str)
        .context("workspace/create without path")
        .map_err(internal)?
        .to_string();
    let canonical = tokio::task::spawn_blocking(move || {
        let path = PathBuf::from(&requested);
        if !path.is_absolute() {
            bail!("{} is not an absolute path", path.display());
        }
        let canonical = std::fs::canonicalize(&path).context("canonicalize workspace")?;
        if !canonical.is_dir() {
            bail!("{} is not a directory", canonical.display());
        }
        Ok::<PathBuf, anyhow::Error>(canonical)
    })
    .await
    .map_err(|err| internal(anyhow!("workspace create join failed: {err}")))?
    .map_err(|err| workspace_entries_error("workspace/invalid-path", format!("{err:#}")))?;

    let _guard = host.workspace_lock.lock().await;
    let mut entries = registry_for_mutation(host).await.map_err(internal)?;
    if let Some(existing) = entries
        .iter()
        .find(|entry| paths_match(&entry.path, &canonical))
    {
        let id = existing.workspace_id.clone();
        drop(_guard);
        let view = view_for(host, &id)
            .await
            .unwrap_or_else(|| view_of(existing, &[]));
        return Ok(json!({ "workspace": view, "created": false }));
    }
    let now = now_iso();
    let title = workspace_title(&canonical);
    let entry = WorkspaceEntry {
        workspace_id: workspace_id_for(&canonical, &host.cwd),
        path: canonical,
        title,
        created_at: now.clone(),
        updated_at: now,
    };
    entries.push(entry.clone());
    save_registry_parked(&host.settings_home(), entries)
        .await
        .map_err(internal)?;
    drop(_guard);
    push_replace(host).await;
    let view = view_for(host, &entry.workspace_id)
        .await
        .unwrap_or_else(|| view_of(&entry, &[]));
    Ok(json!({ "workspace": view, "created": true }))
}

/// `workspace/rename` — set one registration's title.
pub async fn rename(host: &HostState, args: &Value) -> Result<Value, OpError> {
    let args = args.get("request").unwrap_or(args);
    let workspace_id = args
        .get("workspaceId")
        .and_then(Value::as_str)
        .context("workspace/rename without workspaceId")
        .map_err(internal)?
        .to_string();
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .context("workspace/rename without title")
        .map_err(internal)?
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(workspace_entries_error(
            "gateway/bad-request",
            "a workspace title cannot be empty",
        ));
    }
    let _guard = host.workspace_lock.lock().await;
    let mut entries = registry_for_mutation(host).await.map_err(internal)?;
    let Some(index) = entries
        .iter()
        .position(|entry| entry.workspace_id == workspace_id)
    else {
        return Err(workspace_entries_error(
            "workspace/not-found",
            format!("workspace {workspace_id} not found"),
        ));
    };
    if entries
        .iter()
        .any(|other| other.workspace_id != workspace_id && other.title == title)
    {
        return Err(workspace_entries_error(
            "workspace/name-conflict",
            format!("workspace name {title:?} is already in use"),
        ));
    }
    entries[index].title = title;
    entries[index].updated_at = now_iso();
    let updated = entries[index].clone();
    save_registry_parked(&host.settings_home(), entries)
        .await
        .map_err(internal)?;
    drop(_guard);
    push_replace(host).await;
    let view = view_for(host, &workspace_id)
        .await
        .unwrap_or_else(|| view_of(&updated, &[]));
    Ok(json!({ "workspace": view }))
}

/// `workspace/delete` — remove one registration. Its sessions keep their
/// directory and trail under the client's Ungrouped bucket.
pub async fn delete(host: &HostState, args: &Value) -> Result<Value, OpError> {
    let args = args.get("request").unwrap_or(args);
    let workspace_id = args
        .get("workspaceId")
        .and_then(Value::as_str)
        .context("workspace/delete without workspaceId")
        .map_err(internal)?;
    let _guard = host.workspace_lock.lock().await;
    let mut entries = registry_for_mutation(host).await.map_err(internal)?;
    let before = entries.len();
    entries.retain(|entry| entry.workspace_id != workspace_id);
    if entries.len() == before {
        return Err(workspace_entries_error(
            "workspace/not-found",
            format!("workspace {workspace_id} not found"),
        ));
    }
    save_registry_parked(&host.settings_home(), entries)
        .await
        .map_err(internal)?;
    drop(_guard);
    push_replace(host).await;
    Ok(json!({ "deleted": true }))
}

/// `workspace/insertBefore` — reorder the registry.
pub async fn insert_workspace_before(host: &HostState, args: &Value) -> Result<Value, OpError> {
    let args = args.get("request").unwrap_or(args);
    let workspace_id = args
        .get("workspaceId")
        .and_then(Value::as_str)
        .context("workspace/insertBefore without workspaceId")
        .map_err(internal)?
        .to_string();
    let before = args
        .get("beforeWorkspaceId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let _guard = host.workspace_lock.lock().await;
    let mut entries = registry_for_mutation(host).await.map_err(internal)?;
    if !entries
        .iter()
        .any(|entry| entry.workspace_id == workspace_id)
    {
        return Err(workspace_entries_error(
            "workspace/not-found",
            format!("workspace {workspace_id} not found"),
        ));
    }
    let mut ids: Vec<String> = entries
        .iter()
        .map(|entry| entry.workspace_id.clone())
        .collect();
    ids.retain(|id| id != &workspace_id);
    match before {
        Some(anchor) => {
            let Some(at) = ids.iter().position(|id| id == &anchor) else {
                return Err(workspace_entries_error(
                    "workspace/move-invalid",
                    format!("anchor workspace {anchor} not found"),
                ));
            };
            ids.insert(at, workspace_id);
        }
        None => ids.push(workspace_id),
    }
    let mut ordered: Vec<WorkspaceEntry> = Vec::with_capacity(entries.len());
    for id in &ids {
        if let Some(index) = entries.iter().position(|entry| &entry.workspace_id == id) {
            ordered.push(entries.remove(index));
        }
    }
    save_registry_parked(&host.settings_home(), ordered)
        .await
        .map_err(internal)?;
    drop(_guard);
    push_replace(host).await;
    Ok(json!({ "workspaceIds": ids }))
}

/// `workspace/initializeDefault` — establish the bridge cwd workspace when
/// none exists, and answer the first row otherwise.
pub async fn initialize_default(host: &HostState) -> Result<Value, OpError> {
    let _guard = host.workspace_lock.lock().await;
    let mut entries = registry_for_mutation(host).await.map_err(internal)?;
    if entries.is_empty() {
        entries.push(default_entry(&host.cwd));
        save_registry_parked(&host.settings_home(), entries.clone())
            .await
            .map_err(internal)?;
    }
    let id = entries[0].workspace_id.clone();
    drop(_guard);
    push_replace(host).await;
    let view = view_for(host, &id)
        .await
        .ok_or_else(|| workspace_entries_error("workspace/not-found", "workspace not found"))?;
    // WorkspaceValue | undefined: the client reads `value.workspace`.
    Ok(json!({ "workspace": view }))
}

/// The live (non-archived) and archived thread id lists from the runtime.
async fn thread_sets(host: &HostState) -> Result<(Vec<String>, Vec<String>)> {
    #[derive(serde::Deserialize)]
    struct ThreadId {
        id: String,
    }
    let live: Vec<String> = host
        .backend
        .get::<Vec<ThreadId>>("/v1/threads/summary?limit=200")
        .await?
        .into_iter()
        .map(|t| t.id)
        .collect();
    let archived: Vec<String> = host
        .backend
        .get::<Vec<ThreadId>>("/v1/threads/summary?limit=200&archived_only=true")
        .await?
        .into_iter()
        .map(|t| t.id)
        .collect();
    Ok((live, archived))
}

/// `workspace/pinSession` / `workspace/unpinSession`: validate against the
/// runtime's live/archived sets, mutate the persisted pin set, echo the
/// `pinned` increment to every open workspace follow.
pub async fn pin_or_unpin(host: &HostState, pin: bool, args: &Value) -> Result<Value, OpError> {
    // The workspace remote's single `request` parameter: the gateway packs
    // its args as `{request: {…}}` (the shape every bridge reader unwraps).
    let args = args.get("request").unwrap_or(args);
    let session_id = args
        .get("sessionId")
        .and_then(Value::as_str)
        .context("workspace pin without sessionId")
        .map_err(internal)?
        .to_string();
    let (live, archived) = thread_sets(host).await.map_err(internal)?;
    let mut state = load_parked(&host.settings_home()).await.map_err(internal)?;
    if pin && !live.contains(&session_id) {
        return Err(if archived.contains(&session_id) {
            OpError::code(
                "gateway/bad-request",
                "an archived session cannot be pinned",
            )
        } else {
            OpError::code(
                "session/not-found",
                format!("session {session_id} not found"),
            )
        });
    }
    if pin {
        state.pin(&session_id);
    } else {
        // Unpin is idempotent by contract — an unpinned id is not an error.
        state.unpin(&session_id);
    }
    save_parked(&state).await.map_err(internal)?;
    let pins = state.live_pins(&live);
    host.push_live(
        "workspace/follow",
        json!({ "type": "pinned", "pinnedSessionIds": pins }),
    )
    .await;
    Ok(json!({ "pinnedSessionIds": pins }))
}

/// `workspace/insertSessionBefore`: splice the manual order and answer the
/// updated workspace view. Both the session and an anchor must belong to the
/// named workspace; the saved order stays registry-global (the view filters
/// it per workspace), so a cross-workspace splice keeps every other row.
pub async fn insert_session_before(host: &HostState, args: &Value) -> Result<Value, OpError> {
    let args = args.get("request").unwrap_or(args);
    let workspace_id = args
        .get("workspaceId")
        .and_then(Value::as_str)
        .context("workspace move without workspaceId")
        .map_err(internal)?
        .to_string();
    let session_id = args
        .get("sessionId")
        .and_then(Value::as_str)
        .context("workspace move without sessionId")
        .map_err(internal)?
        .to_string();
    let before = args
        .get("beforeSessionId")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let baseline = baseline(host).await;
    let members: Vec<String> = baseline
        .items
        .iter()
        .find(|item| item["workspaceId"].as_str() == Some(workspace_id.as_str()))
        .ok_or_else(|| {
            workspace_entries_error(
                "workspace/not-found",
                format!("workspace {workspace_id} not found"),
            )
        })
        .and_then(|view| {
            view["sessionIds"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_string))
                        .collect()
                })
                .ok_or_else(|| {
                    workspace_entries_error(
                        "workspace/move-invalid",
                        format!("workspace {workspace_id} has no session list"),
                    )
                })
        })?;
    if !members.contains(&session_id) {
        return Err(workspace_entries_error(
            "workspace/move-invalid",
            format!("session {session_id} is not in workspace {workspace_id}"),
        ));
    }
    if let Some(before) = before.as_ref() {
        if !members.contains(before) {
            return Err(workspace_entries_error(
                "workspace/move-invalid",
                format!("anchor session {before} is not in workspace {workspace_id}"),
            ));
        }
    }
    let (live, _archived) = thread_sets(host).await.map_err(internal)?;
    let mut state = load_parked(&host.settings_home()).await.map_err(internal)?;
    state
        .insert_before(&live, &session_id, before.as_deref())
        .map_err(|err| OpError::code("workspace/move-invalid", format!("{err:#}")))?;
    save_parked(&state).await.map_err(internal)?;
    push_replace(host).await;
    let view = view_for(host, &workspace_id).await.ok_or_else(|| {
        workspace_entries_error(
            "workspace/not-found",
            format!("workspace {workspace_id} not found"),
        )
    })?;
    // WorkspaceValue: the client merges `value.workspace` into its rows (a
    // bare view here would upsert `undefined`).
    Ok(json!({ "workspace": view }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cds-workspace-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn state_with(tag: &str, pinned: &[&str], order: &[&str]) -> SidebarState {
        SidebarState {
            pinned: pinned.iter().map(|s| s.to_string()).collect(),
            order: order.iter().map(|s| s.to_string()).collect(),
            path: state_path(&temp_home(tag)),
        }
    }

    #[test]
    fn pin_moves_to_front_and_survives_a_reload() {
        let home = temp_home("pin");
        let mut state = load(&home).unwrap();
        state.pin("a");
        state.pin("b");
        state.pin("a"); // re-pinning refreshes MRU position, it never duplicates
        assert_eq!(state.pinned, vec!["a".to_string(), "b".to_string()]);
        state.save().unwrap();
        assert_eq!(load(&home).unwrap().pinned, vec!["a", "b"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unpin_is_an_idempotent_no_op_for_unpinned_ids() {
        let mut state = state_with("unpin", &["a", "b"], &[]);
        state.unpin("missing");
        assert_eq!(state.pinned, vec!["a", "b"]);
        state.unpin("a");
        state.unpin("a");
        assert_eq!(state.pinned, vec!["b"]);
    }

    #[test]
    fn ordered_session_ids_keeps_manual_order_then_recency() {
        let order = ["x".to_string(), "a".to_string()];
        let live = [
            "b".to_string(),
            "a".to_string(),
            "x".to_string(),
            "c".to_string(),
        ];
        // Manual survivors in saved order; never-dragged sessions follow in
        // runtime order; a dead manual id (x here is live, try one that is
        // not) drops out.
        assert_eq!(ordered_session_ids(&order, &live), vec!["x", "a", "b", "c"]);
        let dead = ["gone".to_string(), "a".to_string()];
        assert_eq!(ordered_session_ids(&dead, &live), vec!["a", "b", "x", "c"]);
    }

    #[test]
    fn live_pins_drop_dead_ids() {
        let state = state_with("pins-live", &["a", "dead"], &[]);
        let live = ["a".to_string(), "b".to_string()];
        assert_eq!(state.live_pins(&live), vec!["a".to_string()]);
    }

    #[test]
    fn insert_before_splices_over_the_merged_order() {
        let mut state = state_with("splice", &[], &["a", "b"]);
        let live = ["c", "a", "b"].map(|s| s.to_string());
        // Dropping the never-dragged c onto a makes the whole visible order
        // the manual order.
        state.insert_before(&live, "c", Some("a")).unwrap();
        assert_eq!(state.order, vec!["c", "a", "b"]);
        // Without an anchor the session moves to the end of the manual order.
        state.insert_before(&live, "a", None).unwrap();
        assert_eq!(state.order, vec!["c", "b", "a"]);
        // A drop onto itself leaves the order alone.
        state.insert_before(&live, "b", Some("b")).unwrap();
        assert_eq!(state.order, vec!["c", "b", "a"]);
    }

    #[test]
    fn insert_before_rejects_unknown_session_or_anchor() {
        let mut state = state_with("move-invalid", &[], &["a"]);
        let live = ["a", "b"].map(|s| s.to_string());
        assert!(state.insert_before(&live, "ghost", Some("a")).is_err());
        assert!(state.insert_before(&live, "a", Some("ghost")).is_err());
        // A refusal leaves the saved order untouched.
        assert_eq!(state.order, vec!["a"]);
    }

    #[test]
    fn a_corrupt_state_file_fails_loud_not_silently() {
        let home = temp_home("corrupt");
        let path = state_path(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(
            load(&home).is_err(),
            "corrupt sidebar state must not read as defaults"
        );
        // The view helpers degrade to the runtime order instead of failing
        // an unrelated flow.
        let live = ["a".to_string()];
        assert_eq!(ordered_live(&home, &live), vec!["a"]);
        assert_eq!(pinned_live(&home, &live), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn workspace_ids_keep_the_legacy_default_and_hash_custom_paths() {
        let cwd = PathBuf::from("/work/project");
        assert_eq!(
            workspace_id_for(&cwd, &cwd),
            super::super::DEFAULT_WORKSPACE_ID
        );
        assert_eq!(
            workspace_id_for(&cwd.join("."), &cwd),
            super::super::DEFAULT_WORKSPACE_ID,
            "the same directory spelled differently keeps the default id"
        );
        let custom = PathBuf::from("/work/other");
        let id = workspace_id_for(&custom, &cwd);
        assert!(id.starts_with("ws_"));
        assert_eq!(
            id,
            workspace_id_for(&custom, &cwd),
            "the id is a pure function of the path"
        );
        assert_ne!(id, workspace_id_for(&PathBuf::from("/work/third"), &cwd));
    }

    #[test]
    fn registry_round_trips_and_a_missing_file_is_none() {
        let home = temp_home("registry");
        assert!(load_registry(&home).unwrap().is_none());
        let entry = WorkspaceEntry {
            workspace_id: "ws_test".into(),
            path: PathBuf::from("/work/project"),
            title: "project".into(),
            created_at: now_iso(),
            updated_at: now_iso(),
        };
        save_registry(&home, std::slice::from_ref(&entry)).unwrap();
        assert_eq!(load_registry(&home).unwrap(), Some(vec![entry.clone()]));
        // An explicit empty list stays distinguishable from a fresh install.
        save_registry(&home, &[]).unwrap();
        assert_eq!(load_registry(&home).unwrap(), Some(Vec::new()));
        // A corrupt file fails loud instead of reading as a fresh install.
        std::fs::write(registry_path(&home), "{ not json").unwrap();
        assert!(load_registry(&home).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn default_entry_is_titled_and_stable_in_identity() {
        let cwd = PathBuf::from("/work/project");
        let entry = default_entry(&cwd);
        assert_eq!(entry.workspace_id, super::super::DEFAULT_WORKSPACE_ID);
        assert_eq!(entry.title, "project");
        assert_eq!(entry.path, cwd);
        assert!(paths_match(Path::new("/work/project/."), &cwd));
        assert!(!paths_match(Path::new("/work/other"), &cwd));
    }
}
