//! Portable automation data types shared between the CLI `AutomationManager`
//! and the `AutomationManagerHost` trait in [`crate::host_services`].
//!
//! These are pure serde data with no terminal-coupled dependencies, so they
//! live in the runtime crate to keep `spec.rs` / `tool-impls` free of the
//! concrete `AutomationManager` (C4-3b.5a). The schedule-parsing logic
//! (`AutomationSchedule`, `parse_rrule`, `next_after`) is scheduler-internal
//! and stays in the CLI crate.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const CURRENT_AUTOMATION_SCHEMA_VERSION: u32 = 1;
pub const CURRENT_RUN_SCHEMA_VERSION: u32 = 1;

pub const fn default_automation_schema_version() -> u32 {
    CURRENT_AUTOMATION_SCHEMA_VERSION
}

pub const fn default_run_schema_version() -> u32 {
    CURRENT_RUN_SCHEMA_VERSION
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutomationStatus {
    Active,
    // Forward-compat fallback: an unknown status in a persisted (possibly
    // newer-schema) file deserializes as Paused so it never auto-executes.
    #[serde(other)]
    Paused,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunStatus {
    Queued,
    Running,
    Completed,
    Canceled,
    // Forward-compat fallback: an unknown run status deserializes as a
    // terminal Failed so reconcile logic never retries it. (Serde requires
    // the `other` variant to be last.)
    #[serde(other)]
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationRecord {
    #[serde(default = "default_automation_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub rrule: String,
    #[serde(default)]
    pub cwds: Vec<PathBuf>,
    pub status: AutomationStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationRunRecord {
    #[serde(default = "default_run_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub automation_id: String,
    pub scheduled_for: DateTime<Utc>,
    pub status: AutomationRunStatus,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAutomationRequest {
    pub name: String,
    pub prompt: String,
    pub rrule: String,
    /// Working directories for runs. Raw, unvalidated paths at this shared
    /// contract boundary — a hand-edited persisted `AutomationRecord`
    /// deserializes the same field, so validation cannot live only at the
    /// tool-input edge. Consumers must canonicalize each entry and verify
    /// it exists (and is within allowed roots, where the deployment has
    /// them) before using it as a task workspace; `allow_shell: false`
    /// downstream mitigates but does not remove the exposure (file
    /// reads/writes still land in the chosen directory).
    #[serde(default)]
    pub cwds: Vec<PathBuf>,
    #[serde(default)]
    pub status: Option<AutomationStatus>,
}

/// Patch semantics: every field is optional; `Some` means "set/replace".
/// Empty strings for `name`/`prompt` are rejected by the receivers;
/// `Some(Vec::new())` for `cwds` clears the list (an empty cwd list means
/// runs default to workspace `None`); an all-`None` request is rejected by
/// the canonical receiver (`AutomationManager::update_automation`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateAutomationRequest {
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub rrule: Option<String>,
    pub cwds: Option<Vec<PathBuf>>,
    pub status: Option<AutomationStatus>,
}
