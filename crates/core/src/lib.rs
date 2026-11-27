use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use codesmith_agent::ModelRegistry;
use codesmith_config::{CliRuntimeOverrides, ConfigToml, ProviderKind};
use codesmith_execpolicy::{
    AskForApproval, ExecApprovalRequirement, ExecPolicyContext, ExecPolicyDecision,
    ExecPolicyEngine,
};
use codesmith_hooks::{HookDispatcher, HookEvent};
use codesmith_mcp::{
    McpManager, McpStartupCompleteEvent, McpStartupStatus as McpManagerStartupStatus,
};
use codesmith_protocol::{
    AppResponse, EventFrame, ExecApprovalRequestEvent, PromptRequest, PromptResponse,
    ResponseChannel, ReviewDecision, Thread, ThreadForkParams, ThreadListParams, ThreadReadParams,
    ThreadRequest, ThreadResponse, ThreadResumeParams, ThreadSetNameParams, ThreadStatus,
    ToolPayload,
};
use codesmith_state::{
    JobStateRecord, JobStateStatus, SessionSource, StateStore, ThreadListFilters, ThreadMetadata,
    ThreadStatus as PersistedThreadStatus,
};
use codesmith_tools::ToolCall;
use serde_json::{Value, json};
use uuid::Uuid;

/// How a new thread's conversation history is initialized.
#[derive(Debug, Clone)]
pub enum InitialHistory {
    /// Start with an empty conversation.
    New,
    /// Forked from an existing thread with the given history items.
    Forked(Vec<Value>),
    /// Resumed from a persisted thread with its full history.
    Resumed {
        conversation_id: String,
        history: Vec<Value>,
        rollout_path: PathBuf,
    },
}

/// Result of spawning or resuming a thread.
#[derive(Debug, Clone)]
pub struct NewThread {
    /// The thread metadata.
    pub thread: Thread,
    /// Resolved model identifier.
    pub model: String,
    /// Provider that serves the model.
    pub model_provider: String,
    /// Working directory for the thread.
    pub cwd: PathBuf,
    /// Approval policy override, if any.
    pub approval_policy: Option<String>,
    /// Sandbox mode override, if any.
    pub sandbox: Option<String>,
}

/// Status of a background job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// Waiting to be picked up.
    Queued,
    /// Currently executing.
    Running,
    /// Temporarily paused.
    Paused,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
    /// Cancelled by the user.
    Cancelled,
}

const JOB_DETAIL_SCHEMA_VERSION: u8 = 1;
const DEFAULT_JOB_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_JOB_BACKOFF_BASE_MS: u64 = 500;
const MAX_JOB_HISTORY_ENTRIES: usize = 64;

/// Retry state for a job that failed and may be retried.
#[derive(Debug, Clone)]
pub struct JobRetryMetadata {
    /// Current attempt number (0 = not yet retried).
    pub attempt: u32,
    /// Maximum number of retry attempts before giving up.
    pub max_attempts: u32,
    /// Base delay in milliseconds for exponential backoff.
    pub backoff_base_ms: u64,
    /// Computed delay in milliseconds until the next retry.
    pub next_backoff_ms: u64,
    /// Timestamp when the next retry should be attempted.
    pub next_retry_at: Option<i64>,
}

impl Default for JobRetryMetadata {
    fn default() -> Self {
        Self {
            attempt: 0,
            max_attempts: DEFAULT_JOB_MAX_ATTEMPTS,
            backoff_base_ms: DEFAULT_JOB_BACKOFF_BASE_MS,
            next_backoff_ms: 0,
            next_retry_at: None,
        }
    }
}

/// A single entry in a job's history log.
#[derive(Debug, Clone)]
pub struct JobHistoryEntry {
    /// Timestamp when this entry was recorded.
    pub at: i64,
    /// Phase name (e.g., "created", "running", "failed").
    pub phase: String,
    /// Job status at this point in time.
    pub status: JobStatus,
    /// Progress percentage at this point, if available.
    pub progress: Option<u8>,
    /// Human-readable detail message.
    pub detail: Option<String>,
    /// Retry state snapshot at this point.
    pub retry: JobRetryMetadata,
}

#[derive(Debug, Clone)]
struct PersistedJobDetail {
    pub status: JobStatus,
    pub detail: Option<String>,
    pub retry: JobRetryMetadata,
    pub history: Vec<JobHistoryEntry>,
}

/// A complete job record with all metadata and history.
#[derive(Debug, Clone)]
pub struct JobRecord {
    /// Unique job identifier.
    pub id: String,
    /// Human-readable job name.
    pub name: String,
    /// Current job status.
    pub status: JobStatus,
    /// Current progress percentage (0-100).
    pub progress: Option<u8>,
    /// Human-readable detail about the current state.
    pub detail: Option<String>,
    /// Retry state for failed jobs.
    pub retry: JobRetryMetadata,
    /// Chronological history of state transitions.
    pub history: Vec<JobHistoryEntry>,
    /// Timestamp when the job was created.
    pub created_at: i64,
    /// Timestamp of the last state change.
    pub updated_at: i64,
}

/// Manages background jobs with retry logic and persistence.
#[derive(Debug, Default)]
pub struct JobManager {
    jobs: HashMap<String, JobRecord>,
}

impl JobManager {
    fn now_ts() -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn deterministic_backoff_ms(retry: &JobRetryMetadata) -> u64 {
        if retry.attempt == 0 {
            return 0;
        }
        let exponent = retry.attempt.saturating_sub(1).min(20);
        let multiplier = 1u64.checked_shl(exponent).unwrap_or(u64::MAX);
        retry.backoff_base_ms.saturating_mul(multiplier)
    }

    fn clear_retry_schedule(retry: &mut JobRetryMetadata) {
        retry.next_backoff_ms = 0;
        retry.next_retry_at = None;
    }

    fn push_history(job: &mut JobRecord, phase: &str) {
        job.history.push(JobHistoryEntry {
            at: job.updated_at,
            phase: phase.to_string(),
            status: job.status,
            progress: job.progress,
            detail: job.detail.clone(),
            retry: job.retry.clone(),
        });
        if job.history.len() > MAX_JOB_HISTORY_ENTRIES {
            let to_drain = job.history.len() - MAX_JOB_HISTORY_ENTRIES;
            job.history.drain(0..to_drain);
        }
    }

    fn parse_persisted_detail(raw: Option<&str>) -> Option<PersistedJobDetail> {
        let raw = raw?;
        let parsed: Value = serde_json::from_str(raw).ok()?;
        let status = parsed
            .get("status")
            .and_then(Value::as_str)
            .and_then(job_status_from_str)?;
        let detail = parsed.get("detail").and_then(json_optional_string);
        let retry = parse_retry_metadata(parsed.get("retry"));
        let history = parsed
            .get("history")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(parse_history_entry)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Some(PersistedJobDetail {
            status,
            detail,
            retry,
            history,
        })
    }

    fn encode_persisted_detail(job: &JobRecord) -> Result<Option<String>> {
        let encoded = json!({
            "schema_version": JOB_DETAIL_SCHEMA_VERSION,
            "status": job_status_to_str(job.status),
            "detail": job.detail.clone(),
            "retry": job_retry_to_value(&job.retry),
            "history": job.history.iter().map(job_history_to_value).collect::<Vec<_>>()
        })
        .to_string();
        Ok(Some(encoded))
    }

    /// Enqueues a new job and returns its record.
    pub fn enqueue(&mut self, name: impl Into<String>) -> JobRecord {
        let now = Self::now_ts();
        let id = format!("job-{}", Uuid::new_v4());
        let mut job = JobRecord {
            id: id.clone(),
            name: name.into(),
            status: JobStatus::Queued,
            progress: Some(0),
            detail: None,
            retry: JobRetryMetadata::default(),
            history: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        Self::push_history(&mut job, "created");
        self.jobs.insert(id, job.clone());
        job
    }

    /// Transitions a job to running and clears its retry schedule.
    pub fn set_running(&mut self, id: &str) {
        if let Some(job) = self.jobs.get_mut(id) {
            job.status = JobStatus::Running;
            Self::clear_retry_schedule(&mut job.retry);
            job.updated_at = Self::now_ts();
            Self::push_history(job, "running");
        }
    }

    /// Updates a job's progress (clamped to 100) and optional detail message.
    pub fn update_progress(&mut self, id: &str, progress: u8, detail: Option<String>) {
        if let Some(job) = self.jobs.get_mut(id) {
            job.progress = Some(progress.min(100));
            job.detail = detail;
            job.updated_at = Self::now_ts();
            Self::push_history(job, "progress_updated");
        }
    }

    /// Marks a job as completed with 100% progress and clears its retry schedule.
    pub fn complete(&mut self, id: &str) {
        if let Some(job) = self.jobs.get_mut(id) {
            job.status = JobStatus::Completed;
            job.progress = Some(100);
            Self::clear_retry_schedule(&mut job.retry);
            job.updated_at = Self::now_ts();
            Self::push_history(job, "completed");
        }
    }

    /// Marks a job as failed and schedules a retry if attempts remain.
    pub fn fail(&mut self, id: &str, detail: impl Into<String>) {
        if let Some(job) = self.jobs.get_mut(id) {
            let now = Self::now_ts();
            job.status = JobStatus::Failed;
            job.detail = Some(detail.into());
            if job.retry.attempt < job.retry.max_attempts {
                job.retry.attempt += 1;
                job.retry.next_backoff_ms = Self::deterministic_backoff_ms(&job.retry);
                let delay_secs = ((job.retry.next_backoff_ms.saturating_add(999)) / 1000)
                    .min(i64::MAX as u64) as i64;
                job.retry.next_retry_at = Some(now.saturating_add(delay_secs));
            } else {
                Self::clear_retry_schedule(&mut job.retry);
            }
            job.updated_at = now;
            Self::push_history(job, "failed");
        }
    }

    /// Cancels a job and clears any pending retry schedule.
    pub fn cancel(&mut self, id: &str) {
        if let Some(job) = self.jobs.get_mut(id) {
            job.status = JobStatus::Cancelled;
            Self::clear_retry_schedule(&mut job.retry);
            job.updated_at = Self::now_ts();
            Self::push_history(job, "cancelled");
        }
    }

    /// Pauses a job, optionally updating its detail message.
    pub fn pause(&mut self, id: &str, detail: Option<String>) {
        if let Some(job) = self.jobs.get_mut(id) {
            job.status = JobStatus::Paused;
            if detail.is_some() {
                job.detail = detail;
            }
            job.updated_at = Self::now_ts();
            Self::push_history(job, "paused");
        }
    }

    /// Resumes a paused or failed job back to running status.
    pub fn resume(&mut self, id: &str, detail: Option<String>) {
        if let Some(job) = self.jobs.get_mut(id) {
            job.status = JobStatus::Running;
            if detail.is_some() {
                job.detail = detail;
            }
            Self::clear_retry_schedule(&mut job.retry);
            job.updated_at = Self::now_ts();
            Self::push_history(job, "resumed");
        }
    }

    /// Returns all jobs sorted by most recently updated first.
    pub fn list(&self) -> Vec<JobRecord> {
        let mut out = self.jobs.values().cloned().collect::<Vec<_>>();
        out.sort_by_key(|job| std::cmp::Reverse(job.updated_at));
        out
    }

    /// Returns the history entries for a job, or an empty vec if not found.
    pub fn history(&self, id: &str) -> Vec<JobHistoryEntry> {
        self.jobs
            .get(id)
            .map(|job| job.history.clone())
            .unwrap_or_default()
    }

    /// Resets queued or running jobs back to queued on application resume.
    pub fn resume_pending(&mut self) -> Vec<JobRecord> {
        let mut resumed = Vec::new();
        for job in self.jobs.values_mut() {
            if matches!(job.status, JobStatus::Queued | JobStatus::Running) {
                job.status = JobStatus::Queued;
                job.updated_at = Self::now_ts();
                Self::push_history(job, "queued_after_resume");
                resumed.push(job.clone());
            }
        }
        resumed
    }

    /// Loads jobs from the state store, deserializing extended detail when available.
    pub fn load_from_store(&mut self, store: &StateStore) -> Result<()> {
        let persisted = store.list_jobs(Some(500))?;
        for job in persisted {
            let fallback_status = job_state_status_to_runtime(job.status);
            let parsed = Self::parse_persisted_detail(job.detail.as_deref());
            let (status, detail, retry, history) = if let Some(detail_state) = parsed {
                (
                    detail_state.status,
                    detail_state.detail,
                    detail_state.retry,
                    detail_state.history,
                )
            } else {
                (
                    fallback_status,
                    job.detail,
                    JobRetryMetadata::default(),
                    Vec::new(),
                )
            };
            self.jobs.insert(
                job.id.clone(),
                JobRecord {
                    id: job.id,
                    name: job.name,
                    status,
                    progress: job.progress,
                    detail,
                    retry,
                    history,
                    created_at: job.created_at,
                    updated_at: job.updated_at,
                },
            );
        }
        Ok(())
    }

    /// Persists a single job's current state to the state store.
    pub fn persist_job(&self, store: &StateStore, id: &str) -> Result<()> {
        let Some(job) = self.jobs.get(id) else {
            return Ok(());
        };
        let encoded_detail = Self::encode_persisted_detail(job)?;
        store.upsert_job(&JobStateRecord {
            id: job.id.clone(),
            name: job.name.clone(),
            status: runtime_status_to_job_state(job.status),
            progress: job.progress,
            detail: encoded_detail,
            created_at: job.created_at,
            updated_at: job.updated_at,
        })
    }

    /// Persists all in-memory jobs to the state store.
    pub fn persist_all(&self, store: &StateStore) -> Result<()> {
        for id in self.jobs.keys() {
            self.persist_job(store, id)?;
        }
        Ok(())
    }
}

/// Manages thread lifecycle: spawn, resume, fork, archive, and persistence.
pub struct ThreadManager {
    store: StateStore,
    running_threads: HashMap<String, Thread>,
    cli_version: String,
}

impl ThreadManager {
    /// Creates a new `ThreadManager` backed by the given state store.
    pub fn new(store: StateStore) -> Self {
        Self {
            store,
            running_threads: HashMap::new(),
            cli_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Returns a reference to the underlying state store.
    pub fn state_store(&self) -> &StateStore {
        &self.store
    }

    /// Spawns a new thread with the given initial history and persists it.
    pub fn spawn_thread_with_history(
        &mut self,
        model_provider: String,
        cwd: PathBuf,
        initial_history: InitialHistory,
        persist_extended_history: bool,
    ) -> Result<NewThread> {
        let id = format!("thread-{}", Uuid::new_v4());
        let now = chrono::Utc::now().timestamp();
        let preview = preview_from_initial_history(&initial_history);
        let source = match initial_history {
            InitialHistory::New => SessionSource::Interactive,
            InitialHistory::Forked(_) => SessionSource::Fork,
            InitialHistory::Resumed { .. } => SessionSource::Resume,
        };
        let thread = Thread {
            id: id.clone(),
            preview,
            ephemeral: !persist_extended_history,
            model_provider: model_provider.clone(),
            created_at: now,
            updated_at: now,
            status: ThreadStatus::Running,
            path: None,
            cwd: cwd.clone(),
            cli_version: self.cli_version.clone(),
            source: match source {
                SessionSource::Interactive => codesmith_protocol::SessionSource::Interactive,
                SessionSource::Resume => codesmith_protocol::SessionSource::Resume,
                SessionSource::Fork => codesmith_protocol::SessionSource::Fork,
                SessionSource::Api => codesmith_protocol::SessionSource::Api,
                SessionSource::Unknown => codesmith_protocol::SessionSource::Unknown,
            },
            name: None,
        };
        self.persist_thread(&thread)?;
        let items: &[Value] = match &initial_history {
            InitialHistory::Forked(items) => items,
            InitialHistory::Resumed { history, .. } => history,
            InitialHistory::New => &[],
        };
        for item in items {
            if let Err(err) = self.store.append_message(
                &thread.id,
                "history",
                &item.to_string(),
                Some(item.clone()),
            ) {
                // The thread row is already committed; without cleanup a
                // mid-loop failure leaves a ghost thread with a partial
                // history. Best-effort rollback (cascades to messages).
                let _ = self.store.delete_thread(&thread.id);
                return Err(err);
            }
        }
        self.running_threads
            .insert(thread.id.clone(), thread.clone());
        Ok(NewThread {
            thread,
            model: "auto".to_string(),
            model_provider,
            cwd,
            approval_policy: None,
            sandbox: None,
        })
    }

    /// Resumes an existing thread, returning `None` if not found.
    pub fn resume_thread_with_history(
        &mut self,
        params: &ThreadResumeParams,
        fallback_cwd: &Path,
        model_provider: String,
    ) -> Result<Option<NewThread>> {
        if params.history.is_none()
            && let Some(thread) = self.running_threads.get(&params.thread_id).cloned()
            && thread.status == ThreadStatus::Running
        {
            return Ok(Some(NewThread {
                model: params.model.clone().unwrap_or_else(|| "auto".to_string()),
                model_provider: params.model_provider.clone().unwrap_or(model_provider),
                cwd: params.cwd.clone().unwrap_or_else(|| thread.cwd.clone()),
                approval_policy: params.approval_policy.clone(),
                sandbox: params.sandbox.clone(),
                thread,
            }));
        }

        let persisted = self.store.get_thread(&params.thread_id)?;
        let Some(metadata) = persisted else {
            return Ok(None);
        };
        let mut thread = to_protocol_thread(metadata);
        thread.status = ThreadStatus::Running;
        thread.updated_at = chrono::Utc::now().timestamp();
        thread.cwd = params
            .cwd
            .clone()
            .unwrap_or_else(|| fallback_cwd.to_path_buf());
        self.persist_thread(&thread)?;
        self.running_threads
            .insert(thread.id.clone(), thread.clone());
        if let Some(history) = params.history.as_ref() {
            for item in history {
                self.store.append_message(
                    &thread.id,
                    "history",
                    &item.to_string(),
                    Some(item.clone()),
                )?;
            }
        }

        Ok(Some(NewThread {
            model: params.model.clone().unwrap_or_else(|| "auto".to_string()),
            model_provider: params.model_provider.clone().unwrap_or(model_provider),
            cwd: thread.cwd.clone(),
            approval_policy: params.approval_policy.clone(),
            sandbox: params.sandbox.clone(),
            thread,
        }))
    }

    /// Forks an existing thread into a new one, inheriting the parent's provider.
    pub fn fork_thread(
        &mut self,
        params: &ThreadForkParams,
        fallback_cwd: &Path,
    ) -> Result<Option<NewThread>> {
        let parent = self.store.get_thread(&params.thread_id)?;
        let Some(parent) = parent else {
            return Ok(None);
        };
        let parent_thread = to_protocol_thread(parent);
        // Copy the parent's actual history (its current branch), then append
        // the provenance marker last so the fork preview derives from the
        // first real item instead of raw marker JSON. A marker-only fork
        // starts effectively empty, contradicting `InitialHistory::Forked`'s
        // documented "with the given history items".
        let parent_id = parent_thread.id.clone();
        // Synthesize an item from role/content when `item` is absent:
        // user turns are persisted via `append_message(.., None)`, so the
        // old `filter_map(record.item)` dropped EVERY user prompt from the
        // forked history.
        let mut history: Vec<Value> = self
            .store
            .list_messages(&parent_id, None)?
            .into_iter()
            .map(|record| {
                record.item.unwrap_or_else(|| {
                    json!({
                        "type": "message",
                        "role": record.role,
                        "content": record.content,
                    })
                })
            })
            .collect();
        history.push(json!({
            "type": "fork",
            "from_thread_id": parent_id
        }));
        let new = self.spawn_thread_with_history(
            params
                .model_provider
                .clone()
                .unwrap_or_else(|| parent_thread.model_provider.clone()),
            params
                .cwd
                .clone()
                .unwrap_or_else(|| fallback_cwd.to_path_buf()),
            InitialHistory::Forked(history),
            params.persist_extended_history,
        )?;
        Ok(Some(new))
    }

    /// Lists threads matching the given filter parameters.
    pub fn list_threads(&self, params: &ThreadListParams) -> Result<Vec<Thread>> {
        let list = self.store.list_threads(ThreadListFilters {
            include_archived: params.include_archived,
            limit: params.limit,
        })?;
        Ok(list.into_iter().map(to_protocol_thread).collect())
    }

    /// Reads a single thread by id, or `None` if not found.
    pub fn read_thread(&self, params: &ThreadReadParams) -> Result<Option<Thread>> {
        Ok(self
            .store
            .get_thread(&params.thread_id)?
            .map(to_protocol_thread))
    }

    /// Sets the display name for a thread, returning the updated thread or `None`.
    pub fn set_thread_name(&mut self, params: &ThreadSetNameParams) -> Result<Option<Thread>> {
        let Some(mut metadata) = self.store.get_thread(&params.thread_id)? else {
            return Ok(None);
        };
        metadata.name = Some(params.name.clone());
        metadata.updated_at = chrono::Utc::now().timestamp();
        self.store.upsert_thread(&metadata)?;
        let updated = to_protocol_thread(metadata);
        self.running_threads
            .insert(updated.id.clone(), updated.clone());
        Ok(Some(updated))
    }

    /// Archives a thread so it no longer appears in default listings.
    pub fn archive_thread(&mut self, thread_id: &str) -> Result<()> {
        self.store.mark_archived(thread_id)?;
        if let Some(thread) = self.running_threads.get_mut(thread_id) {
            thread.status = ThreadStatus::Archived;
        }
        Ok(())
    }

    /// Restores an archived thread to active status.
    pub fn unarchive_thread(&mut self, thread_id: &str) -> Result<()> {
        self.store.mark_unarchived(thread_id)?;
        // Mirror archive_thread: the cached entry would otherwise keep
        // reporting Archived after a successful unarchive.
        if let Some(thread) = self.running_threads.get_mut(thread_id) {
            thread.status = ThreadStatus::Idle;
        }
        Ok(())
    }

    /// Records a user message in a thread and updates its preview and timestamp.
    pub fn touch_message(&mut self, thread_id: &str, input: &str) -> Result<()> {
        // Fail loud on an unknown thread: returning Ok(()) made
        // `ThreadRequest::Message` acknowledge a message that was never
        // recorded, and `handle_prompt` then died later with an unrelated
        // leaf-id error.
        let Some(mut metadata) = self.store.get_thread(thread_id)? else {
            anyhow::bail!("cannot record message: thread {thread_id} not found");
        };
        metadata.updated_at = chrono::Utc::now().timestamp();
        metadata.preview = truncate_preview(input);
        metadata.status = PersistedThreadStatus::Running;
        self.store.upsert_thread(&metadata)?;
        if let Some(thread) = self.running_threads.get_mut(thread_id) {
            thread.updated_at = metadata.updated_at;
            thread.preview = metadata.preview;
            thread.status = ThreadStatus::Running;
        }
        let message_id = self.store.append_message(thread_id, "user", input, None)?;
        self.store.save_checkpoint(
            thread_id,
            "latest",
            &json!({
                "reason": "thread_message",
                "message_id": message_id,
                "role": "user",
                "preview": truncate_preview(input),
                "updated_at": metadata.updated_at
            }),
        )?;
        Ok(())
    }

    fn persist_thread(&self, thread: &Thread) -> Result<()> {
        self.store.upsert_thread(&ThreadMetadata {
            id: thread.id.clone(),
            preview: thread.preview.clone(),
            ephemeral: thread.ephemeral,
            model_provider: thread.model_provider.clone(),
            created_at: thread.created_at,
            updated_at: thread.updated_at,
            status: to_persisted_status(&thread.status),
            path: thread.path.clone(),
            cwd: thread.cwd.clone(),
            cli_version: thread.cli_version.clone(),
            source: to_persisted_source(&thread.source),
            name: thread.name.clone(),
            sandbox_policy: None,
            approval_mode: None,
            archived: matches!(thread.status, ThreadStatus::Archived),
            archived_at: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
            memory_mode: None,
            current_leaf_id: None,
        })
    }
}

/// Top-level runtime combining config, model registry, threads, tools, MCP, and hooks.
pub struct Runtime {
    /// Resolved application configuration.
    pub config: ConfigToml,
    /// Registry of available model providers.
    pub model_registry: ModelRegistry,
    /// Manages conversation thread lifecycle.
    ///
    /// Interior-mutability seam: every `Runtime` method is `&self`, so the
    /// app-server shares one `Arc<Runtime>` across handlers without a
    /// process-global lock. The mutex guards the running-thread table and
    /// its state store (all synchronous operations — a guard is never held
    /// across an `.await`), which serializes thread mutations against each
    /// other while everything else (hook emission, tool dispatch, MCP
    /// startup, job reads) runs concurrently.
    pub thread_manager: std::sync::Mutex<ThreadManager>,
    /// Manager for MCP server connections.
    pub mcp_manager: Arc<McpManager>,
    /// Engine for evaluating execution policy decisions.
    pub exec_policy: ExecPolicyEngine,
    /// Dispatcher for lifecycle hooks.
    pub hooks: HookDispatcher,
    /// Manager for background job lifecycle.
    pub jobs: JobManager,
}

impl Runtime {
    /// Constructs a new `Runtime`, loading existing jobs from the state store.
    pub fn new(
        config: ConfigToml,
        model_registry: ModelRegistry,
        state: StateStore,
        mcp_manager: Arc<McpManager>,
        exec_policy: ExecPolicyEngine,
        hooks: HookDispatcher,
    ) -> Self {
        let mut jobs = JobManager::default();
        if let Err(err) = jobs.load_from_store(&state) {
            // Starting with an empty job list is recoverable, but it must be
            // visible — otherwise persisted jobs vanish with no trace.
            tracing::error!("failed to load jobs from state store: {err:#}");
        } else {
            // Jobs that were Running/Queued at crash time reload in that
            // state with no executor attached and would stay stuck forever.
            // `resume_pending` exists exactly for this — without the call it
            // was dead code on the only load path.
            for job in jobs.resume_pending() {
                if let Err(err) = jobs.persist_job(&state, &job.id) {
                    tracing::error!("failed to persist requeued job {}: {err:#}", job.id);
                }
            }
        }
        Self {
            config,
            model_registry,
            thread_manager: std::sync::Mutex::new(ThreadManager::new(state)),
            mcp_manager,
            exec_policy,
            hooks,
            jobs,
        }
    }

    fn persisted_thread_data(&self, thread_id: &str) -> Result<Value> {
        const HISTORY_LIMIT: usize = 500;
        let mut records = self
            .thread_manager
            .lock()
            .unwrap()
            .state_store()
            .list_messages(thread_id, Some(HISTORY_LIMIT + 1))?;
        // `list_messages` walks back from the leaf, so the cap keeps the
        // most recent messages; older turns are silently dropped unless the
        // payload says so.
        let truncated = records.len() > HISTORY_LIMIT;
        if truncated {
            records.drain(..records.len() - HISTORY_LIMIT);
        }
        let history = records
            .into_iter()
            .map(|message| {
                json!({
                    "id": message.id,
                    "role": message.role,
                    "content": message.content,
                    "item": message.item,
                    "created_at": message.created_at
                })
            })
            .collect::<Vec<_>>();

        let checkpoint = self
            .thread_manager
            .lock()
            .unwrap()
            .state_store()
            .load_checkpoint(thread_id, None)?
            .map(|record| {
                json!({
                    "checkpoint_id": record.checkpoint_id,
                    "state": record.state,
                    "created_at": record.created_at
                })
            });

        Ok(json!({
            "history": history,
            "truncated": truncated,
            "checkpoint": checkpoint
        }))
    }

    fn persist_latest_checkpoint(&self, thread_id: &str, reason: &str, state: Value) -> Result<()> {
        self.thread_manager
            .lock()
            .unwrap()
            .state_store()
            .save_checkpoint(
                thread_id,
                "latest",
                &json!({
                    "reason": reason,
                    "saved_at": chrono::Utc::now().timestamp(),
                    "state": state
                }),
            )
    }

    /// Dispatches a thread request (create, start, resume, fork, list, read, etc.).
    pub async fn handle_thread(&self, req: ThreadRequest) -> Result<ThreadResponse> {
        match req {
            ThreadRequest::Create { .. } => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let new = self
                    .thread_manager
                    .lock()
                    .unwrap()
                    .spawn_thread_with_history(
                        self.config.provider.as_str().to_string(),
                        cwd,
                        InitialHistory::New,
                        false,
                    )?;
                let mut response = thread_response_from_new("created", new);
                response.data = self.persisted_thread_data(&response.thread_id)?;
                Ok(response)
            }
            ThreadRequest::Start(params) => {
                let cwd = params.cwd.clone().unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                });
                let new = self
                    .thread_manager
                    .lock()
                    .unwrap()
                    .spawn_thread_with_history(
                        params
                            .model_provider
                            .clone()
                            .unwrap_or_else(|| self.config.provider.as_str().to_string()),
                        cwd,
                        InitialHistory::New,
                        params.persist_extended_history,
                    )?;
                let mut response = thread_response_from_new("started", new);
                response.data = self.persisted_thread_data(&response.thread_id)?;
                Ok(response)
            }
            ThreadRequest::Resume(params) => {
                let fallback_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                // Bind the result before the `if let`: a MutexGuard temporary
                // in the scrutinee lives to the end of the whole `if let`
                // expression, and `persisted_thread_data` below re-locks the
                // same mutex — a self-deadlock on every store-backed resume.
                let resumed = self
                    .thread_manager
                    .lock()
                    .unwrap()
                    .resume_thread_with_history(
                        &params,
                        &fallback_cwd,
                        self.config.provider.as_str().to_string(),
                    )?;
                if let Some(new) = resumed {
                    let mut response = thread_response_from_new("resumed", new);
                    response.data = self.persisted_thread_data(&response.thread_id)?;
                    Ok(response)
                } else {
                    Ok(ThreadResponse {
                        thread_id: params.thread_id,
                        status: "missing".to_string(),
                        thread: None,
                        threads: Vec::new(),
                        model: None,
                        model_provider: None,
                        cwd: None,
                        approval_policy: params.approval_policy,
                        sandbox: params.sandbox,
                        events: Vec::new(),
                        data: json!({"error":"thread not found"}),
                    })
                }
            }
            ThreadRequest::Fork(params) => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                // Same guard-scoping fix as the Resume arm above: the fork
                // response reads persisted history, which re-locks.
                let forked = self
                    .thread_manager
                    .lock()
                    .unwrap()
                    .fork_thread(&params, &cwd)?;
                if let Some(new) = forked {
                    let mut response = thread_response_from_new("forked", new);
                    response.data = self.persisted_thread_data(&response.thread_id)?;
                    Ok(response)
                } else {
                    Ok(ThreadResponse {
                        thread_id: params.thread_id,
                        status: "missing".to_string(),
                        thread: None,
                        threads: Vec::new(),
                        model: None,
                        model_provider: None,
                        cwd: None,
                        approval_policy: params.approval_policy,
                        sandbox: params.sandbox,
                        events: Vec::new(),
                        data: json!({"error":"thread not found"}),
                    })
                }
            }
            ThreadRequest::List(params) => Ok(ThreadResponse {
                thread_id: "list".to_string(),
                status: "ok".to_string(),
                thread: None,
                threads: self.thread_manager.lock().unwrap().list_threads(&params)?,
                model: None,
                model_provider: None,
                cwd: None,
                approval_policy: None,
                sandbox: None,
                events: Vec::new(),
                data: json!({}),
            }),
            ThreadRequest::Read(params) => {
                let id = params.thread_id.clone();
                let data = self.persisted_thread_data(&id)?;
                Ok(ThreadResponse {
                    thread_id: id,
                    status: "ok".to_string(),
                    thread: self.thread_manager.lock().unwrap().read_thread(&params)?,
                    threads: Vec::new(),
                    model: None,
                    model_provider: None,
                    cwd: None,
                    approval_policy: None,
                    sandbox: None,
                    events: Vec::new(),
                    data,
                })
            }
            ThreadRequest::SetName(params) => Ok(ThreadResponse {
                thread_id: params.thread_id.clone(),
                status: "ok".to_string(),
                thread: self
                    .thread_manager
                    .lock()
                    .unwrap()
                    .set_thread_name(&params)?,
                threads: Vec::new(),
                model: None,
                model_provider: None,
                cwd: None,
                approval_policy: None,
                sandbox: None,
                events: Vec::new(),
                data: json!({}),
            }),
            ThreadRequest::Archive { thread_id } => {
                self.thread_manager
                    .lock()
                    .unwrap()
                    .archive_thread(&thread_id)?;
                Ok(ThreadResponse {
                    thread_id,
                    status: "archived".to_string(),
                    thread: None,
                    threads: Vec::new(),
                    model: None,
                    model_provider: None,
                    cwd: None,
                    approval_policy: None,
                    sandbox: None,
                    events: Vec::new(),
                    data: json!({}),
                })
            }
            ThreadRequest::Unarchive { thread_id } => {
                self.thread_manager
                    .lock()
                    .unwrap()
                    .unarchive_thread(&thread_id)?;
                Ok(ThreadResponse {
                    thread_id,
                    status: "unarchived".to_string(),
                    thread: None,
                    threads: Vec::new(),
                    model: None,
                    model_provider: None,
                    cwd: None,
                    approval_policy: None,
                    sandbox: None,
                    events: Vec::new(),
                    data: json!({}),
                })
            }
            ThreadRequest::Message { thread_id, input } => {
                self.thread_manager
                    .lock()
                    .unwrap()
                    .touch_message(&thread_id, &input)?;
                // UUID-based like `handle_prompt`: `{thread_id}:{len}` collides
                // for same-length inputs, conflating distinct turns in any
                // response_id-keyed consumer.
                let response_id = format!("resp-{}", Uuid::new_v4());
                self.hooks
                    .emit(HookEvent::ResponseStart {
                        response_id: response_id.clone(),
                    })
                    .await;
                self.hooks
                    .emit(HookEvent::ResponseEnd {
                        response_id: response_id.clone(),
                    })
                    .await;

                Ok(ThreadResponse {
                    thread_id,
                    status: "accepted".to_string(),
                    thread: None,
                    threads: Vec::new(),
                    model: None,
                    model_provider: None,
                    cwd: None,
                    approval_policy: None,
                    sandbox: None,
                    events: vec![
                        EventFrame::ResponseStart {
                            response_id: response_id.clone(),
                        },
                        EventFrame::ResponseDelta {
                            response_id: response_id.clone(),
                            delta: "queued".to_string(),
                            channel: ResponseChannel::Text,
                        },
                        EventFrame::ResponseEnd { response_id },
                    ],
                    data: json!({}),
                })
            }
        }
    }

    /// Resolves the model for a prompt, records the message, and returns the response.
    pub async fn handle_prompt(
        &self,
        req: PromptRequest,
        cli_overrides: &CliRuntimeOverrides,
    ) -> Result<PromptResponse> {
        let resolved = self.config.resolve_runtime_options(cli_overrides)?;
        let requested_model = req.model.clone().unwrap_or_else(|| resolved.model.clone());
        let selection = self
            .model_registry
            .resolve(Some(&requested_model), Some(resolved.provider));
        let resolved_model = selection.resolved.id.clone();
        let response_id = format!("resp-{}", Uuid::new_v4());

        self.hooks
            .emit(HookEvent::ResponseStart {
                response_id: response_id.clone(),
            })
            .await;
        self.hooks
            .emit(HookEvent::ResponseDelta {
                response_id: response_id.clone(),
                delta: "model-selected".to_string(),
            })
            .await;
        self.hooks
            .emit(HookEvent::ResponseEnd {
                response_id: response_id.clone(),
            })
            .await;

        let payload = json!({
            "provider": resolved.provider.as_str(),
            "model": resolved_model.clone(),
            "prompt": req.prompt,
            "telemetry": resolved.telemetry,
            "base_url": resolved.base_url,
            "has_api_key": resolved.api_key.as_ref().is_some_and(|k| !k.trim().is_empty()),
            "approval_policy": resolved.approval_policy,
            "sandbox_mode": resolved.sandbox_mode
        });
        if let Some(thread_id) = req.thread_id.as_ref() {
            let mut thread_manager = self.thread_manager.lock().unwrap();
            thread_manager.touch_message(thread_id, &req.prompt)?;
            // Runtime metadata, not assistant output — a "assistant" role
            // would be replayed as conversation history by Read/Resume data,
            // forks, and any list_messages consumer, polluting model context
            // with config echoes.
            let assistant_message_id = thread_manager.store.append_message(
                thread_id,
                "metadata",
                &payload.to_string(),
                Some(payload.clone()),
            )?;
            drop(thread_manager);
            self.persist_latest_checkpoint(
                thread_id,
                "prompt_response",
                json!({
                    "response_id": response_id.clone(),
                    "model": resolved_model.clone(),
                    "provider": resolved.provider.as_str(),
                    "assistant_message_id": assistant_message_id
                }),
            )?;
        }

        Ok(PromptResponse {
            output: payload.to_string(),
            model: resolved_model,
            events: vec![
                EventFrame::ResponseStart {
                    response_id: response_id.clone(),
                },
                EventFrame::ResponseDelta {
                    response_id: response_id.clone(),
                    delta: "model-selected".to_string(),
                    channel: ResponseChannel::Text,
                },
                EventFrame::ResponseEnd { response_id },
            ],
        })
    }

    /// Evaluates execution policy for a tool call.
    ///
    /// This runtime is a policy gate, not a tool host: a call the policy
    /// allows (and that needs no approval) fails loud with a no-executor
    /// error — the value-layer registry that used to sit here was deleted
    /// in the §E surface unification, and it had never held a tool outside
    /// tests anyway.
    pub async fn invoke_tool(
        &self,
        call: ToolCall,
        approval_mode: AskForApproval,
        cwd: &Path,
    ) -> Result<Value> {
        let fallback_cwd = cwd.display().to_string();
        let (command, policy_cwd, execution_kind) = call.execution_subject(&fallback_cwd);
        let policy_tool = match &call.payload {
            ToolPayload::LocalShell { .. } => "exec_shell",
            _ => call.name.as_str(),
        };
        let resolved = self
            .config
            .resolve_runtime_options(&CliRuntimeOverrides::default())?;
        let policy_path = policy_subject_path(&call.payload);
        let decision = self.exec_policy.check(ExecPolicyContext {
            command: &command,
            cwd: &policy_cwd,
            tool: Some(policy_tool),
            path: policy_path.as_deref(),
            ask_for_approval: approval_mode,
            sandbox_mode: resolved.sandbox_mode.as_deref(),
        })?;
        let precheck = policy_precheck_payload(&decision, &command, &policy_cwd, execution_kind);
        let response_id = format!("tool-{}", Uuid::new_v4());
        let call_id = call
            .raw_tool_call_id
            .clone()
            .unwrap_or_else(|| format!("tool-call-{}", Uuid::new_v4()));
        self.hooks
            .emit(HookEvent::ToolLifecycle {
                response_id: response_id.clone(),
                tool_name: call.name.clone(),
                phase: "precheck".to_string(),
                payload: precheck.clone(),
            })
            .await;

        if !decision.allow {
            let reason = decision.reason().to_string();
            let approval_id = format!("approval-{}", Uuid::new_v4());
            let error_frame = EventFrame::Error {
                response_id: response_id.clone(),
                message: reason.clone(),
            };
            self.hooks
                .emit(HookEvent::ApprovalLifecycle {
                    approval_id,
                    phase: "denied".to_string(),
                    reason: Some(reason.clone()),
                })
                .await;
            self.hooks
                .emit(HookEvent::GenericEventFrame {
                    frame: error_frame.clone(),
                })
                .await;
            return Ok(json!({
                "ok": false,
                "status": "denied",
                "execution_kind": execution_kind,
                "response_id": response_id,
                "precheck": precheck,
                "error": reason,
                "events": [event_frame_payload(&error_frame)],
            }));
        }

        if decision.requires_approval {
            let approval_id = format!("approval-{}", Uuid::new_v4());
            let reason = decision.reason().to_string();
            let maybe_approval_frame = approval_request_frame(
                &decision.requirement,
                call_id,
                approval_id.clone(),
                response_id.clone(),
                command.clone(),
                policy_cwd.clone(),
            );
            self.hooks
                .emit(HookEvent::ApprovalLifecycle {
                    approval_id: approval_id.clone(),
                    phase: "requested".to_string(),
                    reason: Some(reason.clone()),
                })
                .await;
            let mut events = Vec::new();
            if let Some(frame) = maybe_approval_frame {
                self.hooks
                    .emit(HookEvent::GenericEventFrame {
                        frame: frame.clone(),
                    })
                    .await;
                events.push(event_frame_payload(&frame));
            }
            return Ok(json!({
                "ok": false,
                "status": "approval_required",
                "execution_kind": execution_kind,
                "response_id": response_id,
                "approval_id": approval_id,
                "precheck": precheck,
                "error": reason,
                "events": events,
            }));
        }

        let start_frame = EventFrame::ToolCallStart {
            response_id: response_id.clone(),
            tool_name: call.name.clone(),
            arguments: tool_payload_value(&call.payload),
        };
        self.hooks
            .emit(HookEvent::GenericEventFrame {
                frame: start_frame.clone(),
            })
            .await;
        self.hooks
            .emit(HookEvent::ToolLifecycle {
                response_id: response_id.clone(),
                tool_name: call.name.clone(),
                phase: "dispatching".to_string(),
                payload: json!({
                    "call_id": call_id,
                    "execution_kind": execution_kind
                }),
            })
            .await;

        let message = format!(
            "tool '{}' was allowed by policy, but this runtime has no tool \
             executor — the /tool endpoint is a policy gate, not a tool host",
            call.name
        );
        let error_frame = EventFrame::Error {
            response_id: response_id.clone(),
            message: message.clone(),
        };
        self.hooks
            .emit(HookEvent::GenericEventFrame {
                frame: error_frame.clone(),
            })
            .await;
        self.hooks
            .emit(HookEvent::ToolLifecycle {
                response_id: response_id.clone(),
                tool_name: call.name,
                phase: "failed".to_string(),
                payload: json!({ "error": message.clone() }),
            })
            .await;
        Ok(json!({
            "ok": false,
            "status": "failed",
            "execution_kind": execution_kind,
            "response_id": response_id,
            "precheck": precheck,
            "error": message,
            "events": [
                event_frame_payload(&start_frame),
                event_frame_payload(&error_frame)
            ]
        }))
    }

    /// Starts all configured MCP servers and emits startup events via hooks.
    pub async fn mcp_startup(&self) -> McpStartupCompleteEvent {
        let mut updates = Vec::new();
        let summary = self.mcp_manager.start_all(|update| {
            updates.push(update);
        });
        for update in updates {
            let status = match update.status {
                McpManagerStartupStatus::Starting => codesmith_protocol::McpStartupStatus::Starting,
                McpManagerStartupStatus::Ready => codesmith_protocol::McpStartupStatus::Ready,
                McpManagerStartupStatus::Failed { error } => {
                    codesmith_protocol::McpStartupStatus::Failed { error }
                }
                McpManagerStartupStatus::Cancelled => {
                    codesmith_protocol::McpStartupStatus::Cancelled
                }
            };
            self.hooks
                .emit(HookEvent::GenericEventFrame {
                    frame: EventFrame::McpStartupUpdate {
                        update: codesmith_protocol::McpStartupUpdateEvent {
                            server_name: update.server_name,
                            status,
                        },
                    },
                })
                .await;
        }
        self.hooks
            .emit(HookEvent::GenericEventFrame {
                frame: EventFrame::McpStartupComplete {
                    summary: codesmith_protocol::McpStartupCompleteEvent {
                        ready: summary.ready.clone(),
                        failed: summary
                            .failed
                            .iter()
                            .map(|f| codesmith_protocol::McpStartupFailure {
                                server_name: f.server_name.clone(),
                                error: f.error.clone(),
                            })
                            .collect(),
                        cancelled: summary.cancelled.clone(),
                    },
                },
            })
            .await;
        summary
    }

    /// Returns the current application status including all jobs and their history.
    pub fn app_status(&self) -> AppResponse {
        let jobs = self.jobs.list();
        let events = jobs
            .iter()
            .flat_map(|job| {
                job.history.iter().map(|entry| EventFrame::ResponseDelta {
                    response_id: job.id.clone(),
                    delta: json!({
                        "kind": "job_transition",
                        "job_id": job.id.clone(),
                        "phase": entry.phase.clone(),
                        "status": job_status_to_str(entry.status),
                        "progress": entry.progress,
                        "detail": entry.detail.clone(),
                        "retry": job_retry_to_value(&entry.retry),
                        "at": entry.at
                    })
                    .to_string(),
                    channel: ResponseChannel::Text,
                })
            })
            .collect::<Vec<_>>();
        AppResponse {
            ok: true,
            data: json!({
                "jobs": jobs.into_iter().map(|job| {
                    json!({
                        "id": job.id,
                        "name": job.name,
                        "status": job_status_to_str(job.status),
                        "progress": job.progress,
                        "detail": job.detail,
                        "retry": job_retry_to_value(&job.retry),
                        "history": job.history.iter().map(job_history_to_value).collect::<Vec<_>>()
                    })
                }).collect::<Vec<_>>()
            }),
            events,
        }
    }

    /// Returns the default model provider from the resolved configuration.
    pub fn provider_default(&self) -> ProviderKind {
        self.config.provider
    }

    /// Saves a named checkpoint for a thread.
    pub fn save_thread_checkpoint(
        &self,
        thread_id: &str,
        checkpoint_id: &str,
        state: &Value,
    ) -> Result<()> {
        self.thread_manager
            .lock()
            .unwrap()
            .state_store()
            .save_checkpoint(thread_id, checkpoint_id, state)
    }

    /// Loads a checkpoint for a thread. Pass `None` for the latest.
    pub fn load_thread_checkpoint(
        &self,
        thread_id: &str,
        checkpoint_id: Option<&str>,
    ) -> Result<Option<Value>> {
        Ok(self
            .thread_manager
            .lock()
            .unwrap()
            .state_store()
            .load_checkpoint(thread_id, checkpoint_id)?
            .map(|checkpoint| checkpoint.state))
    }

    /// Enqueues a new background job and persists it immediately.
    pub fn enqueue_job(&mut self, name: impl Into<String>) -> Result<JobRecord> {
        let job = self.jobs.enqueue(name);
        self.jobs
            .persist_job(self.thread_manager.lock().unwrap().state_store(), &job.id)?;
        Ok(job)
    }

    /// Transitions a job to running and persists the change.
    pub fn set_job_running(&mut self, job_id: &str) -> Result<()> {
        self.jobs.set_running(job_id);
        self.jobs
            .persist_job(self.thread_manager.lock().unwrap().state_store(), job_id)
    }

    /// Updates a job's progress and persists the change.
    pub fn update_job_progress(
        &mut self,
        job_id: &str,
        progress: u8,
        detail: Option<String>,
    ) -> Result<()> {
        self.jobs.update_progress(job_id, progress, detail);
        self.jobs
            .persist_job(self.thread_manager.lock().unwrap().state_store(), job_id)
    }

    /// Marks a job as completed and persists the change.
    pub fn complete_job(&mut self, job_id: &str) -> Result<()> {
        self.jobs.complete(job_id);
        self.jobs
            .persist_job(self.thread_manager.lock().unwrap().state_store(), job_id)
    }

    /// Marks a job as failed and persists the change.
    pub fn fail_job(&mut self, job_id: &str, detail: impl Into<String>) -> Result<()> {
        self.jobs.fail(job_id, detail);
        self.jobs
            .persist_job(self.thread_manager.lock().unwrap().state_store(), job_id)
    }

    /// Cancels a job and persists the change.
    pub fn cancel_job(&mut self, job_id: &str) -> Result<()> {
        self.jobs.cancel(job_id);
        self.jobs
            .persist_job(self.thread_manager.lock().unwrap().state_store(), job_id)
    }

    /// Pauses a job and persists the change.
    pub fn pause_job(&mut self, job_id: &str, detail: Option<String>) -> Result<()> {
        self.jobs.pause(job_id, detail);
        self.jobs
            .persist_job(self.thread_manager.lock().unwrap().state_store(), job_id)
    }

    /// Resumes a paused job and persists the change.
    pub fn resume_job(&mut self, job_id: &str, detail: Option<String>) -> Result<()> {
        self.jobs.resume(job_id, detail);
        self.jobs
            .persist_job(self.thread_manager.lock().unwrap().state_store(), job_id)
    }

    /// Returns the state-transition history for a job.
    pub fn job_history(&self, job_id: &str) -> Vec<JobHistoryEntry> {
        self.jobs.history(job_id)
    }
}

fn thread_response_from_new(status: &str, new: NewThread) -> ThreadResponse {
    ThreadResponse {
        thread_id: new.thread.id.clone(),
        status: status.to_string(),
        thread: Some(new.thread),
        threads: Vec::new(),
        model: Some(new.model),
        model_provider: Some(new.model_provider),
        cwd: Some(new.cwd),
        approval_policy: new.approval_policy,
        sandbox: new.sandbox,
        events: Vec::new(),
        data: json!({}),
    }
}

fn preview_from_initial_history(initial_history: &InitialHistory) -> String {
    match initial_history {
        InitialHistory::New => "New conversation".to_string(),
        InitialHistory::Forked(items) => truncate_preview(
            &items
                .first()
                .map(Value::to_string)
                .unwrap_or_else(|| "Forked conversation".to_string()),
        ),
        InitialHistory::Resumed { history, .. } => truncate_preview(
            &history
                .first()
                .map(Value::to_string)
                .unwrap_or_else(|| "Resumed conversation".to_string()),
        ),
    }
}

fn truncate_preview(value: &str) -> String {
    value.chars().take(120).collect()
}

fn to_protocol_thread(thread: ThreadMetadata) -> Thread {
    Thread {
        id: thread.id,
        preview: thread.preview,
        ephemeral: thread.ephemeral,
        model_provider: thread.model_provider,
        created_at: thread.created_at,
        updated_at: thread.updated_at,
        status: match thread.status {
            PersistedThreadStatus::Running => ThreadStatus::Running,
            PersistedThreadStatus::Idle => ThreadStatus::Idle,
            PersistedThreadStatus::Completed => ThreadStatus::Completed,
            PersistedThreadStatus::Failed => ThreadStatus::Failed,
            PersistedThreadStatus::Paused => ThreadStatus::Paused,
            PersistedThreadStatus::Archived => ThreadStatus::Archived,
        },
        path: thread.path,
        cwd: thread.cwd,
        cli_version: thread.cli_version,
        source: match thread.source {
            SessionSource::Interactive => codesmith_protocol::SessionSource::Interactive,
            SessionSource::Resume => codesmith_protocol::SessionSource::Resume,
            SessionSource::Fork => codesmith_protocol::SessionSource::Fork,
            SessionSource::Api => codesmith_protocol::SessionSource::Api,
            SessionSource::Unknown => codesmith_protocol::SessionSource::Unknown,
        },
        name: thread.name,
    }
}

fn to_persisted_status(status: &ThreadStatus) -> PersistedThreadStatus {
    match status {
        ThreadStatus::Running => PersistedThreadStatus::Running,
        ThreadStatus::Idle => PersistedThreadStatus::Idle,
        ThreadStatus::Completed => PersistedThreadStatus::Completed,
        ThreadStatus::Failed => PersistedThreadStatus::Failed,
        ThreadStatus::Paused => PersistedThreadStatus::Paused,
        ThreadStatus::Archived => PersistedThreadStatus::Archived,
    }
}

fn to_persisted_source(source: &codesmith_protocol::SessionSource) -> SessionSource {
    match source {
        codesmith_protocol::SessionSource::Interactive => SessionSource::Interactive,
        codesmith_protocol::SessionSource::Resume => SessionSource::Resume,
        codesmith_protocol::SessionSource::Fork => SessionSource::Fork,
        codesmith_protocol::SessionSource::Api => SessionSource::Api,
        codesmith_protocol::SessionSource::Unknown => SessionSource::Unknown,
    }
}

fn approval_request_frame(
    requirement: &ExecApprovalRequirement,
    call_id: String,
    approval_id: String,
    turn_id: String,
    command: String,
    cwd: String,
) -> Option<EventFrame> {
    let ExecApprovalRequirement::NeedsApproval {
        reason,
        proposed_execpolicy_amendment,
        proposed_network_policy_amendments,
    } = requirement
    else {
        return None;
    };

    let mut available_decisions = vec![
        ReviewDecision::Approved,
        ReviewDecision::ApprovedForSession,
        ReviewDecision::Denied,
        ReviewDecision::Abort,
    ];
    if proposed_execpolicy_amendment
        .as_ref()
        .is_some_and(|amendment| !amendment.prefixes.is_empty())
    {
        available_decisions.push(ReviewDecision::ApprovedExecpolicyAmendment);
    }
    available_decisions.extend(proposed_network_policy_amendments.iter().cloned().map(
        |amendment| ReviewDecision::NetworkPolicyAmendment {
            host: amendment.host,
            action: amendment.action,
        },
    ));

    Some(EventFrame::ExecApprovalRequest {
        request: ExecApprovalRequestEvent {
            call_id,
            approval_id,
            turn_id,
            command,
            cwd,
            reason: reason.clone(),
            network_approval_context: None,
            proposed_execpolicy_amendment: proposed_execpolicy_amendment
                .as_ref()
                .map(|amendment| amendment.prefixes.clone())
                .unwrap_or_default(),
            proposed_network_policy_amendments: proposed_network_policy_amendments.clone(),
            additional_permissions: Vec::new(),
            available_decisions,
        },
    })
}

fn approval_requirement_payload(requirement: &ExecApprovalRequirement) -> Value {
    match requirement {
        ExecApprovalRequirement::Skip {
            bypass_sandbox,
            proposed_execpolicy_amendment,
        } => json!({
            "type": "skip",
            "bypass_sandbox": bypass_sandbox,
            "reason": requirement.reason(),
            "proposed_execpolicy_amendment": proposed_execpolicy_amendment
                .as_ref()
                .map(|amendment| amendment.prefixes.clone())
                .unwrap_or_default()
        }),
        ExecApprovalRequirement::NeedsApproval {
            reason,
            proposed_execpolicy_amendment,
            proposed_network_policy_amendments,
        } => json!({
            "type": "needs_approval",
            "reason": reason,
            "proposed_execpolicy_amendment": proposed_execpolicy_amendment
                .as_ref()
                .map(|amendment| amendment.prefixes.clone())
                .unwrap_or_default(),
            "proposed_network_policy_amendments": proposed_network_policy_amendments
        }),
        ExecApprovalRequirement::Forbidden { reason } => json!({
            "type": "forbidden",
            "reason": reason
        }),
    }
}

fn policy_precheck_payload(
    decision: &ExecPolicyDecision,
    command: &str,
    cwd: &str,
    execution_kind: &str,
) -> Value {
    json!({
        "execution_kind": execution_kind,
        "command": command,
        "cwd": cwd,
        "allow": decision.allow,
        "requires_approval": decision.requires_approval,
        "matched_rule": decision.matched_rule.clone(),
        "phase": decision.requirement.phase(),
        "reason": decision.reason(),
        "requirement": approval_requirement_payload(&decision.requirement)
    })
}

fn tool_payload_value(payload: &ToolPayload) -> Value {
    serde_json::to_value(payload).unwrap_or_else(
        |_| json!({"type":"serialization_error","message":"tool payload unavailable"}),
    )
}

/// Extracts the file path a tool call acts on, for path-keyed policy rules.
/// Only function-tool JSON arguments carrying a `path`/`file_path` string
/// yield a subject; shell/MCP payloads have no single path.
fn policy_subject_path(payload: &ToolPayload) -> Option<String> {
    let ToolPayload::Function { arguments } = payload else {
        return None;
    };
    let value = serde_json::from_str::<Value>(arguments.as_str()).ok()?;
    ["path", "file_path"]
        .iter()
        .filter_map(|key| value.get(*key))
        .find_map(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(str::to_string)
}

fn event_frame_payload(frame: &EventFrame) -> Value {
    serde_json::to_value(frame)
        .unwrap_or_else(|_| json!({"event":"error","message":"failed to encode event frame"}))
}

fn json_optional_string(value: &Value) -> Option<String> {
    if value.is_null() {
        None
    } else {
        value.as_str().map(ToString::to_string)
    }
}

fn parse_retry_metadata(value: Option<&Value>) -> JobRetryMetadata {
    let Some(value) = value else {
        return JobRetryMetadata::default();
    };
    JobRetryMetadata {
        attempt: value
            .get("attempt")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        max_attempts: value
            .get("max_attempts")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_JOB_MAX_ATTEMPTS as u64)
            .min(u32::MAX as u64) as u32,
        backoff_base_ms: value
            .get("backoff_base_ms")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_JOB_BACKOFF_BASE_MS),
        next_backoff_ms: value
            .get("next_backoff_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        next_retry_at: value.get("next_retry_at").and_then(Value::as_i64),
    }
}

fn parse_history_entry(value: &Value) -> Option<JobHistoryEntry> {
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .and_then(job_status_from_str)?;
    Some(JobHistoryEntry {
        at: value.get("at").and_then(Value::as_i64).unwrap_or(0),
        phase: value
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        status,
        progress: value
            .get("progress")
            .and_then(Value::as_u64)
            .map(|v| v.min(u8::MAX as u64) as u8),
        detail: value.get("detail").and_then(json_optional_string),
        retry: parse_retry_metadata(value.get("retry")),
    })
}

fn job_status_to_str(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "queued",
        JobStatus::Running => "running",
        JobStatus::Paused => "paused",
        JobStatus::Completed => "completed",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
    }
}

fn job_status_from_str(value: &str) -> Option<JobStatus> {
    match value {
        "queued" => Some(JobStatus::Queued),
        "running" => Some(JobStatus::Running),
        "paused" => Some(JobStatus::Paused),
        "completed" => Some(JobStatus::Completed),
        "failed" => Some(JobStatus::Failed),
        "cancelled" => Some(JobStatus::Cancelled),
        _ => None,
    }
}

fn job_retry_to_value(retry: &JobRetryMetadata) -> Value {
    json!({
        "attempt": retry.attempt,
        "max_attempts": retry.max_attempts,
        "backoff_base_ms": retry.backoff_base_ms,
        "next_backoff_ms": retry.next_backoff_ms,
        "next_retry_at": retry.next_retry_at
    })
}

fn job_history_to_value(entry: &JobHistoryEntry) -> Value {
    json!({
        "at": entry.at,
        "phase": entry.phase.clone(),
        "status": job_status_to_str(entry.status),
        "progress": entry.progress,
        "detail": entry.detail.clone(),
        "retry": job_retry_to_value(&entry.retry)
    })
}

fn runtime_status_to_job_state(status: JobStatus) -> JobStateStatus {
    match status {
        JobStatus::Queued => JobStateStatus::Queued,
        JobStatus::Running => JobStateStatus::Running,
        JobStatus::Paused => JobStateStatus::Paused,
        JobStatus::Completed => JobStateStatus::Completed,
        JobStatus::Failed => JobStateStatus::Failed,
        JobStatus::Cancelled => JobStateStatus::Cancelled,
    }
}

fn job_state_status_to_runtime(status: JobStateStatus) -> JobStatus {
    match status {
        JobStateStatus::Queued => JobStatus::Queued,
        JobStateStatus::Running => JobStatus::Running,
        JobStateStatus::Paused => JobStatus::Paused,
        JobStateStatus::Completed => JobStatus::Completed,
        JobStateStatus::Failed => JobStatus::Failed,
        JobStateStatus::Cancelled => JobStatus::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── JobManager: lifecycle ──────────────────────────────────────────

    #[test]
    fn enqueue_creates_queued_job_with_zero_progress() {
        let mut jm = JobManager::default();
        let job = jm.enqueue("build");
        assert_eq!(job.name, "build");
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.progress, Some(0));
        assert!(job.detail.is_none());
        assert_eq!(job.history.len(), 1);
        assert_eq!(job.history[0].phase, "created");
    }

    #[test]
    fn set_running_transitions_from_queued() {
        let mut jm = JobManager::default();
        let job = jm.enqueue("deploy");
        let id = job.id.clone();
        jm.set_running(&id);
        let jobs = jm.list();
        let updated = jobs.iter().find(|j| j.id == id).unwrap();
        assert_eq!(updated.status, JobStatus::Running);
        assert_eq!(updated.history.last().unwrap().phase, "running");
    }

    #[test]
    fn update_progress_clamps_to_100() {
        let mut jm = JobManager::default();
        let job = jm.enqueue("task");
        let id = job.id.clone();
        jm.update_progress(&id, 150, Some("over".to_string()));
        let jobs = jm.list();
        let updated = jobs.iter().find(|j| j.id == id).unwrap();
        assert_eq!(updated.progress, Some(100));
    }

    #[test]
    fn complete_sets_progress_to_100() {
        let mut jm = JobManager::default();
        let job = jm.enqueue("task");
        let id = job.id.clone();
        jm.set_running(&id);
        jm.complete(&id);
        let jobs = jm.list();
        let updated = jobs.iter().find(|j| j.id == id).unwrap();
        assert_eq!(updated.status, JobStatus::Completed);
        assert_eq!(updated.progress, Some(100));
    }

    #[test]
    fn fail_increments_attempt_and_sets_backoff() {
        let mut jm = JobManager::default();
        let job = jm.enqueue("fragile");
        let id = job.id.clone();
        jm.set_running(&id);
        jm.fail(&id, "crashed");
        let jobs = jm.list();
        let updated = jobs.iter().find(|j| j.id == id).unwrap();
        assert_eq!(updated.status, JobStatus::Failed);
        assert_eq!(updated.retry.attempt, 1);
        assert!(updated.retry.next_backoff_ms > 0);
        assert!(updated.retry.next_retry_at.is_some());
        assert_eq!(updated.detail.as_deref(), Some("crashed"));
    }

    #[test]
    fn fail_clears_retry_after_max_attempts() {
        let mut jm = JobManager::default();
        let job = jm.enqueue("fragile");
        let id = job.id.clone();
        for _ in 0..=DEFAULT_JOB_MAX_ATTEMPTS {
            jm.set_running(&id);
            jm.fail(&id, "boom");
        }
        let jobs = jm.list();
        let updated = jobs.iter().find(|j| j.id == id).unwrap();
        assert_eq!(updated.retry.attempt, DEFAULT_JOB_MAX_ATTEMPTS);
        assert_eq!(updated.retry.next_backoff_ms, 0);
        assert!(updated.retry.next_retry_at.is_none());
    }

    #[test]
    fn cancel_sets_status_and_clears_retry() {
        let mut jm = JobManager::default();
        let job = jm.enqueue("task");
        let id = job.id.clone();
        jm.cancel(&id);
        let jobs = jm.list();
        let updated = jobs.iter().find(|j| j.id == id).unwrap();
        assert_eq!(updated.status, JobStatus::Cancelled);
        assert_eq!(updated.retry.next_backoff_ms, 0);
    }

    #[test]
    fn pause_and_resume_round_trip() {
        let mut jm = JobManager::default();
        let job = jm.enqueue("task");
        let id = job.id.clone();
        jm.set_running(&id);
        jm.pause(&id, Some("waiting".to_string()));
        let jobs = jm.list();
        let paused = jobs.iter().find(|j| j.id == id).unwrap();
        assert_eq!(paused.status, JobStatus::Paused);
        assert_eq!(paused.detail.as_deref(), Some("waiting"));

        jm.resume(&id, None);
        let jobs = jm.list();
        let resumed = jobs.iter().find(|j| j.id == id).unwrap();
        assert_eq!(resumed.status, JobStatus::Running);
        assert_eq!(resumed.history.last().unwrap().phase, "resumed");
    }

    #[test]
    fn list_returns_jobs_sorted_by_updated_at_desc() {
        let mut jm = JobManager::default();
        jm.enqueue("first");
        jm.enqueue("second");
        jm.enqueue("third");
        let jobs = jm.list();
        assert_eq!(jobs.len(), 3);
        for window in jobs.windows(2) {
            assert!(window[0].updated_at >= window[1].updated_at);
        }
    }

    #[test]
    fn history_returns_entries_for_existing_job() {
        let mut jm = JobManager::default();
        let job = jm.enqueue("task");
        let id = job.id.clone();
        jm.set_running(&id);
        jm.complete(&id);
        let history = jm.history(&id);
        assert_eq!(history.len(), 3); // created, running, completed
        assert_eq!(history[0].phase, "created");
        assert_eq!(history[1].phase, "running");
        assert_eq!(history[2].phase, "completed");
    }

    #[test]
    fn history_returns_empty_for_unknown_job() {
        let jm = JobManager::default();
        assert!(jm.history("nonexistent").is_empty());
    }

    #[test]
    fn resume_pending_requeues_running_and_queued() {
        let mut jm = JobManager::default();
        let _j1 = jm.enqueue("queued_task");
        let j2 = jm.enqueue("running_task");
        let j3 = jm.enqueue("completed_task");
        let id2 = j2.id.clone();
        let id3 = j3.id.clone();
        jm.set_running(&id2);
        jm.set_running(&id3);
        jm.complete(&id3);

        let resumed = jm.resume_pending();
        assert_eq!(resumed.len(), 2);
        for job in &resumed {
            assert_eq!(job.status, JobStatus::Queued);
        }
    }

    // ── JobManager: backoff ────────────────────────────────────────────

    #[test]
    fn deterministic_backoff_zero_on_first_attempt() {
        let retry = JobRetryMetadata {
            attempt: 0,
            ..Default::default()
        };
        assert_eq!(JobManager::deterministic_backoff_ms(&retry), 0);
    }

    #[test]
    fn deterministic_backoff_exponential_growth() {
        let base = DEFAULT_JOB_BACKOFF_BASE_MS;
        for attempt in 1..=5 {
            let retry = JobRetryMetadata {
                attempt,
                backoff_base_ms: base,
                ..Default::default()
            };
            let expected = base * 2u64.pow(attempt.saturating_sub(1).min(20));
            assert_eq!(
                JobManager::deterministic_backoff_ms(&retry),
                expected,
                "attempt {attempt}"
            );
        }
    }

    #[test]
    fn deterministic_backoff_saturates_at_high_exponent() {
        let retry = JobRetryMetadata {
            attempt: 63,
            backoff_base_ms: 1000,
            ..Default::default()
        };
        // Should not panic; result saturates
        let _ = JobManager::deterministic_backoff_ms(&retry);
    }

    // ── JobManager: history truncation ─────────────────────────────────

    #[test]
    fn push_history_truncates_beyond_max() {
        let mut jm = JobManager::default();
        let job = jm.enqueue("task");
        let id = job.id.clone();
        // Generate more history entries than the limit
        for i in 0..(MAX_JOB_HISTORY_ENTRIES + 20) {
            jm.update_progress(&id, (i % 100) as u8, Some(format!("step {i}")));
        }
        let history = jm.history(&id);
        assert_eq!(history.len(), MAX_JOB_HISTORY_ENTRIES);
    }

    // ── JobManager: persistence encoding/parsing ───────────────────────

    #[test]
    fn encode_and_parse_persisted_detail_round_trip() {
        let mut jm = JobManager::default();
        let job = jm.enqueue("task");
        let id = job.id.clone();
        jm.set_running(&id);
        jm.fail(&id, "oops");
        let job = jm.list().into_iter().find(|j| j.id == id).unwrap();

        let encoded = JobManager::encode_persisted_detail(&job).unwrap().unwrap();
        let parsed = JobManager::parse_persisted_detail(Some(&encoded)).unwrap();

        assert_eq!(parsed.status, job.status);
        assert_eq!(parsed.detail, job.detail);
        assert_eq!(parsed.retry.attempt, job.retry.attempt);
        assert_eq!(parsed.history.len(), job.history.len());
    }

    #[test]
    fn parse_persisted_detail_returns_none_for_none_input() {
        assert!(JobManager::parse_persisted_detail(None).is_none());
    }

    #[test]
    fn parse_persisted_detail_returns_none_for_invalid_json() {
        assert!(JobManager::parse_persisted_detail(Some("not json")).is_none());
    }

    // ── Helper functions ───────────────────────────────────────────────

    #[test]
    fn job_status_round_trip_str() {
        let statuses = [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Paused,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ];
        for status in &statuses {
            let s = job_status_to_str(*status);
            let parsed = job_status_from_str(s);
            assert_eq!(parsed, Some(*status), "round-trip failed for {s:?}");
        }
    }

    #[test]
    fn job_status_from_str_returns_none_for_unknown() {
        assert_eq!(job_status_from_str("unknown"), None);
        assert_eq!(job_status_from_str(""), None);
    }

    #[test]
    fn truncate_preview_limits_to_120_chars() {
        let long = "a".repeat(200);
        let truncated = truncate_preview(&long);
        assert_eq!(truncated.len(), 120);
    }

    #[test]
    fn truncate_preview_preserves_short_strings() {
        let short = "hello";
        assert_eq!(truncate_preview(short), "hello");
    }

    #[test]
    fn runtime_status_to_job_state_maps_correctly() {
        assert_eq!(
            runtime_status_to_job_state(JobStatus::Queued),
            JobStateStatus::Queued
        );
        assert_eq!(
            runtime_status_to_job_state(JobStatus::Running),
            JobStateStatus::Running
        );
        assert_eq!(
            runtime_status_to_job_state(JobStatus::Paused),
            JobStateStatus::Paused
        );
        assert_eq!(
            runtime_status_to_job_state(JobStatus::Completed),
            JobStateStatus::Completed
        );
        assert_eq!(
            runtime_status_to_job_state(JobStatus::Failed),
            JobStateStatus::Failed
        );
        assert_eq!(
            runtime_status_to_job_state(JobStatus::Cancelled),
            JobStateStatus::Cancelled
        );
    }

    #[test]
    fn job_state_status_to_runtime_maps_correctly() {
        assert_eq!(
            job_state_status_to_runtime(JobStateStatus::Queued),
            JobStatus::Queued
        );
        assert_eq!(
            job_state_status_to_runtime(JobStateStatus::Running),
            JobStatus::Running
        );
        assert_eq!(
            job_state_status_to_runtime(JobStateStatus::Completed),
            JobStatus::Completed
        );
        assert_eq!(
            job_state_status_to_runtime(JobStateStatus::Failed),
            JobStatus::Failed
        );
        assert_eq!(
            job_state_status_to_runtime(JobStateStatus::Cancelled),
            JobStatus::Cancelled
        );
    }

    #[test]
    fn preview_from_initial_history_new() {
        let preview = preview_from_initial_history(&InitialHistory::New);
        assert_eq!(preview, "New conversation");
    }

    #[test]
    fn preview_from_initial_history_forked() {
        let preview = preview_from_initial_history(&InitialHistory::Forked(vec![json!("hello")]));
        assert!(preview.contains("hello"));
    }

    #[test]
    fn preview_from_initial_history_resumed() {
        let preview = preview_from_initial_history(&InitialHistory::Resumed {
            conversation_id: "test".to_string(),
            history: vec![json!("world")],
            rollout_path: PathBuf::from("/tmp/test"),
        });
        assert!(preview.contains("world"));
    }

    #[test]
    fn json_optional_string_handles_null() {
        assert!(json_optional_string(&Value::Null).is_none());
    }

    #[test]
    fn json_optional_string_handles_string() {
        assert_eq!(
            json_optional_string(&Value::String("hello".to_string())),
            Some("hello".to_string())
        );
    }

    #[test]
    fn json_optional_string_handles_non_string() {
        assert!(json_optional_string(&json!(42)).is_none());
    }

    #[test]
    fn parse_retry_metadata_returns_default_for_none() {
        let retry = parse_retry_metadata(None);
        assert_eq!(retry.attempt, 0);
        assert_eq!(retry.max_attempts, DEFAULT_JOB_MAX_ATTEMPTS);
        assert_eq!(retry.backoff_base_ms, DEFAULT_JOB_BACKOFF_BASE_MS);
    }

    #[test]
    fn parse_retry_metadata_parses_fields() {
        let value = json!({
            "attempt": 2,
            "max_attempts": 5,
            "backoff_base_ms": 1000,
            "next_backoff_ms": 2000,
            "next_retry_at": 1234567890i64
        });
        let retry = parse_retry_metadata(Some(&value));
        assert_eq!(retry.attempt, 2);
        assert_eq!(retry.max_attempts, 5);
        assert_eq!(retry.backoff_base_ms, 1000);
        assert_eq!(retry.next_backoff_ms, 2000);
        assert_eq!(retry.next_retry_at, Some(1234567890));
    }

    #[test]
    fn parse_history_entry_returns_none_without_status() {
        let value = json!({"at": 1, "phase": "test"});
        assert!(parse_history_entry(&value).is_none());
    }

    #[test]
    fn parse_history_entry_parses_valid_entry() {
        let value = json!({
            "at": 100,
            "phase": "running",
            "status": "running",
            "progress": 50,
            "detail": "working",
            "retry": {"attempt": 0, "max_attempts": 3, "backoff_base_ms": 500}
        });
        let entry = parse_history_entry(&value).unwrap();
        assert_eq!(entry.at, 100);
        assert_eq!(entry.phase, "running");
        assert_eq!(entry.status, JobStatus::Running);
        assert_eq!(entry.progress, Some(50));
        assert_eq!(entry.detail.as_deref(), Some("working"));
    }

    // ── ThreadManager / persistence (scratch StateStore per test) ──────

    use codesmith_protocol::{LocalShellParams, ThreadStartParams};
    use codesmith_tools::ToolCallSource;

    fn scratch_state_store() -> (tempfile::TempDir, StateStore) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store =
            StateStore::open(Some(tmp.path().join("state.db"))).expect("open scratch state store");
        (tmp, store)
    }

    fn resume_params(thread_id: &str) -> ThreadResumeParams {
        ThreadResumeParams {
            thread_id: thread_id.to_string(),
            history: None,
            path: None,
            model: None,
            model_provider: None,
            cwd: None,
            approval_policy: None,
            sandbox: None,
            config: None,
            base_instructions: None,
            developer_instructions: None,
            personality: None,
            persist_extended_history: false,
        }
    }

    fn fork_params(thread_id: &str) -> ThreadForkParams {
        ThreadForkParams {
            thread_id: thread_id.to_string(),
            path: None,
            model: None,
            model_provider: None,
            cwd: None,
            approval_policy: None,
            sandbox: None,
            config: None,
            base_instructions: None,
            developer_instructions: None,
            persist_extended_history: false,
        }
    }

    #[test]
    fn spawn_persists_thread_readable_via_list_and_read() {
        let (_tmp, store) = scratch_state_store();
        let mut tm = ThreadManager::new(store);
        let new = tm
            .spawn_thread_with_history(
                "openai".to_string(),
                PathBuf::from("/tmp/ws"),
                InitialHistory::New,
                false,
            )
            .expect("spawn");
        assert_eq!(new.model, "auto");
        assert_eq!(new.thread.model_provider, "openai");
        assert_eq!(new.thread.status, ThreadStatus::Running);
        assert!(
            new.thread.ephemeral,
            "persist_extended_history=false → ephemeral thread"
        );

        let listed = tm
            .list_threads(&ThreadListParams {
                include_archived: false,
                limit: None,
            })
            .expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, new.thread.id);

        let read = tm
            .read_thread(&ThreadReadParams {
                thread_id: new.thread.id.clone(),
            })
            .expect("read")
            .expect("thread present");
        assert_eq!(read.preview, "New conversation");
    }

    #[test]
    fn spawn_with_forked_history_persists_items_and_preview() {
        let (_tmp, store) = scratch_state_store();
        let mut tm = ThreadManager::new(store);
        let history = vec![
            json!({"role": "user", "content": "hello fork"}),
            json!({"role": "assistant", "content": "hi"}),
        ];
        let new = tm
            .spawn_thread_with_history(
                "p".to_string(),
                PathBuf::from("/tmp/ws"),
                InitialHistory::Forked(history),
                true,
            )
            .expect("spawn");
        assert!(!new.thread.ephemeral);
        assert!(
            new.thread.preview.contains("hello fork"),
            "preview derives from the first forked item: {}",
            new.thread.preview
        );
        let messages = tm
            .state_store()
            .list_messages(&new.thread.id, None)
            .expect("list messages");
        assert_eq!(messages.len(), 2, "both forked items persisted");
        assert!(messages[0].content.contains("hello fork"));
    }

    #[test]
    fn resume_of_running_thread_without_history_returns_cached_entry() {
        let (_tmp, store) = scratch_state_store();
        let mut tm = ThreadManager::new(store);
        let spawned = tm
            .spawn_thread_with_history(
                "openai".to_string(),
                PathBuf::from("/tmp/ws"),
                InitialHistory::New,
                false,
            )
            .expect("spawn");
        let mut params = resume_params(&spawned.thread.id);
        params.model = Some("custom-model".to_string());
        params.cwd = Some(PathBuf::from("/tmp/override"));
        let resumed = tm
            .resume_thread_with_history(&params, Path::new("/fallback"), "fallback".to_string())
            .expect("resume")
            .expect("thread");
        assert_eq!(resumed.thread.id, spawned.thread.id);
        assert_eq!(resumed.model, "custom-model");
        assert_eq!(resumed.cwd, PathBuf::from("/tmp/override"));
    }

    #[test]
    fn resume_after_restart_loads_thread_from_store() {
        let (tmp, store) = scratch_state_store();
        let mut tm = ThreadManager::new(store);
        let spawned = tm
            .spawn_thread_with_history(
                "openai".to_string(),
                PathBuf::from("/tmp/ws"),
                InitialHistory::New,
                false,
            )
            .expect("spawn");
        tm.touch_message(&spawned.thread.id, "persisted prompt")
            .expect("touch");
        drop(tm);

        // A fresh manager on the same database simulates a process restart.
        let store2 = StateStore::open(Some(tmp.path().join("state.db"))).expect("reopen");
        let mut tm2 = ThreadManager::new(store2);
        let mut params = resume_params(&spawned.thread.id);
        params.cwd = Some(PathBuf::from("/tmp/restarted"));
        let resumed = tm2
            .resume_thread_with_history(&params, Path::new("/fallback"), "p".to_string())
            .expect("resume")
            .expect("thread loaded from store");
        assert_eq!(resumed.thread.id, spawned.thread.id);
        assert_eq!(resumed.cwd, PathBuf::from("/tmp/restarted"));
        assert_eq!(resumed.thread.status, ThreadStatus::Running);
        let messages = tm2
            .state_store()
            .list_messages(&spawned.thread.id, None)
            .expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "persisted prompt");
    }

    #[test]
    fn resume_unknown_thread_returns_none() {
        let (_tmp, store) = scratch_state_store();
        let mut tm = ThreadManager::new(store);
        let resumed = tm
            .resume_thread_with_history(
                &resume_params("thread-missing"),
                Path::new("/fallback"),
                "p".to_string(),
            )
            .expect("resume dispatch");
        assert!(resumed.is_none());
    }

    #[test]
    fn fork_thread_copies_history_and_appends_provenance_marker() {
        let (_tmp, store) = scratch_state_store();
        let mut tm = ThreadManager::new(store);
        let parent = tm
            .spawn_thread_with_history(
                "openai".to_string(),
                PathBuf::from("/tmp/ws"),
                InitialHistory::New,
                false,
            )
            .expect("spawn parent");
        // `touch_message` persists a user turn with `item = None`, the shape
        // fork must synthesize a history entry for.
        tm.touch_message(&parent.thread.id, "original question")
            .expect("touch");

        let forked = tm
            .fork_thread(&fork_params(&parent.thread.id), Path::new("/fallback"))
            .expect("fork")
            .expect("parent found");
        assert_ne!(forked.thread.id, parent.thread.id);
        assert_eq!(forked.thread.model_provider, "openai", "provider inherited");

        let messages = tm
            .state_store()
            .list_messages(&forked.thread.id, None)
            .expect("messages");
        assert_eq!(messages.len(), 2, "parent turn + fork marker");
        assert!(messages[0].content.contains("original question"));
        assert!(messages[1].content.contains("\"from_thread_id\""));
        assert!(
            forked.thread.preview.contains("original question"),
            "fork preview derives from the first real item, not the marker: {}",
            forked.thread.preview
        );
    }

    #[test]
    fn fork_unknown_thread_returns_none() {
        let (_tmp, store) = scratch_state_store();
        let mut tm = ThreadManager::new(store);
        let forked = tm
            .fork_thread(&fork_params("thread-missing"), Path::new("/fallback"))
            .expect("fork dispatch");
        assert!(forked.is_none());
    }

    #[test]
    fn set_thread_name_updates_metadata() {
        let (_tmp, store) = scratch_state_store();
        let mut tm = ThreadManager::new(store);
        let spawned = tm
            .spawn_thread_with_history(
                "openai".to_string(),
                PathBuf::from("/tmp/ws"),
                InitialHistory::New,
                false,
            )
            .expect("spawn");
        let updated = tm
            .set_thread_name(&ThreadSetNameParams {
                thread_id: spawned.thread.id.clone(),
                name: "my thread".to_string(),
            })
            .expect("set name")
            .expect("thread");
        assert_eq!(updated.name.as_deref(), Some("my thread"));

        let renamed = tm
            .set_thread_name(&ThreadSetNameParams {
                thread_id: "thread-missing".to_string(),
                name: "x".to_string(),
            })
            .expect("set name dispatch");
        assert!(renamed.is_none());
    }

    #[test]
    fn archive_and_unarchive_toggle_thread_listing() {
        let (_tmp, store) = scratch_state_store();
        let mut tm = ThreadManager::new(store);
        let spawned = tm
            .spawn_thread_with_history(
                "openai".to_string(),
                PathBuf::from("/tmp/ws"),
                InitialHistory::New,
                false,
            )
            .expect("spawn");

        tm.archive_thread(&spawned.thread.id).expect("archive");
        let active = tm
            .list_threads(&ThreadListParams {
                include_archived: false,
                limit: None,
            })
            .expect("list");
        assert!(active.is_empty(), "archived thread hidden by default");
        let with_archived = tm
            .list_threads(&ThreadListParams {
                include_archived: true,
                limit: None,
            })
            .expect("list");
        assert_eq!(with_archived.len(), 1);
        assert_eq!(with_archived[0].status, ThreadStatus::Archived);

        tm.unarchive_thread(&spawned.thread.id).expect("unarchive");
        let active = tm
            .list_threads(&ThreadListParams {
                include_archived: false,
                limit: None,
            })
            .expect("list");
        assert_eq!(active.len(), 1, "unarchived thread listed again");
    }

    #[test]
    fn touch_message_updates_preview_appends_user_message_and_checkpoint() {
        let (_tmp, store) = scratch_state_store();
        let mut tm = ThreadManager::new(store);
        let spawned = tm
            .spawn_thread_with_history(
                "openai".to_string(),
                PathBuf::from("/tmp/ws"),
                InitialHistory::New,
                false,
            )
            .expect("spawn");
        tm.touch_message(&spawned.thread.id, "hello world")
            .expect("touch");

        let read = tm
            .read_thread(&ThreadReadParams {
                thread_id: spawned.thread.id.clone(),
            })
            .expect("read")
            .expect("thread");
        assert_eq!(read.preview, "hello world");

        let messages = tm
            .state_store()
            .list_messages(&spawned.thread.id, None)
            .expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "hello world");

        let checkpoint = tm
            .state_store()
            .load_checkpoint(&spawned.thread.id, None)
            .expect("checkpoint")
            .expect("latest checkpoint written by touch_message");
        assert_eq!(checkpoint.state["reason"], "thread_message");
        assert_eq!(checkpoint.state["role"], "user");
    }

    #[test]
    fn touch_message_unknown_thread_fails_loud() {
        let (_tmp, store) = scratch_state_store();
        let mut tm = ThreadManager::new(store);
        let err = tm
            .touch_message("thread-missing", "input")
            .expect_err("unknown thread must fail, not acknowledge");
        assert!(
            err.to_string().contains("thread-missing"),
            "error names the missing thread"
        );
    }

    // ── JobManager: store round-trips ──────────────────────────────────

    #[test]
    fn persist_all_and_load_from_store_round_trip() {
        let (_tmp, store) = scratch_state_store();
        let mut jm = JobManager::default();
        let job = jm.enqueue("persisted-job");
        jm.set_running(&job.id);
        jm.fail(&job.id, "transient failure");
        jm.persist_all(&store).expect("persist all");

        let mut reloaded = JobManager::default();
        reloaded.load_from_store(&store).expect("load");
        let restored = reloaded
            .list()
            .into_iter()
            .find(|j| j.id == job.id)
            .expect("job reloaded");
        assert_eq!(restored.status, JobStatus::Failed);
        assert_eq!(restored.detail.as_deref(), Some("transient failure"));
        assert_eq!(
            restored.retry.attempt, 1,
            "retry schedule survives the round trip"
        );
        assert!(
            !restored.history.is_empty(),
            "history is encoded in the persisted detail"
        );
    }

    #[test]
    fn load_from_store_falls_back_when_detail_unparsable() {
        let (_tmp, store) = scratch_state_store();
        store
            .upsert_job(&JobStateRecord {
                id: "job-legacy".to_string(),
                name: "legacy".to_string(),
                status: JobStateStatus::Running,
                progress: Some(42),
                detail: Some("plain text, not json".to_string()),
                created_at: 1_000,
                updated_at: 2_000,
            })
            .expect("seed job");

        let mut jm = JobManager::default();
        jm.load_from_store(&store).expect("load");
        let job = jm
            .list()
            .into_iter()
            .find(|j| j.id == "job-legacy")
            .expect("legacy job loaded");
        assert_eq!(job.status, JobStatus::Running, "status from the column");
        assert_eq!(job.detail.as_deref(), Some("plain text, not json"));
        assert_eq!(job.retry.attempt, 0, "default retry when detail unparsable");
        assert!(job.history.is_empty());
    }

    #[test]
    fn persist_job_unknown_id_is_a_noop() {
        let (_tmp, store) = scratch_state_store();
        let jm = JobManager::default();
        jm.persist_job(&store, "job-missing").expect("noop ok");
    }

    // ── Runtime: construction + thread/job surface ─────────────────────

    fn test_runtime(store: StateStore) -> Runtime {
        Runtime::new(
            ConfigToml::default(),
            ModelRegistry::default(),
            store,
            Arc::new(McpManager::default()),
            ExecPolicyEngine::new(Vec::new(), Vec::new()),
            HookDispatcher::default(),
        )
    }

    fn runtime_with_scratch_store() -> (tempfile::TempDir, Runtime) {
        let (tmp, store) = scratch_state_store();
        let runtime = test_runtime(store);
        (tmp, runtime)
    }

    #[test]
    fn runtime_new_requeues_persisted_pending_jobs() {
        let (tmp, store) = scratch_state_store();
        {
            let mut runtime = test_runtime(store);
            let job = runtime.enqueue_job("stuck-job").expect("enqueue");
            runtime.set_job_running(&job.id).expect("running");
        }
        // A persisted Running job with no executor must come back Queued.
        let store2 = StateStore::open(Some(tmp.path().join("state.db"))).expect("reopen");
        let runtime = test_runtime(store2);
        let job = runtime
            .jobs
            .list()
            .into_iter()
            .find(|j| j.name == "stuck-job")
            .expect("persisted job reloaded");
        assert_eq!(job.status, JobStatus::Queued, "resume_pending on load");
    }

    #[test]
    fn runtime_job_wrappers_drive_manager_and_persist() {
        let (tmp, runtime) = runtime_with_scratch_store();
        let mut runtime = runtime;
        let job = runtime.enqueue_job("wrapper-job").expect("enqueue");
        runtime.set_job_running(&job.id).expect("running");
        runtime
            .update_job_progress(&job.id, 60, Some("step 1".to_string()))
            .expect("progress");
        runtime.complete_job(&job.id).expect("complete");
        drop(runtime);

        let store2 = StateStore::open(Some(tmp.path().join("state.db"))).expect("reopen");
        let runtime = test_runtime(store2);
        let job = runtime
            .jobs
            .list()
            .into_iter()
            .find(|j| j.id == job.id)
            .expect("wrapper job persisted");
        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.progress, Some(100));
        assert!(
            runtime
                .job_history(&job.id)
                .iter()
                .any(|e| e.phase == "progress_updated"),
            "history includes the progress transition"
        );
    }

    #[test]
    fn runtime_fail_cancel_pause_resume_jobs() {
        let (_tmp, mut runtime) = runtime_with_scratch_store();
        let failed = runtime.enqueue_job("f").expect("enqueue");
        runtime.fail_job(&failed.id, "boom").expect("fail");
        let paused = runtime.enqueue_job("p").expect("enqueue");
        runtime.set_job_running(&paused.id).expect("running");
        runtime.pause_job(&paused.id, None).expect("pause");
        runtime.resume_job(&paused.id, None).expect("resume");
        let cancelled = runtime.enqueue_job("c").expect("enqueue");
        runtime.cancel_job(&cancelled.id).expect("cancel");

        let by_id = |runtime: &Runtime, id: &str| {
            runtime
                .jobs
                .list()
                .into_iter()
                .find(|j| j.id == id)
                .expect("job")
        };
        assert_eq!(by_id(&runtime, &failed.id).status, JobStatus::Failed);
        assert_eq!(by_id(&runtime, &paused.id).status, JobStatus::Running);
        assert_eq!(by_id(&runtime, &cancelled.id).status, JobStatus::Cancelled);
    }

    #[test]
    fn app_status_reports_job_history_as_events() {
        let (_tmp, mut runtime) = runtime_with_scratch_store();
        let job = runtime.enqueue_job("status-job").expect("enqueue");
        runtime.complete_job(&job.id).expect("complete");

        let status = runtime.app_status();
        assert!(status.ok);
        assert_eq!(status.data["jobs"].as_array().expect("jobs array").len(), 1);
        assert_eq!(status.data["jobs"][0]["name"], "status-job");
        assert!(
            !status.events.is_empty(),
            "job transitions surface as response-delta events"
        );
    }

    #[test]
    fn provider_default_returns_configured_provider() {
        let (_tmp, runtime) = runtime_with_scratch_store();
        assert_eq!(runtime.provider_default(), ConfigToml::default().provider);
    }

    #[tokio::test]
    async fn save_and_load_thread_checkpoint_round_trip() {
        let (_tmp, runtime) = runtime_with_scratch_store();
        assert!(
            runtime
                .load_thread_checkpoint("thread-missing", None)
                .unwrap()
                .is_none(),
            "no checkpoint for an unknown thread"
        );
        // Checkpoints are FK-scoped to their thread: create one first.
        let created = runtime
            .handle_thread(ThreadRequest::Create {
                metadata: json!({}),
            })
            .await
            .expect("create");
        runtime
            .save_thread_checkpoint(&created.thread_id, "named", &json!({"step": 3}))
            .expect("save");
        let loaded = runtime
            .load_thread_checkpoint(&created.thread_id, Some("named"))
            .expect("load")
            .expect("checkpoint");
        assert_eq!(loaded["step"], 3);
    }

    #[tokio::test]
    async fn handle_thread_full_lifecycle() {
        let (_tmp, runtime) = runtime_with_scratch_store();

        let created = runtime
            .handle_thread(ThreadRequest::Create {
                metadata: json!({}),
            })
            .await
            .expect("create");
        assert_eq!(created.status, "created");
        let thread_id = created.thread_id.clone();
        assert!(
            created.data["history"].as_array().is_some(),
            "Create returns persisted thread data"
        );

        let accepted = runtime
            .handle_thread(ThreadRequest::Message {
                thread_id: thread_id.clone(),
                input: "hello runtime".to_string(),
            })
            .await
            .expect("message");
        assert_eq!(accepted.status, "accepted");
        assert_eq!(accepted.events.len(), 3);

        let read = runtime
            .handle_thread(ThreadRequest::Read(ThreadReadParams {
                thread_id: thread_id.clone(),
            }))
            .await
            .expect("read");
        let history = read.data["history"].as_array().expect("history").clone();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["role"], "user");

        let named = runtime
            .handle_thread(ThreadRequest::SetName(ThreadSetNameParams {
                thread_id: thread_id.clone(),
                name: "renamed".to_string(),
            }))
            .await
            .expect("set name");
        assert_eq!(
            named.thread.expect("thread").name.as_deref(),
            Some("renamed")
        );

        let forked = runtime
            .handle_thread(ThreadRequest::Fork(fork_params(&thread_id)))
            .await
            .expect("fork");
        assert_eq!(forked.status, "forked");
        assert_ne!(forked.thread_id, thread_id);

        let archived = runtime
            .handle_thread(ThreadRequest::Archive {
                thread_id: thread_id.clone(),
            })
            .await
            .expect("archive");
        assert_eq!(archived.status, "archived");

        let listed = runtime
            .handle_thread(ThreadRequest::List(ThreadListParams {
                include_archived: false,
                limit: None,
            }))
            .await
            .expect("list");
        assert_eq!(listed.threads.len(), 1, "only the fork remains active");

        let unarchived = runtime
            .handle_thread(ThreadRequest::Unarchive {
                thread_id: thread_id.clone(),
            })
            .await
            .expect("unarchive");
        assert_eq!(unarchived.status, "unarchived");

        let resumed = runtime
            .handle_thread(ThreadRequest::Resume(resume_params(&thread_id)))
            .await
            .expect("resume");
        assert_eq!(resumed.status, "resumed");
        assert_eq!(resumed.thread.expect("thread").id, thread_id);
    }

    #[tokio::test]
    async fn handle_thread_resume_and_fork_missing_report_missing() {
        let (_tmp, runtime) = runtime_with_scratch_store();
        let resumed = runtime
            .handle_thread(ThreadRequest::Resume(resume_params("thread-missing")))
            .await
            .expect("resume dispatch");
        assert_eq!(resumed.status, "missing");
        assert_eq!(resumed.data["error"], "thread not found");

        let forked = runtime
            .handle_thread(ThreadRequest::Fork(fork_params("thread-missing")))
            .await
            .expect("fork dispatch");
        assert_eq!(forked.status, "missing");
    }

    #[tokio::test]
    async fn handle_thread_start_uses_explicit_cwd_and_provider() {
        let (_tmp, runtime) = runtime_with_scratch_store();
        let started = runtime
            .handle_thread(ThreadRequest::Start(ThreadStartParams {
                model: None,
                model_provider: Some("anthropic".to_string()),
                cwd: Some(PathBuf::from("/tmp/explicit")),
                persist_extended_history: true,
            }))
            .await
            .expect("start");
        assert_eq!(started.status, "started");
        let thread = started.thread.expect("thread");
        assert_eq!(thread.model_provider, "anthropic");
        assert_eq!(thread.cwd, PathBuf::from("/tmp/explicit"));
        assert!(!thread.ephemeral, "persist_extended_history honored");
    }

    #[tokio::test]
    async fn invoke_tool_denied_by_prefix_reports_denied() {
        let (_tmp, store) = scratch_state_store();
        let runtime = Runtime::new(
            ConfigToml::default(),
            ModelRegistry::default(),
            store,
            Arc::new(McpManager::default()),
            ExecPolicyEngine::new(Vec::new(), vec!["denied-cmd".to_string()]),
            HookDispatcher::default(),
        );
        let call = ToolCall {
            name: "exec_shell".to_string(),
            payload: ToolPayload::LocalShell {
                params: LocalShellParams {
                    command: "denied-cmd --flag".to_string(),
                    cwd: None,
                    timeout_ms: None,
                },
            },
            source: ToolCallSource::Direct,
            raw_tool_call_id: None,
        };
        let result = runtime
            .invoke_tool(call, AskForApproval::OnRequest, Path::new("/tmp"))
            .await
            .expect("invoke dispatches");
        assert_eq!(result["ok"], false);
        assert_eq!(result["status"], "denied");
        assert!(
            result["error"]
                .as_str()
                .expect("error string")
                .contains("denied prefix"),
            "denial reason surfaces: {result}"
        );
    }

    #[tokio::test]
    async fn invoke_tool_policy_allowed_call_fails_loud_without_executor() {
        let (_tmp, runtime) = runtime_with_scratch_store();
        let call = ToolCall {
            name: "no_such_tool".to_string(),
            payload: ToolPayload::Function {
                arguments: "{}".to_string(),
            },
            source: ToolCallSource::Direct,
            raw_tool_call_id: None,
        };
        // OnFailure allows the dispatch (approval comes after a failure), so
        // the call reaches the no-executor failure — the arm under test.
        // OnRequest would stop at "approval_required".
        let result = runtime
            .invoke_tool(call, AskForApproval::OnFailure, Path::new("/tmp"))
            .await
            .expect("invoke_tool itself must not hard-error");
        assert_eq!(result["ok"], false);
        assert_eq!(result["status"], "failed");
        assert!(
            result["error"]
                .as_str()
                .expect("error string")
                .contains("no tool executor"),
            "the failure names the missing executor: {result}"
        );
    }

    #[tokio::test]
    async fn mcp_startup_with_no_servers_completes_empty() {
        let (_tmp, runtime) = runtime_with_scratch_store();
        let summary = runtime.mcp_startup().await;
        assert!(summary.ready.is_empty());
        assert!(summary.failed.is_empty());
        assert!(summary.cancelled.is_empty());
    }

    #[tokio::test]
    async fn invoke_tool_untrusted_command_requests_approval() {
        let (_tmp, runtime) = runtime_with_scratch_store();
        let call = ToolCall {
            name: "exec_shell".to_string(),
            payload: ToolPayload::LocalShell {
                params: LocalShellParams {
                    command: "cargo build".to_string(),
                    cwd: None,
                    timeout_ms: None,
                },
            },
            source: ToolCallSource::Direct,
            raw_tool_call_id: None,
        };
        // No trusted prefixes and OnRequest: the policy stops the call at the
        // approval gate — nothing is dispatched, an approval id is returned.
        let result = runtime
            .invoke_tool(call, AskForApproval::OnRequest, Path::new("/tmp"))
            .await
            .expect("invoke dispatches");
        assert_eq!(result["ok"], false);
        assert_eq!(result["status"], "approval_required");
        assert!(
            result["approval_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
            "an approval id names the pending decision: {result}"
        );
    }
}
