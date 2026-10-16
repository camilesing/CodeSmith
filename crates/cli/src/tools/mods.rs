//! `manage_mods` tool — the model-visible surface of the script Mods layer
//! (§F script mod layer Phase C). Lets the agent write + install mods for
//! the user within the current session, mirroring Claude Code Mods'
//! agent-self-install posture.
//!
//! Approval posture (plan §五): `list` / `reload` are `Auto`; `write` /
//! `activate` / `disable` / `remove` return [`ApprovalRequirement::Required`]
//! from `approval_requirement_for_input`, so the user sees — and must
//! approve — exactly the calls that persist in-process code or change what
//! runs next turn. `activate` is the one-time consent moment: the approval
//! prompt surfaces the mod's id/version/description via this tool's schema
//! + result text.
//!
//! State handling: every mutating action loads a fresh
//! [`ModStateStore`](crate::mod_state::ModStateStore) from disk (the store
//! is file-backed; the `/mods` commands and this tool can never diverge).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::mod_ops::{self, ModReloadCtx};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

pub struct ManageModsTool {
    /// Captured at engine build; `None` short-circuits the reload actions
    /// with a clear error (embeds/tests that skip the engine).
    pub reload: Option<ModReloadCtx>,
}

#[async_trait]
impl ToolSpec for ManageModsTool {
    fn name(&self) -> &'static str {
        "manage_mods"
    }

    fn description(&self) -> &'static str {
        "Manage CodeSmith script mods (Rhai): list discovered mods, write a new mod's files \
         (mod.toml + mod.rhai), activate a discovered mod (one-time user consent; hooks/tools/commands \
         go live immediately after), disable/enable, remove, or reload the mod layer. \
         `list` also shows the resolved policy layer (each kind's effective value and the winning mod id). \
         `session_write` binds an ephemeral session experiment mod from inline Rhai source (no disk writes, \
         gone when the session ends; an identical re-write needs no fresh approval) — verify it with \
         `list`, then persist a proven experiment via `write` + `activate`. \
         Mods persist across sessions; activation and file writes require user approval."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "list",
                        "write",
                        "activate",
                        "disable",
                        "enable",
                        "remove",
                        "reload",
                        "session_write",
                        "session_remove"
                    ],
                    "description": "Operation to perform."
                },
                "id": {
                    "type": "string",
                    "description": "Mod id (directory name under the mods root; for session_write/session_remove a 'session-' prefixed id of your choice). Required for write/session_write/activate/disable/enable/remove/session_remove."
                },
                "source": {
                    "type": "string",
                    "description": "For action=session_write: the mod's Rhai source (inline, no files on disk)."
                },
                "files": {
                    "type": "array",
                    "description": "For action=write: the mod's files as {path, content} pairs. Must include mod.toml; paths are relative to the mod directory (no absolute paths, no '..').",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"},
                            "content": {"type": "string"}
                        },
                        "required": ["path", "content"],
                        "additionalProperties": false
                    }
                },
                "global": {
                    "type": "boolean",
                    "description": "For action=write: install into the user-global mods root (~/.codesmith/mods) instead of the project root (<workspace>/.codesmith/mods). Default false."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // No static capability implies approval; the per-action gate in
        // `approval_requirement_for_input` is the single source of truth.
        Vec::new()
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn approval_requirement_for_input(
        &self,
        input: &Value,
        _context: &ToolContext,
    ) -> ApprovalRequirement {
        match input.get("action").and_then(Value::as_str) {
            // Read-only / idempotent ops run without a prompt.
            Some("list") | Some("reload") => ApprovalRequirement::Auto,
            // A session re-write of EXACTLY the (id, source) the user
            // already approved this session skips the prompt — the
            // session-scoped analogue of the disk tier's activation hash.
            // Any content change falls through to Required.
            Some("session_write") if self.session_write_preapproved(input) => {
                ApprovalRequirement::Auto
            }
            // Persisting in-process code + changing what runs next turn:
            // every one of these is the user-consent surface (plan §五).
            Some("write")
            | Some("activate")
            | Some("disable")
            | Some("enable")
            | Some("remove")
            | Some("session_write")
            | Some("session_remove") => ApprovalRequirement::Required,
            // Unknown action: the execute path rejects it; require approval
            // so a malformed call can't slip through un-reviewed.
            _ => ApprovalRequirement::Required,
        }
    }

    fn is_destructive(&self, input: &Value) -> bool {
        matches!(
            input.get("action").and_then(Value::as_str),
            Some("write")
                | Some("activate")
                | Some("disable")
                | Some("enable")
                | Some("remove")
                | Some("session_write")
                | Some("session_remove")
        )
    }

    fn supports_parallel(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::missing_field("action"))?;
        let id = input
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let workspace = &context.workspace;

        let require_id = || {
            if id.is_empty() {
                Err(ToolError::invalid_input(format!(
                    "action '{action}' requires a non-empty `id`"
                )))
            } else {
                Ok(())
            }
        };

        match action {
            "list" => {
                // Disk-bound (state load + recursive discovery scan) — keep
                // it off the tokio worker, like `reload_note` does. The
                // policy section is runner-memory only (mutex + clone) and
                // rides the same blocking closure.
                let ws = workspace.clone();
                let runner = self.reload.as_ref().map(|r| Arc::clone(&r.runner));
                let listing = tokio::task::spawn_blocking(move || {
                    let state = crate::mod_state::ModStateStore::load_default()
                        .map_err(|e| ToolError::not_available(format!("load mod state: {e}")))?;
                    Ok::<String, ToolError>(mod_ops::list_mods(&ws, &state, runner.as_deref()))
                })
                .await
                .map_err(|e| ToolError::execution_failed(format!("join: {e}")))??;
                Ok(ToolResult::success(listing))
            }
            "write" => {
                require_id()?;
                let global = input
                    .get("global")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let files = parse_files(input.get("files"))?;
                let ws = workspace.clone();
                let id_in = id.clone();
                let dir = tokio::task::spawn_blocking(move || {
                    mod_ops::write_mod(&ws, &id_in, &files, global)
                })
                .await
                .map_err(|e| ToolError::execution_failed(format!("join: {e}")))?
                .map_err(ToolError::invalid_input)?;
                Ok(ToolResult::success(format!(
                    "Wrote mod '{id}' to {}. It is NOT active yet — the user activates it \
                     (this action=activate call requires their approval), then it loads.",
                    dir.display()
                ))
                .with_metadata(json!({"mod_id": id, "path": dir.display().to_string()})))
            }
            "activate" => {
                require_id()?;
                // Serialize with the /mods command path: each persist
                // rewrites the whole state file, so unlocked racing
                // mutations are last-writer-wins. The guard scopes to
                // load→mutate→persist ONLY — the per-mod load after it may
                // re-acquire the guard at the activation-hash backfill
                // (std Mutex is not reentrant). All of it is synchronous
                // disk work — off the tokio worker, like the reload path.
                let ws = workspace.clone();
                let id_in = id.clone();
                let reload_ctx = self.reload.clone();
                let msg = tokio::task::spawn_blocking(move || {
                    let _guard = crate::mod_ops::mod_state_lock();
                    let mut state = crate::mod_state::ModStateStore::load_default()
                        .map_err(|e| ToolError::not_available(format!("load mod state: {e}")))?;
                    mod_ops::activate(&ws, &mut state, &id_in).map_err(ToolError::execution_failed)
                })
                .await
                .map_err(|e| ToolError::execution_failed(format!("join: {e}")))??;
                let note = self
                    .per_mod_note(reload_ctx, workspace, &id, PerModTransition::Load)
                    .await;
                Ok(ToolResult::success(format!("{msg}\n{note}")))
            }
            "disable" | "enable" => {
                require_id()?;
                let id_in = id.clone();
                let enable = action == "enable";
                let reload_ctx = self.reload.clone();
                let msg = tokio::task::spawn_blocking(move || {
                    let _guard = crate::mod_ops::mod_state_lock();
                    let mut state = crate::mod_state::ModStateStore::load_default()
                        .map_err(|e| ToolError::not_available(format!("load mod state: {e}")))?;
                    mod_ops::set_enabled(&mut state, &id_in, enable)
                        .map_err(ToolError::execution_failed)
                })
                .await
                .map_err(|e| ToolError::execution_failed(format!("join: {e}")))??;
                let transition = if enable {
                    PerModTransition::Load
                } else {
                    PerModTransition::Unload
                };
                let note = self
                    .per_mod_note(reload_ctx, workspace, &id, transition)
                    .await;
                Ok(ToolResult::success(format!("{msg}\n{note}")))
            }
            "remove" => {
                require_id()?;
                let ws = workspace.clone();
                let id_in = id.clone();
                let reload_ctx = self.reload.clone();
                let msg = tokio::task::spawn_blocking(move || {
                    let _guard = crate::mod_ops::mod_state_lock();
                    let mut state = crate::mod_state::ModStateStore::load_default()
                        .map_err(|e| ToolError::not_available(format!("load mod state: {e}")))?;
                    mod_ops::remove(&ws, &mut state, &id_in).map_err(ToolError::execution_failed)
                })
                .await
                .map_err(|e| ToolError::execution_failed(format!("join: {e}")))??;
                let note = self
                    .per_mod_note(reload_ctx, workspace, &id, PerModTransition::Unload)
                    .await;
                Ok(ToolResult::success(format!("{msg}\n{note}")))
            }
            "session_write" => {
                require_id()?;
                let Some(source) = input.get("source").and_then(Value::as_str) else {
                    return Err(ToolError::invalid_input(
                        "action 'session_write' requires a non-empty `source` (the Rhai script)"
                            .to_string(),
                    ));
                };
                let Some(ctx) = self.reload.clone() else {
                    return Err(ToolError::not_available(
                        "mod layer not live in this session (session mods need the extension runner)"
                            .to_string(),
                    ));
                };
                let id_in = id.clone();
                let src = source.to_string();
                // Rhai compilation + the runner transition — synchronous,
                // off the tokio worker like the other mod transitions.
                let note = tokio::task::spawn_blocking(move || {
                    mod_ops::session_write_mod(&ctx, &id_in, &src)
                })
                .await
                .map_err(|e| ToolError::execution_failed(format!("join: {e}")))?;
                Ok(ToolResult::success(note))
            }
            "session_remove" => {
                require_id()?;
                let Some(ctx) = self.reload.clone() else {
                    return Err(ToolError::not_available(
                        "mod layer not live in this session (session mods need the extension runner)"
                            .to_string(),
                    ));
                };
                let id_in = id.clone();
                let note =
                    tokio::task::spawn_blocking(move || mod_ops::session_remove_mod(&ctx, &id_in))
                        .await
                        .map_err(|e| ToolError::execution_failed(format!("join: {e}")))?;
                Ok(ToolResult::success(note))
            }
            "reload" => Ok(ToolResult::success(self.reload_note().await)),
            other => Err(ToolError::invalid_input(format!(
                "unknown action {other:?} (expected list | write | session_write | activate | disable | enable | remove | reload | session_remove)"
            ))),
        }
    }
}

impl ManageModsTool {
    /// Whether this exact session-write input — same id AND same source —
    /// was already user-approved this session (the approval-memoization
    /// half of the session-mod consent rule; only a successful bind
    /// records it).
    fn session_write_preapproved(&self, input: &Value) -> bool {
        let Some(ctx) = self.reload.as_ref() else {
            return false;
        };
        match (
            input.get("id").and_then(Value::as_str),
            input.get("source").and_then(Value::as_str),
        ) {
            (Some(id), Some(source)) => ctx.session_mods.is_approved(id, source),
            _ => false,
        }
    }

    /// Which single-mod runtime transition follows a persisted state
    /// change (the per-mod counterpart of `reload_note`'s whole-layer
    /// reload).
    async fn per_mod_note(
        &self,
        reload_ctx: Option<mod_ops::ModReloadCtx>,
        workspace: &std::path::Path,
        id: &str,
        transition: PerModTransition,
    ) -> String {
        let Some(ctx) = reload_ctx else {
            return "No extension runner in this session; the change takes effect at the next session start."
                .to_string();
        };
        let runner = ctx.runner;
        let ws = workspace.to_path_buf();
        let id_in = id.to_string();
        // Synchronous disk work (discovery scan, SHA-256, Rhai
        // compilation on load) — off the tokio worker, like `reload_note`.
        tokio::task::spawn_blocking(move || match transition {
            PerModTransition::Load => mod_ops::load_mod(&runner, &ws, &id_in, ctx.mods_enabled),
            PerModTransition::Unload => mod_ops::unload_mod(&runner, &id_in),
        })
        .await
        .unwrap_or_else(|e| format!("mod transition failed: {e}"))
    }

    async fn reload_note(&self) -> String {
        match &self.reload {
            // mods_enabled captured at engine build (next to the runner) —
            // a tool-triggered reload must not resurrect the mod layer
            // against `[mods] enabled = false`. reload_mods is synchronous
            // disk work (recursive scans, TOML parsing, Rhai compilation)
            // — keep it off the tokio worker, like the watcher path does.
            Some(ctx) => {
                let runner = Arc::clone(&ctx.runner);
                let workspace = ctx.workspace.clone();
                let token = ctx.shared_cancel_token.clone();
                let mods_enabled = ctx.mods_enabled;
                let session_mods = Arc::clone(&ctx.session_mods);
                tokio::task::spawn_blocking(move || {
                    mod_ops::reload_mods(&runner, &workspace, token, mods_enabled, &session_mods)
                })
                .await
                .unwrap_or_else(|e| format!("mods reload failed: {e}"))
            }
            None => "Reload unavailable in this session (no bound extension runner); \
                     changes take effect on the next session start or /mods reload."
                .to_string(),
        }
    }
}

/// The single-mod runtime transition that follows a persisted state change
/// in `manage_mods` — see [`ManageModsTool::per_mod_note`].
#[derive(Clone, Copy)]
enum PerModTransition {
    /// Compile + bind + retain (activate / enable).
    Load,
    /// Evict the compiled form + replay survivors (disable / remove).
    Unload,
}

/// Parse the `files` array into `(path, content)` pairs.
fn parse_files(raw: Option<&Value>) -> Result<Vec<(String, String)>, ToolError> {
    let Some(arr) = raw else {
        return Err(ToolError::missing_field("files"));
    };
    let arr = arr
        .as_array()
        .ok_or_else(|| ToolError::invalid_input("`files` must be an array"))?;
    if arr.is_empty() {
        return Err(ToolError::invalid_input(
            "`files` must contain at least one entry (mod.toml is required)",
        ));
    }
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::invalid_input("each `files` entry needs a string `path`"))?;
        let content = item.get("content").and_then(Value::as_str).ok_or_else(|| {
            ToolError::invalid_input("each `files` entry needs a string `content`")
        })?;
        out.push((path.to_string(), content.to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx_in(tmp: &tempfile::TempDir) -> ToolContext {
        ToolContext::new(tmp.path().to_path_buf())
    }

    #[test]
    fn approval_matrix_per_action() {
        let tool = ManageModsTool { reload: None };
        let tool_ctx = ToolContext::new(PathBuf::from("."));
        let req = |action: &str| {
            tool.approval_requirement_for_input(&json!({"action": action}), &tool_ctx)
        };
        assert_eq!(req("list"), ApprovalRequirement::Auto);
        assert_eq!(req("reload"), ApprovalRequirement::Auto);
        assert_eq!(req("write"), ApprovalRequirement::Required);
        assert_eq!(req("activate"), ApprovalRequirement::Required);
        assert_eq!(req("disable"), ApprovalRequirement::Required);
        assert_eq!(req("enable"), ApprovalRequirement::Required);
        assert_eq!(req("remove"), ApprovalRequirement::Required);
        // Session actions with no live mod layer (reload: None → no
        // approved-content memo possible): both ask.
        assert_eq!(req("session_write"), ApprovalRequirement::Required);
        assert_eq!(req("session_remove"), ApprovalRequirement::Required);
        assert_eq!(
            tool.approval_requirement_for_input(&json!({}), &tool_ctx),
            ApprovalRequirement::Required
        );
    }

    #[tokio::test]
    async fn unknown_action_is_invalid_input() {
        let tool = ManageModsTool { reload: None };
        let tmp = tempfile::tempdir().unwrap();
        let err = tool
            .execute(json!({"action": "detonate"}), &ctx_in(&tmp))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown action"), "{err}");
    }

    #[tokio::test]
    async fn write_without_id_is_rejected() {
        let tool = ManageModsTool { reload: None };
        let tmp = tempfile::tempdir().unwrap();
        let err = tool
            .execute(json!({"action": "write", "files": []}), &ctx_in(&tmp))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("id"), "{err}");
    }

    #[tokio::test]
    async fn write_without_files_is_rejected() {
        let tool = ManageModsTool { reload: None };
        let tmp = tempfile::tempdir().unwrap();
        let err = tool
            .execute(json!({"action": "write", "id": "x"}), &ctx_in(&tmp))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("files"), "{err}");
    }

    #[tokio::test]
    async fn write_creates_project_mod_files() {
        let tool = ManageModsTool { reload: None };
        let tmp = tempfile::tempdir().unwrap();
        // The project mods root sits under the temp workspace; write_mod
        // gates on workspace trust, and a bare tempdir is untrusted by
        // default — so this exercises the trust refusal path…
        let res = tool
            .execute(
                json!({
                    "action": "write",
                    "id": "demo",
                    "files": [
                        {"path": "mod.toml", "content": "id = \"demo\"\nversion = \"0.1.0\"\n"},
                        {"path": "mod.rhai", "content": "mod_log(\"hi\");"}
                    ]
                }),
                &ctx_in(&tmp),
            )
            .await;
        match res {
            Ok(_) => panic!("untrusted workspace must refuse project mod writes"),
            Err(e) => assert!(e.to_string().contains("trust"), "{e}"),
        }
    }

    #[tokio::test]
    async fn schema_is_object_rooted_with_action_enum() {
        let tool = ManageModsTool { reload: None };
        let schema = tool.input_schema();
        assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
        let action = schema
            .pointer("/properties/action")
            .and_then(|v| v.get("enum"))
            .and_then(Value::as_array)
            .unwrap();
        assert!(action.iter().any(|v| v == "activate"));
    }
}
