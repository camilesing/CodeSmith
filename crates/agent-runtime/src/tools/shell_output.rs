//! Output truncation and summarization helpers for shell tools.
//!
//! Terminal-agnostic pure-string processing: no `crossterm`, no terminal
//! handles, no TUI state. Lives in the runtime crate so both the engine body
//! and downstream tool implementations can call `truncate_with_meta` /
//! `summarize_output` without depending on the `codesmith-tui` binary.
//!
//! P0-2: truncation no longer silently discards the elided middle. Callers
//! that feed a model-visible tool result use [`spill_full_shell_output`] to
//! persist the FULL pre-truncation output when [`TruncationMeta::truncated`]
//! is set, and append [`spillover_footer`] so the model knows how to read it
//! back via `retrieve_tool_result`.

use std::path::PathBuf;

/// Maximum output size before truncation (30KB like Claude Code).
const MAX_OUTPUT_SIZE: usize = 30_000;
/// Head bytes preserved for large shell/test output. The matching tail budget
/// keeps final errors and test summaries visible without a second command.
const TRUNCATED_HEAD_BYTES: usize = 22_000;
const TRUNCATED_TAIL_BYTES: usize = MAX_OUTPUT_SIZE - TRUNCATED_HEAD_BYTES;
/// Limits for summary strings in tool metadata.
const SUMMARY_MAX_LINES: usize = 3;
const SUMMARY_MAX_CHARS: usize = 240;
/// Maximum number of preserved high-signal lines extracted from the tail
/// when output is truncated (#242). Bounded so the preserved summary
/// itself can never blow up the context window.
const MAX_PRESERVED_SUMMARY_LINES: usize = 80;

#[derive(Debug, Clone, Copy, Default)]
pub struct TruncationMeta {
    pub original_len: usize,
    pub omitted: usize,
    pub truncated: bool,
}

/// A full-output spill created by [`spill_full_shell_output`] when
/// truncation would otherwise permanently discard the elided middle.
#[derive(Debug, Clone)]
pub struct ShellSpillInfo {
    /// Reference id for `retrieve_tool_result ref=<ref_id>` — same string
    /// used as the spillover filename stem.
    pub ref_id: String,
    /// Absolute path of the spilled file, for result metadata and the UI.
    pub path: PathBuf,
    /// Total bytes of the combined spilled view.
    pub total_bytes: usize,
}

/// Whether a stream pair's FULL sizes warrant a spill: either stream alone
/// exceeding the truncation budget ([`MAX_OUTPUT_SIZE`]) means truncation
/// discarded bytes on the combined model-visible view. Based on totals (not
/// the per-call truncated flags) so polling paths whose accumulated slice is
/// still small still spill when the task's full output is already large.
#[must_use]
pub fn needs_spill(stdout_total: usize, stderr_total: usize) -> bool {
    stdout_total > MAX_OUTPUT_SIZE || stderr_total > MAX_OUTPUT_SIZE
}

/// Persist the FULL pre-truncation shell output (stdout and stderr combined
/// the same way the tool result renders them) to the spillover store, so the
/// bytes truncation elides stay retrievable via `retrieve_tool_result`
/// instead of being lost (P0-2 — the mirror article's "spill, don't
/// discard"). Call this only when truncation actually happened
/// ([`TruncationMeta::truncated`]). Disk/IO failure degrades to `None`
/// (logged via `tracing::warn!`) — a spillover hiccup must never fail the
/// tool call; the model then simply sees the old truncated view with no
/// footer.
#[must_use]
pub fn spill_full_shell_output(
    spill_id: &str,
    stdout: &str,
    stderr: &str,
) -> Option<ShellSpillInfo> {
    // Borrow stdout when stderr is empty (the common case) instead of
    // duplicating a potentially very large buffer — this runs on every poll
    // of a large-output task.
    let combined: std::borrow::Cow<'_, str> = if stderr.is_empty() {
        std::borrow::Cow::Borrowed(stdout)
    } else {
        std::borrow::Cow::Owned(format!("{stdout}\n\nSTDERR:\n{stderr}"))
    };
    let total_bytes = combined.len();
    let path = match super::truncate::write_spillover(spill_id, &combined) {
        Ok(path) => path,
        Err(err) => {
            // Degrade to `None` (a spillover hiccup must never fail the tool
            // call), but leave a trace — the truncated view's footer promises
            // the full output is on disk, and a silent write failure would
            // break that promise invisibly (same posture as
            // `apply_spillover_inner` in truncate.rs).
            tracing::warn!(
                target: "spillover",
                ?err,
                spill_id,
                "shell spill write failed; keeping truncated view without retrieval footer"
            );
            return None;
        }
    };
    Some(ShellSpillInfo {
        ref_id: spill_id.to_string(),
        path,
        total_bytes,
    })
}

/// Footer appended to a truncated tool result whose full output was spilled,
/// pointing the model at the retrieval tool. Byte-stable format — it becomes
/// part of the model-visible transcript.
#[must_use]
pub fn spillover_footer(info: &ShellSpillInfo) -> String {
    format!(
        "\n\n[Full output saved: {total} bytes. Read the elided middle with retrieve_tool_result ref={ref} mode=lines start_line=<n> end_line=<n>, or mode=query query=<text>.]",
        total = info.total_bytes,
        ref = info.ref_id,
    )
}

pub fn truncate_with_meta(output: &str) -> (String, TruncationMeta) {
    let original_len = output.len();
    if original_len <= MAX_OUTPUT_SIZE {
        return (
            output.to_string(),
            TruncationMeta {
                original_len,
                omitted: 0,
                truncated: false,
            },
        );
    }

    let head_end = char_boundary_at_or_before(output, TRUNCATED_HEAD_BYTES);
    let tail_start =
        char_boundary_at_or_after(output, original_len.saturating_sub(TRUNCATED_TAIL_BYTES));
    let head = &output[..head_end];
    let omitted_middle = &output[head_end..tail_start];
    let tail = &output[tail_start..];
    let omitted = omitted_middle.len();
    let note = format!(
        "...\n\n[Output truncated: showing first {head_bytes} bytes and last {tail_bytes} bytes. {omitted} bytes omitted.]",
        head_bytes = head.len(),
        tail_bytes = tail.len(),
    );

    // Preserve high-signal summary lines from the omitted middle (cargo test
    // results, rustc errors, panics, completion markers). The raw tail is
    // already included below; these snippets keep earlier failures visible
    // without re-running `cargo test | tail` repeatedly (#242/#1450).
    let mut combined = format!("{head}{note}");
    let preserved = collect_summary_lines(omitted_middle);
    if !preserved.is_empty() {
        combined.push_str("\n\n[Preserved summary lines from omitted middle]\n");
        combined.push_str(&preserved.join("\n"));
    }
    combined.push_str("\n\n[Output tail]\n");
    combined.push_str(tail);

    (
        combined,
        TruncationMeta {
            original_len,
            omitted,
            truncated: true,
        },
    )
}

/// Extract high-signal summary lines from a chunk of output that would
/// otherwise be discarded by truncation. Recognises Cargo/rustc output,
/// generic test framework summaries, panic markers, exit-status lines,
/// and `Finished`/`running ...` markers. Returns at most
/// `MAX_PRESERVED_SUMMARY_LINES` lines, oldest-first within each match
/// class so the most actionable signal is at the end.
pub fn collect_summary_lines(text: &str) -> Vec<String> {
    let mut preserved: Vec<String> = Vec::new();
    for line in text.lines() {
        if preserved.len() >= MAX_PRESERVED_SUMMARY_LINES {
            break;
        }
        if is_summary_line(line) {
            preserved.push(line.to_string());
        }
    }
    preserved
}

/// Heuristics for "this line is worth preserving even when most of the
/// output is dropped." Tuned for Cargo/rustc and generic test runner
/// vocabulary. Intentionally conservative: false positives only cost a
/// handful of bytes; false negatives force the agent to re-run gates.
fn is_summary_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    // Cargo / rustc canonical markers. Note `trim_start` already stripped
    // any leading whitespace, so match the bare word — the indentation
    // Cargo prints (e.g. "    Finished") would never reach this point.
    if trimmed.starts_with("test result:")
        || trimmed.starts_with("failures:")
        || trimmed.starts_with("FAILED")
        || trimmed.starts_with("error[")
        || trimmed.starts_with("error:")
        || trimmed.starts_with("warning:")
        || trimmed.starts_with("panicked at")
        || trimmed.starts_with("note:")
        || trimmed.starts_with("help:")
        || trimmed.starts_with("Finished")
        || trimmed.starts_with("Compiling")
        || trimmed.starts_with("Building")
        || trimmed.starts_with("Running")
        || trimmed.starts_with("running ")
        || trimmed.starts_with("Doc-tests")
        || trimmed.starts_with("---- ")
    {
        return true;
    }
    // Generic test runner vocabulary.
    if trimmed.contains("PASS") || trimmed.contains("FAIL") || trimmed.contains("ASSERT") {
        return true;
    }
    // Process-level signal lines.
    if trimmed.starts_with("Killed")
        || trimmed.starts_with("Aborted")
        || trimmed.starts_with("Segmentation fault")
        || trimmed.starts_with("Error:")
        || trimmed.starts_with("exit status")
        || trimmed.starts_with("exit code")
    {
        return true;
    }
    // `test some::name ... ok|FAILED|ignored` is the per-test result line in
    // libtest. Cheap to match and useful for pinpointing the failing case.
    if trimmed.starts_with("test ") && (trimmed.ends_with("FAILED") || trimmed.ends_with("ignored"))
    {
        return true;
    }
    false
}

fn char_boundary_at_or_before(text: &str, max_bytes: usize) -> usize {
    if max_bytes >= text.len() {
        return text.len();
    }

    let mut last_end = 0usize;
    for (idx, ch) in text.char_indices() {
        let end = idx.saturating_add(ch.len_utf8());
        if end > max_bytes {
            break;
        }
        last_end = end;
    }

    last_end.min(text.len())
}

fn char_boundary_at_or_after(text: &str, min_bytes: usize) -> usize {
    if min_bytes >= text.len() {
        return text.len();
    }
    if text.is_char_boundary(min_bytes) {
        return min_bytes;
    }
    text.char_indices()
        .map(|(idx, _)| idx)
        .find(|&idx| idx > min_bytes)
        .unwrap_or(text.len())
}

fn strip_truncation_note(text: &str) -> &str {
    text.split_once("\n\n[Output truncated")
        .map_or(text, |(prefix, _)| prefix)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let mut end = text.len();
    for (count, (idx, _)) in text.char_indices().enumerate() {
        if count == max_chars {
            end = idx;
            break;
        }
    }

    format!("{}...", &text[..end])
}

pub fn summarize_output(text: &str) -> String {
    let stripped = strip_truncation_note(text);
    let summary = stripped
        .lines()
        .take(SUMMARY_MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if summary.is_empty() {
        String::new()
    } else {
        truncate_chars(&summary, SUMMARY_MAX_CHARS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialise through the spillover guard: these tests swap the
    /// process-global test storage root (same convention as truncate.rs).
    /// The guard is held for the whole test body — releasing it after the
    /// swap would let a parallel test swap in its own root (or restore the
    /// real one) while this test's body is still spilling.
    struct TestRoot {
        prior: Option<std::path::PathBuf>,
        _tmp: tempfile::TempDir,
        /// Held until drop so the override stays exclusive through the test
        /// body and the temp-dir cleanup. Declared last so the mutex unlocks
        /// only after `_tmp` has removed its directory.
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl TestRoot {
        fn new() -> Self {
            let guard = super::super::truncate::TEST_SPILLOVER_GUARD
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().expect("tempdir");
            let prior = super::super::truncate::set_test_spillover_root(Some(
                tmp.path().join("tool_outputs"),
            ));
            Self {
                prior,
                _tmp: tmp,
                _guard: guard,
            }
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            // The struct's `_guard` already holds the mutex (fields drop
            // after this body) — re-locking here would self-deadlock.
            let _ = super::super::truncate::set_test_spillover_root(self.prior.take());
        }
    }

    #[test]
    fn spill_writes_combined_view_and_reports_total() {
        let _root = TestRoot::new();
        let stdout = "head\n".repeat(100);
        let stderr = "warn\n".repeat(50);
        let info =
            spill_full_shell_output("call-spill-1", &stdout, &stderr).expect("spill succeeds");

        assert_eq!(info.ref_id, "call-spill-1");
        assert!(info.path.exists(), "spill file missing: {:?}", info.path);
        let body = std::fs::read_to_string(&info.path).unwrap();
        assert_eq!(body.len(), info.total_bytes);
        assert!(body.contains("STDERR:"));
        assert!(body.ends_with(&stderr));
    }

    #[test]
    fn spill_without_stderr_omits_combined_marker() {
        let _root = TestRoot::new();
        let stdout = "plain output".to_string();
        let info = spill_full_shell_output("call-spill-2", &stdout, "").expect("spill");
        let body = std::fs::read_to_string(&info.path).unwrap();
        assert_eq!(body, "plain output");
        assert!(!body.contains("STDERR:"));
    }

    #[test]
    fn spillover_footer_names_the_ref_and_retrieval_tool() {
        let _root = TestRoot::new();
        let info = spill_full_shell_output("call-spill-3", "x", "y").expect("spill");
        let footer = spillover_footer(&info);
        assert!(footer.contains("retrieve_tool_result ref=call-spill-3"));
        assert!(footer.contains("mode=lines"));
        assert!(footer.contains("mode=query"));
        assert!(footer.contains(&info.total_bytes.to_string()));
    }

    #[test]
    fn spill_overwrites_same_id_with_fuller_content() {
        // Re-polling a still-running task spills again under the same
        // task id; the file must end up with the latest (longest) view.
        let _root = TestRoot::new();
        spill_full_shell_output("call-spill-4", "partial", "").expect("first spill");
        let info =
            spill_full_shell_output("call-spill-4", "partial plus more", "").expect("second");
        let body = std::fs::read_to_string(&info.path).unwrap();
        assert_eq!(body, "partial plus more");
    }

    #[test]
    fn truncation_preserves_cargo_test_summary_lines_from_tail() {
        let mut head = String::with_capacity(MAX_OUTPUT_SIZE + 4_000);
        head.push_str("running 5 tests\n");
        for i in 0..3_000 {
            head.push_str(&format!("test test::case_{i} ... ok\n"));
        }
        // Pad to force tail truncation
        while head.len() < MAX_OUTPUT_SIZE {
            head.push_str("...padding line below threshold...\n");
        }
        head.push_str("\ntest result: ok. 1687 passed; 0 failed; 2 ignored\n");
        head.push_str("    Finished `dev` profile target(s) in 4.87s\n");

        let (truncated, meta) = truncate_with_meta(&head);
        assert!(meta.truncated, "expected truncation");
        assert!(
            truncated.contains("test result: ok. 1687 passed"),
            "summary line must be preserved\nGot: {}",
            &truncated[truncated.len().saturating_sub(400)..]
        );
        assert!(
            truncated.contains("Finished"),
            "Finished marker must be preserved"
        );
    }

    #[test]
    fn truncation_preserves_failure_lines_from_tail() {
        let mut head = String::with_capacity(MAX_OUTPUT_SIZE + 1_000);
        for _ in 0..MAX_OUTPUT_SIZE {
            head.push('a');
        }
        head.push_str("\nfailures:\n  test::flaky_thing FAILED\n");
        head.push_str("test result: FAILED. 0 passed; 1 failed\n");

        let (truncated, _meta) = truncate_with_meta(&head);
        assert!(truncated.contains("failures:"), "must preserve failures:");
        assert!(truncated.contains("FAILED"), "must preserve FAILED");
    }

    #[test]
    fn truncation_includes_raw_tail_for_shell_output() {
        let mut output = String::new();
        output.push_str("head-marker\n");
        output.push_str(&"middle noise\n".repeat(3_000));
        output.push_str("tail-marker: final compiler error\n");

        let (truncated, meta) = truncate_with_meta(&output);

        assert!(meta.truncated, "expected truncation");
        assert!(truncated.contains("head-marker"));
        assert!(
            truncated.contains("[Output tail]"),
            "tail section should be explicit: {truncated}"
        );
        assert!(
            truncated.contains("tail-marker: final compiler error"),
            "raw tail must remain visible"
        );
    }

    #[test]
    fn collect_summary_lines_skips_noise() {
        let body = "\nblah blah\nrandom line\nokay\n\n";
        assert!(collect_summary_lines(body).is_empty());
    }

    #[test]
    fn collect_summary_lines_picks_rustc_errors() {
        let body = "\
some preamble
error[E0277]: the trait `Foo` is not implemented for `Bar`
  --> src/lib.rs:42:9
warning: unused variable
note: see help
";
        let preserved = collect_summary_lines(body);
        assert!(preserved.iter().any(|line| line.contains("error[E0277]")));
        assert!(preserved.iter().any(|line| line.contains("warning:")));
    }
}
