//! Async prefetch orchestrator for Knowledge On Demand.
//!
//! Coordinates the full prefetch pipeline: scan directory → rank by
//! relevance → read selected files → truncate + staleness → return
//! surfaced memories ready for context injection.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::pin::Pin;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::age::{memory_age_label, memory_freshness_text};
use super::budget::{SessionByteBudget, MAX_BYTES_PER_MEMORY, MAX_LINES_PER_MEMORY, MAX_MEMORIES_PER_TURN};
use super::dedup::filter_duplicate_attachments;
use super::entrypoint::load_entrypoint;
use super::paths::{ensure_memory_dir_exists, resolve_memory_dir, resolve_memory_entrypoint};
use super::relevance::{select_relevant_memories, RelevanceError};
use super::scan::{scan_memory_files, MemoryHeader};
use super::types::SurfacedMemory;

/// Orchestrates async prefetch of relevant memories each turn.
///
/// Holds a `JoinHandle` to the spawned prefetch task, tracks already-surfaced
/// paths for dedup across turns, and maintains a session-wide byte budget.
pub struct KnowledgePrefetch {
    /// Paths already surfaced in prior turns this session (for dedup).
    already_surfaced: Arc<Mutex<HashSet<PathBuf>>>,
    /// Session-wide byte budget tracker.
    session_budget: Arc<Mutex<SessionByteBudget>>,
    /// JoinHandle for the current turn's prefetch task, if one is running.
    /// Collected via `take_prefetch_handle()` before injecting surfaced
    /// memories into the context.
    prefetch_handle: Option<tokio::task::JoinHandle<PrefetchResult>>,
}

impl KnowledgePrefetch {
    /// Create a new prefetch orchestrator with fresh state.
    pub fn new() -> Self {
        Self {
            already_surfaced: Arc::new(Mutex::new(HashSet::new())),
            session_budget: Arc::new(Mutex::new(SessionByteBudget::new())),
            prefetch_handle: None,
        }
    }

    /// Get the already-surfaced paths set (for passing to prefetch).
    pub fn already_surfaced_paths(&self) -> Arc<Mutex<HashSet<PathBuf>>> {
        self.already_surfaced.clone()
    }

    /// Get the session budget (for passing to prefetch and for budget enforcement).
    pub fn session_budget(&self) -> Arc<Mutex<SessionByteBudget>> {
        self.session_budget.clone()
    }

    /// Store a spawned prefetch task handle for later collection.
    pub fn set_prefetch_handle(&mut self, handle: tokio::task::JoinHandle<PrefetchResult>) {
        self.prefetch_handle = Some(handle);
    }

    /// Take the prefetch handle, clearing it from the struct.
    /// Returns `None` if no prefetch was spawned or it was already collected.
    pub fn take_prefetch_handle(&mut self) -> Option<tokio::task::JoinHandle<PrefetchResult>> {
        self.prefetch_handle.take()
    }

    /// Update tracking state after surfaced memories are injected.
    pub async fn mark_surfaced(&self, memories: &[SurfacedMemory]) {
        let mut surfaced = self.already_surfaced.lock().await;
        let mut budget = self.session_budget.lock().await;
        for mem in memories {
            surfaced.insert(mem.path.clone());
            budget.consume(mem.byte_count);
        }
    }
}

impl Default for KnowledgePrefetch {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a prefetch operation.
#[derive(Debug, Clone)]
pub struct PrefetchResult {
    /// The surfaced memories ready for context injection.
    pub surfaced: Vec<SurfacedMemory>,
    /// Memory headers scanned from the directory (for diagnostics).
    pub scan_headers: Vec<MemoryHeader>,
    /// Duration of the prefetch in milliseconds.
    pub duration_ms: u64,
}

/// Run the full prefetch pipeline: scan → rank → read → format.
///
/// This is the async function that gets spawned as a tokio task.
/// The `side_query_fn` parameter abstracts the DeepSeek API call
/// so this module stays decoupled from the specific client.
pub async fn run_prefetch(
    user_query: &str,
    memory_dir: &Path,
    already_surfaced: Arc<Mutex<HashSet<PathBuf>>>,
    session_budget: Arc<Mutex<SessionByteBudget>>,
    cancel_token: CancellationToken,
    recent_tools: &[String],
    side_query_fn: impl FnOnce(String, String) -> Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send,
) -> Result<PrefetchResult, RelevanceError> {
    let started = std::time::Instant::now();

    // Ensure directory exists (may have been created by RememberTool).
    if let Err(e) = ensure_memory_dir_exists(memory_dir) {
        // Directory creation failure is non-critical for prefetch.
        // Just return empty result.
        return Ok(PrefetchResult {
            surfaced: vec![],
            scan_headers: vec![],
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }

    // 1. Scan memory files.
    let headers = scan_memory_files(memory_dir);
    if headers.is_empty() {
        return Ok(PrefetchResult {
            surfaced: vec![],
            scan_headers: headers,
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }

    // Filter out already-surfaced paths.
    let surfaced_set = already_surfaced.lock().await;
    let unsurfaced_headers: Vec<MemoryHeader> = headers
        .iter()
        .filter(|h| !surfaced_set.contains(&h.file_path))
        .cloned()
        .collect();
    drop(surfaced_set);

    if unsurfaced_headers.is_empty() {
        return Ok(PrefetchResult {
            surfaced: vec![],
            scan_headers: headers,
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }

    // 2. Rank by relevance via side-query.
    let selected_filenames = select_relevant_memories(
        user_query,
        &unsurfaced_headers,
        recent_tools,
        side_query_fn,
        cancel_token.clone(),
    )
    .await?;

    if selected_filenames.is_empty() {
        return Ok(PrefetchResult {
            surfaced: vec![],
            scan_headers: headers,
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }

    // 3. Read selected memory files with truncation and staleness headers.
    let selected_headers: Vec<&MemoryHeader> = unsurfaced_headers
        .iter()
        .filter(|h| selected_filenames.contains(&h.filename))
        .collect();

    let mut surfaced_memories = Vec::new();
    for header in selected_headers.iter().take(MAX_MEMORIES_PER_TURN) {
        if let Some(mem) = read_memory_for_surfacing(header, memory_dir) {
            surfaced_memories.push(mem);
        }
    }

    // 4. Enforce session byte budget.
    let budget = session_budget.lock().await;
    let mut budgeted = Vec::new();
    let mut remaining = budget.remaining();
    for mem in surfaced_memories {
        if remaining >= mem.byte_count {
            remaining -= mem.byte_count;
            budgeted.push(mem);
        } else {
            break; // Budget exhausted.
        }
    }
    drop(budget);

    Ok(PrefetchResult {
        surfaced: budgeted,
        scan_headers: headers,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Read a memory file and format it for surfacing.
///
/// Applies line/byte truncation and staleness headers.
fn read_memory_for_surfacing(header: &MemoryHeader, memory_dir: &Path) -> Option<SurfacedMemory> {
    let content = std::fs::read_to_string(&header.file_path).ok()?;
    let (fm, body) = super::scan::parse_frontmatter(&content);

    // Truncate body to line limit.
    let mut lines: Vec<&str> = body.lines().collect();
    let was_truncated = lines.len() > MAX_LINES_PER_MEMORY;
    if was_truncated {
        lines.truncate(MAX_LINES_PER_MEMORY);
    }

    let mut truncated_body = lines.join("\n");

    // Truncate body to byte limit.
    if truncated_body.len() > MAX_BYTES_PER_MEMORY {
        let cutoff = previous_char_boundary(&truncated_body, MAX_BYTES_PER_MEMORY);
        truncated_body = truncated_body[..cutoff].to_string();
        truncated_body.push_str("\n[truncated: content exceeds memory budget]");
    }

    // Build staleness header.
    let age_label = memory_age_label(header.mtime_ms);
    let staleness_header = format!(
        "[Memory: {}, last modified {}]",
        header.filename, age_label
    );

    // Build full content with staleness warning.
    let freshness_warning = memory_freshness_text(header.mtime_ms);
    let full_content = if freshness_warning.is_empty() {
        format!("{}\n{}", staleness_header, truncated_body)
    } else {
        format!("{}\n{}\n{}", staleness_header, freshness_warning, truncated_body)
    };

    let byte_count = full_content.len();

    // Drop unused frontmatter fields.
    drop(fm);

    Some(SurfacedMemory {
        path: header.file_path.clone(),
        staleness_header,
        content: full_content,
        was_truncated,
        byte_count,
    })
}

fn previous_char_boundary(s: &str, mut idx: usize) -> usize {
    while !s.is_char_boundary(idx) && idx > 0 {
        idx -= 1;
    }
    idx
}

/// Format surfaced memories into a `<system-reminder>` block
/// ready for injection into the context.
pub fn format_surfaced_memories(memories: &[SurfacedMemory]) -> String {
    let mut output = String::from("Relevant memories surfaced for this turn:");
    for mem in memories {
        output.push_str("\n\n");
        output.push_str(&mem.content);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn read_memory_for_surfacing_basic() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        let file_path = dir.join("role.md");

        let now_ms = chrono::Utc::now().timestamp_millis();
        std::fs::write(
            &file_path,
            "---\nname: role\ntype: user\n---\nI am a developer.",
        )
        .unwrap();

        // Set mtime to now.
        let header = MemoryHeader {
            filename: "role.md".to_string(),
            file_path: file_path.clone(),
            mtime_ms: now_ms,
            description: Some("role".to_string()),
            memory_type: Some(super::super::types::MemoryType::User),
        };

        let mem = read_memory_for_surfacing(&header, dir).unwrap();
        assert!(mem.content.contains("[Memory: role.md, last modified today]"));
        assert!(mem.content.contains("I am a developer."));
        assert!(mem.staleness_header.contains("role.md"));
        assert!(!mem.was_truncated);
    }

    #[test]
    fn read_memory_for_surfacing_truncates_long_content() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        let file_path = dir.join("long.md");

        let long_content = (0..100).map(|i| format!("Line {i}")).collect::<Vec<_>>().join("\n");
        let full = format!("---\n---\n{long_content}");
        std::fs::write(&file_path, &full).unwrap();

        let now_ms = chrono::Utc::now().timestamp_millis();
        let header = MemoryHeader {
            filename: "long.md".to_string(),
            file_path: file_path.clone(),
            mtime_ms: now_ms,
            description: None,
            memory_type: None,
        };

        let mem = read_memory_for_surfacing(&header, dir).unwrap();
        assert!(mem.was_truncated);
        assert!(mem.content.lines().count() <= MAX_LINES_PER_MEMORY + 3);
        // +3 for staleness header line + possible truncation marker line
    }

    #[test]
    fn format_surfaced_memories_produces_output() {
        let mem = SurfacedMemory {
            path: PathBuf::from("/tmp/role.md"),
            staleness_header: "[Memory: role.md, last modified today]".to_string(),
            content: "[Memory: role.md, last modified today]\nI am a dev.".to_string(),
            was_truncated: false,
            byte_count: 50,
        };
        let output = format_surfaced_memories(&[mem]);
        assert!(output.starts_with("Relevant memories surfaced for this turn:"));
        assert!(output.contains("role.md"));
    }

    #[tokio::test]
    async fn prefetch_returns_empty_for_empty_dir() {
        let tmp = tempdir().unwrap();
        let budget = Arc::new(Mutex::new(SessionByteBudget::new()));
        let surfaced = Arc::new(Mutex::new(HashSet::new()));
        let cancel = CancellationToken::new();

        let result = run_prefetch(
            "test query",
            tmp.path(),
            surfaced,
            budget,
            cancel,
            &[],
            |_, _| Box::pin(async { Ok(String::from("{}")) }),
        )
        .await
        .unwrap();

        assert!(result.surfaced.is_empty());
        assert!(result.scan_headers.is_empty());
    }
}