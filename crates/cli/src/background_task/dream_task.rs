//! Dream (memory consolidation) background task runner.
//!
//! Mirrors Claude Code's `DreamTask.ts`: runs memory consolidation
//! in the background, surfacing it in the task registry.
#![allow(dead_code)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Status of a dream task run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DreamStatus {
    Running,
    Completed,
    Failed,
}

/// Result of a dream task execution.
#[derive(Debug, Clone)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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
    #[allow(dead_code)]
    pub fn new(memory_dir: PathBuf, model: String, max_rounds: u32) -> Self {
        Self {
            memory_dir,
            model,
            max_rounds,
        }
    }

    /// Get the memory directory path.
    #[allow(dead_code)]
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

    /// Mark a dream as completed, persisting the consolidated memory.
    /// Returns the path where it was written. Failure to write surfaces as
    /// a Failed result — the old body fabricated `Completed` with a path
    /// that never existed (silent data loss behind a success signal).
    pub fn complete_dream(&self, consolidation_summary: &str) -> DreamResult {
        let memory_path = self.memory_dir.join("dream_consolidation.md");
        let write = std::fs::create_dir_all(&self.memory_dir)
            .and_then(|()| std::fs::write(&memory_path, consolidation_summary));
        match write {
            Ok(()) => DreamResult {
                status: DreamStatus::Completed,
                summary: Some(consolidation_summary.to_string()),
                memory_path: Some(memory_path),
                error: None,
                rounds_completed: 0,
            },
            Err(err) => DreamResult {
                status: DreamStatus::Failed,
                summary: Some(consolidation_summary.to_string()),
                memory_path: None,
                error: Some(format!("failed to write {}: {err}", memory_path.display())),
                rounds_completed: 0,
            },
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
