//! Three-zone prompt contract types for prefix-cache stability (#2264).
//!
//! Divides every request into three rigid zones:
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │ PinnedPrefix (frozen after construction) │ ← system prompt + tool catalog
//! │   combined_sha256 computed at freeze()   │   cache hit candidate
//! ├─────────────────────────────────────────┤
//! │ AppendLog (append-only)                  │ ← conversation history
//! │   push() only, no insert / remove / edit │   preserves prefix of prior turns
//! ├─────────────────────────────────────────┤
//! │ TurnScratch (ephemeral)                  │ ← per-turn composition staging,
//! │   cleared at every turn boundary         │   committed to the log pre-request
//! └─────────────────────────────────────────┘
//! ```
//!
//! ## Status (Phase 2 — wired into the engine request path)
//!
//! All six types are load-bearing:
//!
//! * [`AppendLog`] is the transcript store on `Session` — `push` is the only
//!   everyday mutation the type expresses; every wholesale replacement must
//!   name itself through [`AppendLog::rebuild`] with a [`RebuildReason`].
//! * [`ThreeZoneRequest::into_message_request`] assembles the per-step
//!   `MessageRequest` in `engine/host_executor.rs`: `messages` can only be
//!   the log slice plus the scratch tail.
//! * [`PinnedPrefix`] / [`FrozenPrefix`] / [`PrefixDrift`] are the prefix
//!   identity currency: the engine freezes one per step and
//!   `prefix_cache::PrefixStabilityManager` drift-checks against it.
//! * [`TurnScratch`] is the engine's per-turn staging area — composed at
//!   turn start, committed to the log *before* the request loop runs, and
//!   cleared at every turn boundary. In production the request-time scratch
//!   is therefore empty (the request tail is the log tail); the field is the
//!   type-level slot for embedders that keep per-request-only content out
//!   of the log. Honest limitation, stated rather than papered over.
//!
//! ## The escape-hatch policy
//!
//! "Append-only" has sanctioned exceptions: compaction, overflow recovery,
//! `/edit` rollback, session restore, cycle reseeds. They all funnel through
//! [`AppendLog::rebuild`], which requires a [`RebuildReason`] naming the
//! caller and records a [`RebuildRecord`] for diagnostics (`/cache zones`).
//! Every one of those sites busts the KV prefix cache *by design*; the type
//! system's job is to make that impossible to do by accident.

use std::fmt;
use std::sync::{Arc, Mutex as StdMutex};

use crate::models::{Message, MessageRequest, SystemPrompt, Tool};
use crate::utils::sha256_hex;

// ── helpers ────────────────────────────────────────────────────────────

fn system_text(system: Option<&SystemPrompt>) -> String {
    match system {
        Some(SystemPrompt::Text(text)) => text.clone(),
        Some(SystemPrompt::Blocks(blocks)) => {
            let mut text = String::new();
            for block in blocks {
                text.push_str(&block.text);
                text.push('\n');
            }
            text
        }
        None => String::new(),
    }
}

/// Serialize tools to a deterministic, sorted JSON string for hashing.
///
/// Full definitions, not just names: a tool whose description or schema
/// changed re-serializes to different bytes and must be detected as prefix
/// drift even though its name (and catalog position) did not change.
fn tool_catalog_digest(tools: &[Tool]) -> String {
    let mut serialized: Vec<String> = tools
        .iter()
        .filter_map(|t| serde_json::to_string(t).ok())
        .collect();
    serialized.sort();
    serialized.join("\n")
}

fn combined_hash(system_text: &str, tools: &[Tool]) -> String {
    let system_sha = sha256_hex(system_text.as_bytes());
    let tools_digest = tool_catalog_digest(tools);
    let tools_sha = sha256_hex(tools_digest.as_bytes());
    let combined = format!("{system_sha}:{tools_sha}");
    sha256_hex(combined.as_bytes())
}

// ── FrozenPrefix ───────────────────────────────────────────────────────

/// An immutable frozen prefix — system prompt text + tool catalog,
/// hashed at freeze time. The hash is stable as long as the system prompt
/// text and full tool definitions (name, description, schema) are unchanged.
///
/// Use [`PinnedPrefix::freeze`] to produce one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenPrefix {
    pub system_text: String,
    pub tool_catalog: String,
    pub combined_sha256: String,
}

impl FrozenPrefix {
    /// Verify that `current_system_text` and `current_tools` match the frozen
    /// prefix. Returns `Ok(())` when stable, `Err(PrefixDrift)` on mismatch.
    ///
    /// Fast path: compares raw text before falling back to SHA-256.
    pub fn verify(
        &self,
        current_system_text: &str,
        current_tools: &[Tool],
    ) -> Result<(), PrefixDrift> {
        let system_changed = current_system_text != self.system_text;
        let current_tool_catalog = tool_catalog_digest(current_tools);
        let tools_changed = current_tool_catalog != self.tool_catalog;

        if !system_changed && !tools_changed {
            return Ok(());
        }

        let current_hash = combined_hash(current_system_text, current_tools);
        Err(PrefixDrift {
            system_changed,
            tools_changed,
            frozen_hash: self.combined_sha256.clone(),
            current_hash,
        })
    }

    /// Returns a short (12-char) human-readable id for display.
    #[must_use]
    pub fn short_id(&self) -> &str {
        if self.combined_sha256.len() >= 12 {
            &self.combined_sha256[..12]
        } else {
            &self.combined_sha256
        }
    }

    /// Returns the full combined SHA-256.
    #[must_use]
    pub fn hash(&self) -> &str {
        &self.combined_sha256
    }
}

// ── PinnedPrefix ───────────────────────────────────────────────────────

/// A mutable prefix builder. Construct from the system prompt and tool
/// catalog, then call [`freeze`](Self::freeze) to produce a [`FrozenPrefix`].
#[derive(Debug, Clone)]
pub struct PinnedPrefix {
    system_text: String,
    tools: Vec<Tool>,
}

impl PinnedPrefix {
    #[must_use]
    pub fn new(system: Option<&SystemPrompt>, tools: Vec<Tool>) -> Self {
        Self {
            system_text: system_text(system),
            tools,
        }
    }

    /// Freeze this prefix into an immutable [`FrozenPrefix`].
    #[must_use]
    pub fn freeze(&self) -> FrozenPrefix {
        let tool_catalog = tool_catalog_digest(&self.tools);
        let combined_sha256 = combined_hash(&self.system_text, &self.tools);

        FrozenPrefix {
            system_text: self.system_text.clone(),
            tool_catalog,
            combined_sha256,
        }
    }
}

// ── PrefixDrift ────────────────────────────────────────────────────────

/// Describes how the current prefix differs from the frozen baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixDrift {
    pub system_changed: bool,
    pub tools_changed: bool,
    pub frozen_hash: String,
    pub current_hash: String,
}

impl fmt::Display for PrefixDrift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cause = match (self.system_changed, self.tools_changed) {
            (true, true) => "system prompt and tool set",
            (true, false) => "system prompt",
            (false, true) => "tool set",
            (false, false) => "unknown component",
        };
        write!(
            f,
            "prefix drift: {cause} changed (frozen={}, current={})",
            &self.frozen_hash[..12.min(self.frozen_hash.len())],
            &self.current_hash[..12.min(self.current_hash.len())]
        )
    }
}

// ── RebuildReason / RebuildRecord ──────────────────────────────────────

/// Why an [`AppendLog`] was wholesale replaced. Every variant is a
/// sanctioned KV-prefix-cache bust — the enum exists so each bust names
/// itself at the type level and lands in the audit record instead of
/// hiding inside an arbitrary `Vec` assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildReason {
    /// `/compact` — user-initiated LLM-summary compaction.
    ManualCompaction,
    /// Background LLM-summary compaction (capacity gate / auto compact).
    AutoCompaction,
    /// `/purge` context purge.
    Purge,
    /// In-place byte-level micro-compaction (no LLM call).
    MicroCompaction,
    /// Emergency recovery from a provider context-length rejection.
    ContextOverflowRecovery,
    /// Oldest-message front trim to meet an input budget.
    FrontTrim,
    /// VerifyAndReplan turn reset (keep latest user + verified only).
    VerifyAndReplanReset,
    /// `/edit` rollback of the last user exchange.
    EditRollback,
    /// Session sync/restore (`Op::SyncSession`, REPL session load).
    SessionSync,
    /// Cycle-boundary transcript reseed (`build_seed_messages`).
    CycleReset,
    /// Trait-level replacement through `ChatHistory::replace_all` inside
    /// `executor.run` (compaction/recovery paths that only hold
    /// `&mut dyn ChatHistory`); the static str names the operation.
    Runtime(&'static str),
    /// `ChatHistory::clear` on the session bridge (empty replacement).
    TraitClear,
}

impl fmt::Display for RebuildReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            RebuildReason::ManualCompaction => "manual compaction (/compact)",
            RebuildReason::AutoCompaction => "auto compaction",
            RebuildReason::Purge => "context purge (/purge)",
            RebuildReason::MicroCompaction => "micro-compaction",
            RebuildReason::ContextOverflowRecovery => "context-overflow recovery",
            RebuildReason::FrontTrim => "front trim to budget",
            RebuildReason::VerifyAndReplanReset => "verify-and-replan reset",
            RebuildReason::EditRollback => "edit rollback (/edit)",
            RebuildReason::SessionSync => "session sync/restore",
            RebuildReason::CycleReset => "cycle reset reseed",
            RebuildReason::Runtime(op) => return write!(f, "runtime: {op}"),
            RebuildReason::TraitClear => "history clear",
        };
        f.write_str(name)
    }
}

/// Audit entry for one [`AppendLog::rebuild`]: what asked for the swap and
/// the transcript size on both sides of it. Surfaced by `/cache zones`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebuildRecord {
    pub reason: RebuildReason,
    pub before: usize,
    pub after: usize,
}

// ── AppendLog ──────────────────────────────────────────────────────────

/// Append-only conversation history — the `Session` transcript store.
///
/// Recording-layer role (one of four layers; each owning module states its
/// role): while the session runs this log is the **authoritative record** —
/// every transcript projection folds from it (see
/// `crate::projections`), and every durable layer (the session snapshot
/// file, the JSONL event stream, the SQLite query index) is downstream of
/// it. Nothing downstream holds authority over it.
///
/// `push` is the only everyday mutation this type expresses. Wholesale
/// replacement exists solely through [`rebuild`](Self::rebuild), which
/// demands a [`RebuildReason`]. There is deliberately no way to insert,
/// remove, truncate, or index-write: the append-only prefix that DeepSeek's
/// KV cache matches against is a property of the type, not of discipline.
///
/// Reads go through `Deref<Target = [Message]>` (shared only — no
/// `DerefMut`, which would re-expose slice in-place edits like `swap` or
/// `sort`), so `&log`, `log.len()`, `log.iter()`, `log[i]`, and
/// `log.to_vec()` all keep working while every mutating method of `Vec`
/// fails to compile.
#[derive(Debug, Default)]
pub struct AppendLog {
    messages: Vec<Message>,
    last_rebuild: Option<RebuildRecord>,
    /// Shared snapshot handed to `Event::SessionUpdated` and the per-turn
    /// tool-context build. Interior-mutable so `snapshot(&self)` works from
    /// shared-ref call sites: `push` extends it in place while unsold
    /// (`Arc::strong_count == 1`), so per-message emission is amortized
    /// O(1); once a consumer still holds the previous snapshot the cache is
    /// dropped and the next [`snapshot`](Self::snapshot) rebuilds once —
    /// never more than the pre-snapshot `to_vec()` per emission.
    snapshot: StdMutex<Option<Arc<Vec<Message>>>>,
}

/// Manual clone: the snapshot cache is dropped — a cloned log starts
/// uncached (the cache is a sharing optimization, not state).
impl Clone for AppendLog {
    fn clone(&self) -> Self {
        Self {
            messages: self.messages.clone(),
            last_rebuild: self.last_rebuild,
            snapshot: StdMutex::new(None),
        }
    }
}

impl AppendLog {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            last_rebuild: None,
            snapshot: StdMutex::new(None),
        }
    }

    /// Wrap an existing message list (session restore, test fixtures).
    #[must_use]
    pub fn from_messages(messages: Vec<Message>) -> Self {
        Self {
            messages,
            last_rebuild: None,
            snapshot: StdMutex::new(None),
        }
    }

    /// The everyday mutation: append one message to the tail. Every
    /// production growth path (user turns, assistant turns, tool results,
    /// steers, seams, re-injections) funnels through here.
    pub fn push(&mut self, message: Message) {
        {
            let mut guard = self
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match guard.as_mut() {
                Some(snap) if Arc::strong_count(snap) == 1 => {
                    Arc::make_mut(snap).push(message.clone());
                }
                _ => *guard = None,
            }
        }
        self.messages.push(message);
    }

    /// The ONLY sanctioned wholesale replacement. Busts the KV prefix cache
    /// by design — the `reason` names the caller and lands in the audit
    /// record ([`last_rebuild`](Self::last_rebuild)) for `/cache zones`.
    pub fn rebuild(&mut self, reason: RebuildReason, messages: Vec<Message>) {
        let before = self.messages.len();
        self.messages = messages;
        self.last_rebuild = Some(RebuildRecord {
            reason,
            before,
            after: self.messages.len(),
        });
        *self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Shared copy-on-write view of the whole log, for full-transcript
    /// event emission and per-turn context builds. Cheaper than `to_vec()`
    /// when the previous snapshot has been released (cached and extended in
    /// place by `push`); costs one `to_vec()`-equivalent rebuild otherwise.
    /// Known limitation: while a consumer holds an earlier snapshot, every
    /// emission pays that one full rebuild — removing it entirely would
    /// take a delta event or a persistent structure, both larger contract
    /// changes.
    pub fn snapshot(&self) -> Arc<Vec<Message>> {
        let mut guard = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(snap) = guard.as_ref() {
            return Arc::clone(snap);
        }
        let snap = Arc::new(self.messages.clone());
        *guard = Some(Arc::clone(&snap));
        snap
    }

    /// The most recent rebuild's audit entry, if the log was ever rebuilt.
    #[must_use]
    pub fn last_rebuild(&self) -> Option<RebuildRecord> {
        self.last_rebuild
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Message> {
        self.messages.iter()
    }

    #[must_use]
    pub fn as_slice(&self) -> &[Message] {
        &self.messages
    }
}

impl std::ops::Deref for AppendLog {
    type Target = [Message];

    fn deref(&self) -> &[Message] {
        &self.messages
    }
}

// ── TurnScratch ────────────────────────────────────────────────────────

/// Per-turn ephemeral composition state. Populated at turn start (the
/// working-set paths that feed `<turn_meta>` and the user message being
/// composed), committed to the [`AppendLog`] before the request loop runs,
/// and cleared at every turn boundary.
#[derive(Debug, Clone, Default)]
pub struct TurnScratch {
    pub working_set: Vec<String>,
    pub user_message: Option<Message>,
}

impl TurnScratch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.working_set.clear();
        self.user_message = None;
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.working_set.is_empty() && self.user_message.is_none()
    }
}

// ── ThreeZoneRequest ───────────────────────────────────────────────────

/// A composed three-zone request: the frozen prefix identity, the
/// append-log slice, and the per-turn scratch, plus the sampling
/// parameters. Convert with [`into_message_request`](Self::into_message_request)
/// — the engine's per-step `MessageRequest` is produced here and nowhere
/// else.
///
/// The system prompt travels in the `MessageRequest.system` field (the
/// provider shaper maps it to a preamble); it is NOT inlined as a
/// `role:"system"` entry inside `messages`.
#[derive(Debug, Clone)]
pub struct ThreeZoneRequest<'a> {
    pub prefix: &'a FrozenPrefix,
    /// Append-log view. In production this is the `Session` transcript
    /// borrowed through `ChatHistory::messages`; the [`AppendLog`] type
    /// governs the store, this slice is the request-side view of it.
    pub log: &'a [Message],
    pub scratch: TurnScratch,
    pub model: String,
    pub max_tokens: u32,
    pub system: Option<SystemPrompt>,
    pub tools: Option<Vec<Tool>>,
    pub tool_choice: Option<serde_json::Value>,
    pub reasoning_effort: Option<String>,
    pub thinking: Option<serde_json::Value>,
    pub stream: Option<bool>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub metadata: Option<serde_json::Value>,
}

impl<'a> ThreeZoneRequest<'a> {
    /// The `messages` field of the wire request: append-log slice with the
    /// scratch user message (when present) cloned at the tail. Deterministic
    /// — identical zone inputs always serialize to identical bytes, which is
    /// what makes transparent retries cache-safe.
    #[must_use]
    pub fn wire_messages(&self) -> Vec<Message> {
        let mut messages =
            Vec::with_capacity(self.log.len() + usize::from(self.scratch.user_message.is_some()));
        messages.extend_from_slice(self.log);
        if let Some(ref user_msg) = self.scratch.user_message {
            messages.push(user_msg.clone());
        }
        messages
    }

    #[must_use]
    pub fn message_count(&self) -> usize {
        self.log.len() + usize::from(self.scratch.user_message.is_some())
    }

    /// Compose into the wire [`MessageRequest`] the provider client sends.
    /// This is the single production assembly point — hand-building a
    /// `MessageRequest.messages` from an arbitrary `Vec` elsewhere is the
    /// pattern the three-zone contract exists to prevent.
    #[must_use]
    pub fn into_message_request(self) -> MessageRequest {
        let messages = self.wire_messages();
        MessageRequest {
            model: self.model,
            messages,
            max_tokens: self.max_tokens,
            system: self.system,
            tools: self.tools,
            tool_choice: self.tool_choice,
            metadata: self.metadata,
            thinking: self.thinking,
            reasoning_effort: self.reasoning_effort,
            stream: self.stream,
            temperature: self.temperature,
            top_p: self.top_p,
        }
    }
}

// ── tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ContentBlock;

    fn text_message(role: &str, text: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    #[test]
    fn snapshot_tracks_pushes_while_unsold() {
        let mut log = AppendLog::new();
        log.push(text_message("user", "one"));
        let a = log.snapshot();
        let a2 = log.snapshot();
        assert!(Arc::ptr_eq(&a, &a2), "consecutive reads share the cache");
        drop(a);
        drop(a2);

        // Cache is the sole holder again: push extends in place and the
        // next snapshot observes the appended message.
        log.push(text_message("assistant", "two"));
        let b = log.snapshot();
        assert_eq!(b.len(), 2);
        assert_eq!(b[1].role, "assistant");
        drop(b);
        log.push(text_message("user", "three"));
        assert_eq!(log.snapshot().len(), 3);
    }

    #[test]
    fn snapshot_sold_copy_is_frozen_and_cache_rebuilds() {
        let mut log = AppendLog::from_messages(vec![text_message("user", "one")]);
        let sold = log.snapshot();

        log.push(text_message("assistant", "two"));
        // The sold snapshot must not observe the new message.
        assert_eq!(sold.len(), 1);
        let fresh = log.snapshot();
        assert_eq!(fresh.len(), 2);
        assert!(!Arc::ptr_eq(&sold, &fresh));
    }

    #[test]
    fn rebuild_invalidates_the_snapshot() {
        let mut log = AppendLog::new();
        log.push(text_message("user", "one"));
        let stale = log.snapshot();
        log.rebuild(
            RebuildReason::FrontTrim,
            vec![text_message("assistant", "kept")],
        );
        assert_eq!(stale.len(), 1);
        let fresh = log.snapshot();
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].role, "assistant");
    }

    fn make_tool(name: &str) -> Tool {
        Tool {
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::Value::Null,
            output_schema: None,
            tool_type: None,
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }

    fn make_message(role: &str, text: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    // ── FrozenPrefix / PinnedPrefix ────────────────────────────────

    #[test]
    fn freeze_produces_stable_hash() {
        let tools = vec![make_tool("read"), make_tool("write")];
        let sys = SystemPrompt::Text("hello world".to_string());

        let a = PinnedPrefix::new(Some(&sys), tools.clone()).freeze();
        let b = PinnedPrefix::new(Some(&sys), tools).freeze();

        assert_eq!(a.combined_sha256, b.combined_sha256);
        assert_eq!(a.hash(), b.hash());
        assert_eq!(a.short_id(), b.short_id());
    }

    #[test]
    fn freeze_tool_order_is_stable() {
        let sys = SystemPrompt::Text("system".to_string());
        let tools_a = vec![make_tool("b"), make_tool("a")];
        let tools_b = vec![make_tool("a"), make_tool("b")];

        let a = PinnedPrefix::new(Some(&sys), tools_a).freeze();
        let b = PinnedPrefix::new(Some(&sys), tools_b).freeze();

        assert_eq!(a.combined_sha256, b.combined_sha256);
    }

    #[test]
    fn freeze_empty_tools() {
        let sys = SystemPrompt::Text("system".to_string());
        let frozen = PinnedPrefix::new(Some(&sys), vec![]).freeze();
        assert!(frozen.tool_catalog.is_empty());
        assert!(!frozen.combined_sha256.is_empty());
        assert_eq!(frozen.short_id().len(), 12);
    }

    #[test]
    fn freeze_no_system() {
        let tools = vec![make_tool("t1")];
        let frozen = PinnedPrefix::new(None, tools).freeze();
        assert!(frozen.system_text.is_empty());
        assert!(frozen.tool_catalog.contains("t1"));
    }

    #[test]
    fn verify_passes_when_stable() {
        let sys = SystemPrompt::Text("system".to_string());
        let tools = vec![make_tool("a")];
        let frozen = PinnedPrefix::new(Some(&sys), tools.clone()).freeze();

        assert!(frozen.verify("system", &tools).is_ok());
    }

    #[test]
    fn verify_detects_system_change() {
        let sys = SystemPrompt::Text("old".to_string());
        let tools = vec![make_tool("a")];
        let frozen = PinnedPrefix::new(Some(&sys), tools.clone()).freeze();

        let drift = frozen.verify("new", &tools).unwrap_err();
        assert!(drift.system_changed);
        assert!(!drift.tools_changed);
    }

    #[test]
    fn verify_detects_tool_change() {
        let sys = SystemPrompt::Text("system".to_string());
        let tools_a = vec![make_tool("a")];
        let frozen = PinnedPrefix::new(Some(&sys), tools_a).freeze();

        let tools_b = vec![make_tool("b")];
        let drift = frozen.verify("system", &tools_b).unwrap_err();
        assert!(!drift.system_changed);
        assert!(drift.tools_changed);
    }

    #[test]
    fn verify_detects_both_changes() {
        let sys = SystemPrompt::Text("old".to_string());
        let tools = vec![make_tool("a")];
        let frozen = PinnedPrefix::new(Some(&sys), tools).freeze();

        let drift = frozen.verify("new", &[make_tool("b")]).unwrap_err();
        assert!(drift.system_changed);
        assert!(drift.tools_changed);
    }

    #[test]
    fn verify_detects_schema_change() {
        let sys = SystemPrompt::Text("system".to_string());
        let tool_a = make_tool("a");
        let mut tool_a_v2 = make_tool("a");
        tool_a_v2.description = "updated desc".to_string();

        let frozen = PinnedPrefix::new(Some(&sys), vec![tool_a]).freeze();
        let drift = frozen.verify("system", &[tool_a_v2]).unwrap_err();
        // Same name, different schema — should detect the change.
        assert!(drift.tools_changed);
    }

    #[test]
    fn prefix_drift_display_is_readable() {
        let drift = PrefixDrift {
            system_changed: true,
            tools_changed: false,
            frozen_hash: "a".repeat(64),
            current_hash: "b".repeat(64),
        };
        let display = drift.to_string();
        assert!(display.contains("system prompt"));
        assert!(display.contains("aaaaaaaaaaaa"));
        assert!(display.contains("bbbbbbbbbbbb"));
    }

    // ── AppendLog ─────────────────────────────────────────────────

    #[test]
    fn append_log_push_and_iter() {
        let mut log = AppendLog::new();
        assert!(log.is_empty());

        log.push(make_message("user", "hello"));
        log.push(make_message("assistant", "hi"));

        assert_eq!(log.len(), 2);
        assert!(!log.is_empty());

        let messages: Vec<_> = log.iter().collect();
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn append_log_from_messages() {
        let msgs = vec![make_message("user", "a"), make_message("assistant", "b")];
        let log = AppendLog::from_messages(msgs);
        assert_eq!(log.len(), 2);
        assert_eq!(log.as_slice().len(), 2);
        assert!(log.last_rebuild().is_none());
    }

    #[test]
    fn append_log_deref_exposes_slice_reads() {
        let mut log = AppendLog::new();
        log.push(make_message("user", "hello"));
        log.push(make_message("assistant", "hi"));

        // Shared Deref keeps slice-style reads working.
        let slice: &[Message] = &log;
        assert_eq!(slice.len(), 2);
        assert_eq!(log[0].role, "user");
        assert_eq!(log.to_vec().len(), 2);
        assert_eq!(log.iter().next().map(|m| m.role.as_str()), Some("user"));
    }

    #[test]
    fn append_log_rebuild_records_audit() {
        let mut log = AppendLog::from_messages(vec![
            make_message("user", "a"),
            make_message("assistant", "b"),
            make_message("user", "c"),
        ]);
        assert!(log.last_rebuild().is_none());

        log.rebuild(
            RebuildReason::ManualCompaction,
            vec![make_message("user", "summary")],
        );

        let record = log.last_rebuild().expect("rebuild recorded");
        assert_eq!(record.reason, RebuildReason::ManualCompaction);
        assert_eq!(record.before, 3);
        assert_eq!(record.after, 1);
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn rebuild_reason_display_is_readable() {
        assert!(
            RebuildReason::ManualCompaction
                .to_string()
                .contains("/compact")
        );
        assert_eq!(
            RebuildReason::Runtime("micro-compact").to_string(),
            "runtime: micro-compact"
        );
        assert!(RebuildReason::EditRollback.to_string().contains("/edit"));
    }

    // ── TurnScratch ───────────────────────────────────────────────

    #[test]
    fn scratch_clear_empties_all_fields() {
        let mut scratch = TurnScratch::new();
        scratch.working_set.push("file.rs".to_string());
        scratch.user_message = Some(make_message("user", "task"));

        assert!(!scratch.is_empty());
        scratch.clear();
        assert!(scratch.is_empty());
        assert!(scratch.working_set.is_empty());
        assert!(scratch.user_message.is_none());
    }

    // ── ThreeZoneRequest ──────────────────────────────────────────

    #[test]
    fn wire_messages_concatenates_log_and_scratch_tail() {
        let sys = SystemPrompt::Text("you are helpful".to_string());
        let tools = vec![make_tool("read")];
        let prefix = PinnedPrefix::new(Some(&sys), tools).freeze();

        let mut log = AppendLog::new();
        log.push(make_message("user", "prev question"));
        log.push(make_message("assistant", "prev answer"));

        let scratch = TurnScratch {
            working_set: vec!["main.rs".to_string()],
            user_message: Some(make_message("user", "current task")),
        };

        let request = ThreeZoneRequest {
            prefix: &prefix,
            log: log.as_slice(),
            scratch,
            model: "deepseek-v4-pro".to_string(),
            max_tokens: 4096,
            system: Some(sys),
            tools: None,
            tool_choice: None,
            reasoning_effort: None,
            thinking: None,
            stream: None,
            temperature: None,
            top_p: None,
            metadata: None,
        };

        let messages = request.wire_messages();
        // System is NOT inlined — it travels in MessageRequest.system.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].role, "user");
        assert_eq!(request.message_count(), 3);
    }

    #[test]
    fn wire_messages_log_only() {
        let prefix = PinnedPrefix::new(None, vec![]).freeze();

        let mut log = AppendLog::new();
        log.push(make_message("user", "hi"));

        let request = ThreeZoneRequest {
            prefix: &prefix,
            log: log.as_slice(),
            scratch: TurnScratch::new(),
            model: "x".to_string(),
            max_tokens: 1,
            system: None,
            tools: None,
            tool_choice: None,
            reasoning_effort: None,
            thinking: None,
            stream: None,
            temperature: None,
            top_p: None,
            metadata: None,
        };

        let messages = request.wire_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(request.message_count(), 1);
    }

    #[test]
    fn into_message_request_matches_hand_assembly() {
        use crate::models::{CacheControl, SystemBlock};

        let blocks = SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "hello".to_string(),
            cache_control: Some(CacheControl {
                cache_type: "ephemeral".to_string(),
            }),
        }]);
        let tools = vec![make_tool("read"), make_tool("write")];
        let prefix = PinnedPrefix::new(Some(&blocks), tools.clone()).freeze();

        let log_msgs = vec![make_message("user", "q"), make_message("assistant", "a")];
        let scratch = TurnScratch {
            working_set: vec![],
            user_message: Some(make_message("user", "next")),
        };

        let request = ThreeZoneRequest {
            prefix: &prefix,
            log: &log_msgs,
            scratch,
            model: "deepseek-chat".to_string(),
            max_tokens: 8192,
            system: Some(blocks.clone()),
            tools: Some(tools.clone()),
            tool_choice: None,
            reasoning_effort: Some("low".to_string()),
            thinking: None,
            stream: Some(true),
            temperature: Some(0.0),
            top_p: None,
            metadata: None,
        }
        .into_message_request();

        // Byte-identical to the legacy direct MessageRequest assembly.
        let legacy = MessageRequest {
            model: "deepseek-chat".to_string(),
            messages: {
                let mut m = log_msgs.clone();
                m.push(make_message("user", "next"));
                m
            },
            max_tokens: 8192,
            system: Some(blocks),
            tools: Some(tools),
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("low".to_string()),
            stream: Some(true),
            temperature: Some(0.0),
            top_p: None,
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            serde_json::to_string(&legacy).unwrap()
        );
        // Blocks system prompt (incl. cache_control) passes through as-is.
        assert_eq!(
            request.system,
            Some(SystemPrompt::Blocks(vec![SystemBlock {
                block_type: "text".to_string(),
                text: "hello".to_string(),
                cache_control: Some(CacheControl {
                    cache_type: "ephemeral".to_string(),
                }),
            }]))
        );
    }
}
