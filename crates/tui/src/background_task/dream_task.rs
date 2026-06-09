//! Dream (memory consolidation) background task runner.
//!
//! Mirrors Claude Code's `DreamTask.ts`: runs memory consolidation
//! in the background, surfacing it in the task registry.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Status of a dream task run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DreamStatus {
    Running,
    Completed,
    Failed,
}

/// Result of a dream task execution.
#[derive(Debug, Clone)]
pub struct DreamResult {
    pub status: DreamStatus,
    pub summary: Option<String>,
    pub memory_path: Option<PathBuf>,
    pub error: Option<String>,
    pub rounds_completed: u32,
}

/// Dream task runner — reads recent session context, generates a
/// consolidation summary, and writes it to the KoD memory directory.
///
/// This is a placeholder for the full implementation that will
/// integrate with the KoD (Knowledge on Demand) system and the
/// DeepSeek client for generating summaries.
pub struct DreamTaskRunner {
    /// Path to the KoD memory directory.
    memory_dir: PathBuf,
    /// Model to use for consolidation (typically a fast/cheap model).
    model: String,
    /// Maximum consolidation rounds per dream session.
    max_rounds: u32,
}

impl DreamTaskRunner {
    /// Create a new dream task runner.
    pub fn new(memory_dir: PathBuf, model: String, max_rounds: u32) -> Self {
        Self { memory_dir, model, max_rounds }
    }

    /// Get the memory directory path.
    pub fn memory_dir(&self) -> &PathBuf {
        &self.memory_dir
    }

    /// Get the model name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Get the max rounds.
    pub fn max_rounds(&self) -> u32 {
        self.max_rounds
    }

    /// Mark a dream as completed. Returns the path where the
    /// consolidated memory was written.
    pub fn complete_dream(&self, consolidation_summary: &str) -> DreamResult {
        let memory_path = self.memory_dir.join("dream_consolidation.md");
        DreamResult {
            status: DreamStatus::Completed,
            summary: Some(consolidation_summary.to_string()),
            memory_path: Some(memory_path),
            error: None,
            rounds_completed: 0,
        }
    }

    /// Mark a dream as failed and record the error.
    pub fn fail_dream(&self, error: &str) -> DreamResult {
        DreamResult {
            status: DreamStatus::Failed,
            summary: None,
            memory_path: None,
            error: Some(error.to_string()),
            rounds_completed: 0,
        }
    }
}