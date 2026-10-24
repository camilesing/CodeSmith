//! Persistent RLM session state for the v0.8.33 head/hands tool surface.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::models::{ContentBlock, Message, SystemPrompt};
use crate::repl::PythonRuntime;
use crate::utils::sha256_hex;

pub type SharedRlmSessionStore = Arc<Mutex<HashMap<String, Arc<Mutex<RlmSession>>>>>;

#[must_use]
pub fn new_shared_rlm_session_store() -> SharedRlmSessionStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Debug)]
pub struct RlmSession {
    pub name: String,
    pub id: String,
    pub kernel: Option<PythonRuntime>,
    pub context_meta: ContextMeta,
    pub config: RlmSessionConfig,
    pub rpc_count: u32,
    pub total_duration: Duration,
    pub peak_var_count: usize,
    pub final_count: usize,
    pub created_at: Instant,
    pub last_used_at: Instant,
    pub context_path: PathBuf,
}

impl RlmSession {
    #[must_use]
    pub fn new(
        name: String,
        kernel: PythonRuntime,
        context_meta: ContextMeta,
        context_path: PathBuf,
    ) -> Self {
        let now = Instant::now();
        Self {
            name,
            id: format!("rlm:{}", Uuid::new_v4().simple()),
            kernel: Some(kernel),
            context_meta,
            config: RlmSessionConfig::default(),
            rpc_count: 0,
            total_duration: Duration::ZERO,
            peak_var_count: 0,
            final_count: 0,
            created_at: now,
            last_used_at: now,
            context_path,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMeta {
    pub length: usize,
    #[serde(rename = "type")]
    pub type_name: String,
    pub preview_500: String,
    pub sha256: String,
}

impl ContextMeta {
    #[must_use]
    pub fn from_body(body: &str, type_name: impl Into<String>) -> Self {
        Self {
            length: body.chars().count(),
            type_name: type_name.into(),
            preview_500: body.chars().take(500).collect(),
            sha256: sha256_hex(body.as_bytes()),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputFeedback {
    Full,
    Metadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RlmSessionConfig {
    pub output_feedback: OutputFeedback,
    pub sub_query_timeout_secs: u64,
    pub sub_rlm_max_depth: u32,
    pub share_session: bool,
}

impl Default for RlmSessionConfig {
    fn default() -> Self {
        Self {
            output_feedback: OutputFeedback::Full,
            sub_query_timeout_secs: 120,
            sub_rlm_max_depth: 1,
            share_session: false,
        }
    }
}

/// Stage prompt/transcript data for the Python REPL under an unpredictable
/// per-call directory with owner-only permissions: the staged text is
/// sensitive, and a fixed shared name under the system temp dir is
/// world-readable under the default umask and a pre-creation/symlink target
/// for other local users. `create_dir` (not `create_dir_all`) fails loud if
/// the exact directory already exists.
///
/// The first call in a process also sweeps staging directories left behind
/// by crashed previous runs ([`sweep_stale_rlm_context_dirs`]) — the normal
/// cleanup paths remove only `context.txt`, never the parent, so a hard
/// crash (kill -9, power loss) leaks one directory per interrupted turn.
pub async fn write_context_file(body: &str) -> std::io::Result<PathBuf> {
    sweep_stale_context_dirs_once().await;
    let dir = std::env::temp_dir().join(format!(
        "deepseek_rlm_ctx_{}_{}",
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    let path = dir.join("context.txt");
    // mkdir(0700) is atomic hardening: a plain create_dir followed by
    // chmod left a window where the staging dir was group/other-traversable,
    // and the guarantee silently vanished if the calls were ever reordered.
    #[cfg(unix)]
    {
        tokio::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .await?;
    }
    #[cfg(not(unix))]
    {
        tokio::fs::create_dir(&dir).await?;
    }
    let write_result = write_context_body(&path, body).await;
    if write_result.is_err() {
        // A half-staged directory (open/write failure) is garbage nobody
        // will ever clean: this process is the only one that knew the UUID.
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
    write_result.map(|()| path)
}

async fn write_context_body(path: &Path, body: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        // tokio's OpenOptions has an inherent unix `.mode()` — no trait
        // import needed (unlike std's).
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, body.as_bytes()).await?;
    }
    #[cfg(not(unix))]
    {
        // Exclusive create is portable and cheap; the 0600 restriction is
        // unix-only (tracked as a platform caveat, not a guarantee here).
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, body.as_bytes()).await?;
    }
    Ok(())
}

/// Prefix of every RLM staging directory under the system temp dir.
#[cfg(unix)]
const RLM_CTX_DIR_PREFIX: &str = "deepseek_rlm_ctx_";

/// Run [`sweep_stale_rlm_context_dirs`] at most once per process: the
/// winner of the `set` race sweeps, everyone else proceeds (a double sweep
/// is harmless — removals tolerate `NotFound` — but pointless). No-op
/// outside unix, where there is no sweep to run.
async fn sweep_stale_context_dirs_once() {
    static SWEPT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if SWEPT.set(()).is_ok() {
        #[cfg(unix)]
        let _ = tokio::task::spawn_blocking(sweep_stale_rlm_context_dirs).await;
        #[cfg(not(unix))]
        {}
    }
}

/// Remove RLM staging directories whose owning process is gone. Directory
/// names embed the staging pid (`deepseek_rlm_ctx_{pid}_{uuid}`); a dead
/// pid means the run crashed before its cleanup paths ran, and the dir may
/// hold a partially written `context.txt` with sensitive prompt data.
///
/// Liveness is probed with `kill -0 <pid>` (a real signal is never sent):
/// exit 0 means some process with that id exists — same-uid processes
/// always answer, and a foreign-uid owner makes `kill` fail while its
/// 0700 directory refuses the removal anyway, so misclassification can
/// only produce a harmless failed `remove_dir_all`. Unix-only: non-unix
/// staging dirs still accumulate after crashes (known limitation, same
/// family as the non-unix permission caveat).
#[cfg(unix)]
pub fn sweep_stale_rlm_context_dirs() {
    let tmp = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&tmp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(RLM_CTX_DIR_PREFIX) else {
            continue;
        };
        let Some((pid_str, _uuid)) = rest.split_once('_') else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == std::process::id() || pid_alive(pid) {
            continue;
        }
        // Best-effort: another crashed-run sweeper or the owner's dying
        // gasp may remove it first; anything left is still swept next boot.
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// `kill -0`-style liveness probe without sending a signal. Spawning the
/// `kill` binary avoids an `unsafe libc::kill` call in this crate
/// (`deny(unsafe_code)` with narrowly-scoped allows elsewhere); the sweep
/// runs once per process over a handful of entries, so the spawn cost is
/// irrelevant.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[derive(Debug, Clone)]
pub struct SessionObjectSnapshot {
    pub session_id: String,
    pub model: String,
    pub workspace: PathBuf,
    pub system_prompt: Option<SystemPrompt>,
    /// Arc-shared from the session's `AppendLog` snapshot: the per-turn
    /// tool-context build used to deep-clone the whole history here.
    pub messages: Arc<Vec<Message>>,
}

impl SessionObjectSnapshot {
    #[must_use]
    pub fn new(
        session_id: String,
        model: String,
        workspace: PathBuf,
        system_prompt: Option<SystemPrompt>,
        messages: Arc<Vec<Message>>,
    ) -> Self {
        Self {
            session_id,
            model,
            workspace,
            system_prompt,
            messages,
        }
    }

    #[must_use]
    pub fn object_cards(&self) -> Vec<SessionObjectCard> {
        let mut cards = Vec::new();
        for object in self.base_objects() {
            cards.push(SessionObjectCard::from_resolved(&object));
        }
        for (index, message) in self.messages.iter().enumerate() {
            cards.push(SessionObjectCard::from_resolved(&message_resolved_object(
                index,
                message,
                "Transcript message",
            )));
        }
        cards
    }

    #[must_use]
    pub fn resolve(&self, object_ref: &str) -> Option<ResolvedSessionObject> {
        let normalized = normalize_session_object_ref(object_ref);
        match normalized.as_str() {
            "session://active/session" => Some(self.session_metadata_object()),
            "session://active/system_prompt" => self.system_prompt_object(),
            "session://active/transcript" => Some(self.transcript_object()),
            "session://active/latest_user" => self.latest_user_object(),
            _ => self.message_object(&normalized),
        }
    }

    fn base_objects(&self) -> Vec<ResolvedSessionObject> {
        let mut objects = vec![self.session_metadata_object()];
        if let Some(object) = self.system_prompt_object() {
            objects.push(object);
        }
        objects.push(self.transcript_object());
        if let Some(object) = self.latest_user_object() {
            objects.push(object);
        }
        objects
    }

    fn session_metadata_object(&self) -> ResolvedSessionObject {
        let body = json!({
            "session_id": self.session_id,
            "model": self.model,
            "workspace": self.workspace.display().to_string(),
            "message_count": self.messages.len(),
            "object_refs": {
                "system_prompt": "session://active/system_prompt",
                "transcript": "session://active/transcript",
                "latest_user": "session://active/latest_user",
                "message_prefix": "session://active/messages/"
            }
        })
        .to_string();
        ResolvedSessionObject::new(
            "session://active/session",
            "session_metadata",
            "Active session metadata",
            body,
        )
    }

    fn system_prompt_object(&self) -> Option<ResolvedSessionObject> {
        let prompt = self.system_prompt.as_ref()?;
        Some(ResolvedSessionObject::new(
            "session://active/system_prompt",
            "system_prompt",
            "Active system prompt",
            render_system_prompt(prompt),
        ))
    }

    fn transcript_object(&self) -> ResolvedSessionObject {
        let body = self
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| compact_message_json(index, message).to_string())
            .collect::<Vec<_>>()
            .join("\n");
        ResolvedSessionObject::new(
            "session://active/transcript",
            "transcript",
            "Active transcript as JSONL",
            body,
        )
    }

    fn latest_user_object(&self) -> Option<ResolvedSessionObject> {
        self.messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, message)| message.role == "user")
            .map(|(index, message)| message_resolved_object(index, message, "Latest user message"))
    }

    fn message_object(&self, normalized: &str) -> Option<ResolvedSessionObject> {
        let index = normalized
            .strip_prefix("session://active/messages/")?
            .parse::<usize>()
            .ok()?;
        self.messages
            .get(index)
            .map(|message| message_resolved_object(index, message, "Transcript message"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionObjectCard {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub length: usize,
    pub preview_500: String,
    pub sha256: String,
}

impl SessionObjectCard {
    #[must_use]
    pub fn from_resolved(object: &ResolvedSessionObject) -> Self {
        Self {
            id: object.id.clone(),
            kind: object.kind.clone(),
            title: object.title.clone(),
            length: object.body.chars().count(),
            preview_500: object.body.chars().take(500).collect(),
            sha256: sha256_hex(object.body.as_bytes()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSessionObject {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
}

impl ResolvedSessionObject {
    fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            body: body.into(),
        }
    }
}

fn normalize_session_object_ref(object_ref: &str) -> String {
    let trimmed = object_ref.trim();
    if trimmed.starts_with("session://") {
        trimmed.to_string()
    } else {
        format!("session://active/{}", trimmed.trim_start_matches('/'))
    }
}

fn render_system_prompt(prompt: &SystemPrompt) -> String {
    match prompt {
        SystemPrompt::Text(text) => text.clone(),
        SystemPrompt::Blocks(blocks) => blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

fn message_resolved_object(index: usize, message: &Message, title: &str) -> ResolvedSessionObject {
    ResolvedSessionObject::new(
        format!("session://active/messages/{index}"),
        "message",
        format!("{title} {index} ({})", message.role),
        compact_message_json(index, message).to_string(),
    )
}

fn compact_message_json(index: usize, message: &Message) -> Value {
    json!({
        "index": index,
        "role": message.role,
        "content": message.content.iter().map(compact_content_block).collect::<Vec<_>>(),
    })
}

fn compact_content_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text, .. } => json!({
            "type": "text",
            "text": text,
        }),
        // `redacted: true` promises the chain-of-thought is withheld —
        // size/identity metadata is kept, but no preview text: these cards
        // surface to callers and model-visible session objects, and a
        // 240-char preview leaked the CoT while the JSON claimed it was
        // redacted.
        ContentBlock::Thinking { thinking } => json!({
            "type": "thinking",
            "redacted": true,
            "chars": thinking.chars().count(),
            "sha256": sha256_hex(thinking.as_bytes()),
        }),
        ContentBlock::Image { source } => json!({
            "type": "image",
            "summary": source.summary(),
        }),
        ContentBlock::ToolUse {
            id,
            name,
            input,
            caller,
        } => json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
            "caller": caller,
        }),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            content_blocks,
        } => {
            let chars = content.chars().count();
            let large = chars > 2_000;
            json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "is_error": is_error,
                "content": if large { Value::Null } else { Value::String(content.clone()) },
                "content_preview": truncate_chars(content, 500),
                "content_chars": chars,
                "content_sha256": sha256_hex(content.as_bytes()),
                "content_redacted": large,
                "content_blocks": content_blocks,
            })
        }
        ContentBlock::ServerToolUse { id, name, input } => json!({
            "type": "server_tool_use",
            "id": id,
            "name": name,
            "input": input,
        }),
        ContentBlock::ToolSearchToolResult {
            tool_use_id,
            content,
        } => json!({
            "type": "tool_search_tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
        }),
        ContentBlock::CodeExecutionToolResult {
            tool_use_id,
            content,
        } => json!({
            "type": "code_execution_tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
        }),
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars < 3 {
        return text.chars().take(max_chars).collect();
    }
    let take = max_chars - 3;
    let mut out: String = text.chars().take(take).collect();
    out.push_str("...");
    out
}

#[must_use]
pub fn derive_session_name(source_hint: Option<&str>) -> String {
    let hint = source_hint
        .and_then(|raw| {
            Path::new(raw)
                .file_name()
                .and_then(|name| name.to_str())
                .or(Some(raw))
        })
        .unwrap_or("context");
    let mut out = String::new();
    for ch in hint.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
        if out.len() >= 48 {
            break;
        }
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        "context".to_string()
    } else {
        out.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EnvVarGuard, lock_test_env};

    #[test]
    fn derive_session_name_slugifies_path() {
        assert_eq!(
            derive_session_name(Some("src/Big File.rs")),
            "big_file_rs".to_string()
        );
    }

    #[cfg(unix)]
    #[test]
    fn sweep_removes_only_dead_owner_staging_dirs() {
        let _env = lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let _guard = EnvVarGuard::set("TMPDIR", tmp.path());
        // 999999999 exceeds macOS/Linux default pid spaces, so no live
        // process can ever own it; our own pid is alive by definition.
        let dead = tmp.path().join("deepseek_rlm_ctx_999999999_abc123def");
        let live = tmp
            .path()
            .join(format!("deepseek_rlm_ctx_{}_abc123def", std::process::id()));
        let unrelated = tmp.path().join("deepseek_other_999999999_x");
        for dir in [&dead, &live, &unrelated] {
            std::fs::create_dir_all(dir).expect("seed dir");
            std::fs::write(dir.join("context.txt"), "stale transcript").expect("seed file");
        }

        sweep_stale_rlm_context_dirs();

        assert!(!dead.exists(), "dead-owner staging dir must be swept");
        assert!(!dead.join("context.txt").exists());
        assert!(live.exists(), "live-owner staging dir must survive");
        assert!(unrelated.exists(), "non-RLM dirs are not touched");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn staged_dir_and_file_are_owner_only() {
        let _env = lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let _guard = EnvVarGuard::set("TMPDIR", tmp.path());
        let path = write_context_file("secret transcript")
            .await
            .expect("stage");
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = std::fs::metadata(path.parent().expect("parent"))
            .expect("dir meta")
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700, "staging dir is owner-only");
        let file_mode = std::fs::metadata(&path)
            .expect("file meta")
            .permissions()
            .mode();
        assert_eq!(file_mode & 0o777, 0o600, "staged file is owner-only");
        let _ = tokio::fs::remove_dir_all(path.parent().expect("parent")).await;
    }

    #[test]
    fn context_meta_hashes_and_previews_body() {
        let meta = ContextMeta::from_body("abcdef", "text");
        assert_eq!(meta.length, 6);
        assert_eq!(meta.preview_500, "abcdef");
        assert_eq!(
            meta.sha256,
            "bef57ec7f53a6d40beb640a780a639c83bc29ac8a9816f1fc6c5c6dcd93c4721"
        );
    }

    #[test]
    fn session_objects_expose_prompt_and_transcript_cards() {
        let snapshot = SessionObjectSnapshot::new(
            "session-1".to_string(),
            "deepseek-v4-pro".to_string(),
            PathBuf::from("/tmp/work"),
            Some(SystemPrompt::Text("system body".to_string())),
            std::sync::Arc::new(vec![Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "hello RLM".to_string(),
                    cache_control: None,
                }],
            }]),
        );

        let cards = snapshot.object_cards();
        assert!(
            cards
                .iter()
                .any(|card| card.id == "session://active/system_prompt")
        );
        assert!(
            cards
                .iter()
                .any(|card| card.id == "session://active/messages/0")
        );

        let transcript = snapshot
            .resolve("session://active/transcript")
            .expect("transcript object");
        assert!(transcript.body.contains("hello RLM"));
    }

    #[test]
    fn session_object_transcript_keeps_large_tool_results_compact() {
        let large = "tool output\n".repeat(400);
        let snapshot = SessionObjectSnapshot::new(
            "session-1".to_string(),
            "deepseek-v4-pro".to_string(),
            PathBuf::from("/tmp/work"),
            None,
            std::sync::Arc::new(vec![Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: large.clone(),
                    is_error: None,
                    content_blocks: None,
                }],
            }]),
        );

        let object = snapshot
            .resolve("session://active/messages/0")
            .expect("message object");
        assert!(object.body.contains("\"content_redacted\":true"));
        assert!(object.body.len() < large.len());
    }
}
