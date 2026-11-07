//! `workspace/pinSession|unpinSession|insertSessionBefore` — the sidebar's
//! pin set and manual session order.
//!
//! These are desktop-shell preferences, not engine state: the runtime owns
//! threads, the desktop owns how the sidebar groups them. A pinned session
//! leads its section, most recently pinned first (the upstream registry
//! contract); the manual order is the workspace view's `sessionIds` order —
//! sessions that were never dragged keep following the runtime's recency
//! order after the manually ordered ones, which is exactly the merge the
//! client's own reconcile applies (vendored `ui-workspace` `tree.ts`).
//!
//! State persists at `~/.codesmith/desktop/workspace.json` beside the
//! settings document, loaded per operation. A missing file is a fresh
//! install (defaults); a corrupt file fails that one operation loud instead
//! of silently resetting the user's pins.
//!
//! Wire contracts (vendored `api-workspace-controller`):
//! - `pinSession {sessionId}` / `unpinSession {sessionId}` →
//!   `{pinnedSessionIds}` (the complete set, MRU-first; unpin is an
//!   idempotent no-op for an unpinned id); an unknown session →
//!   `session/not-found`, an archived one → `gateway/bad-request`; every
//!   mutation is echoed to `workspace/follow` as
//!   `{type:'pinned', pinnedSessionIds}` so other connected clients reorder.
//! - `insertSessionBefore {workspaceId, sessionId, beforeSessionId?}` → the
//!   updated workspace view (echoed as an `upsert` increment); an unknown
//!   workspace → `workspace/not-found`, an unknown session or anchor →
//!   `workspace/move-invalid`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::HostState;

/// The persisted sidebar state: `{ "pinned": […], "order": […] }`.
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
    let session_id = args
        .get("sessionId")
        .and_then(Value::as_str)
        .context("workspace pin without sessionId")
        .map_err(internal)?
        .to_string();
    let (live, archived) = thread_sets(host).await.map_err(internal)?;
    let mut state = load(&host.settings_home()).map_err(internal)?;
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
    state.save().map_err(internal)?;
    let pins = state.live_pins(&live);
    host.push_live(
        "workspace/follow",
        json!({ "type": "pinned", "pinnedSessionIds": pins }),
    )
    .await;
    Ok(json!({ "pinnedSessionIds": pins }))
}

/// `workspace/insertSessionBefore`: splice the manual order and answer the
/// updated workspace view, echoed as an `upsert` increment (the shape
/// `session/create` already established).
pub async fn insert_session_before(host: &HostState, args: &Value) -> Result<Value, OpError> {
    let workspace_id = args
        .get("workspaceId")
        .and_then(Value::as_str)
        .context("workspace move without workspaceId")
        .map_err(internal)?;
    if workspace_id != super::DEFAULT_WORKSPACE_ID {
        return Err(OpError::code(
            "workspace/not-found",
            format!("workspace {workspace_id} not found"),
        ));
    }
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
    let (live, _archived) = thread_sets(host).await.map_err(internal)?;
    let mut state = load(&host.settings_home()).map_err(internal)?;
    state
        .insert_before(&live, &session_id, before.as_deref())
        .map_err(|err| OpError::code("workspace/move-invalid", format!("{err:#}")))?;
    state.save().map_err(internal)?;
    let view = super::workspace_view_live(host).await;
    host.push_live(
        "workspace/follow",
        json!({ "type": "upsert", "workspace": view.clone() }),
    )
    .await;
    Ok(view)
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
}
