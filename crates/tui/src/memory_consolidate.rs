//! `codesmith memory consolidate` — the offline sleep-learning command
//! (P3-8 step 3).
//!
//! The online loop only appends to memory; this command is the offline
//! counterpart that may rewrite the index. Deterministic passes (exact
//! duplicate + stale pointer removal, orphan/budget reporting — see
//! `agent-runtime::knowledge::curator`) always run first and their result
//! is the floor: if the LLM merge proposal fails to resolve, errors out,
//! or is rejected by `validate_proposed_index`, the deterministic result
//! is what stands. Dual-loop separation honored by construction — nothing
//! here runs inside a session.
//!
//! Write gating: default is a dry run that prints a unified diff of the
//! proposed index; `--apply` writes it, taking a `MEMORY.md.bak` backup
//! first (side-git's rollback discipline, at memory scale). Topic file
//! *contents* are never touched — merging or deleting content is a
//! semantic decision that stays with the human.

use anyhow::Result;

use crate::llm_client::LlmClientHandle;

/// Budget for the consolidation proposal — an index rewrite, not an essay.
const PROPOSAL_MAX_TOKENS: u32 = 2000;

/// Hard timeout for the proposal call. Consolidation is an explicit
/// offline command; 60s is generous without being a hang.
const PROPOSAL_TIMEOUT_SECS: u64 = 60;

/// System prompt for the merge proposal. The output contract (single
/// fenced block, every file still referenced) is enforced downstream by
/// `curator::validate_proposed_index` — the prompt asks, the validator
/// decides.
fn consolidation_system_prompt(line_budget: usize) -> String {
    format!(
        "You are the memory curator for codesmith. You receive the current \
MEMORY.md index (after deterministic cleanup) and the name/description \
frontmatter of every topic file on disk. Rewrite ONLY the index: sharpen \
overlapping descriptions, group related pointers under short section \
headers, keep exactly one pointer per topic file in the \
`- [label](file.md) — description` format, and preserve every topic file. \
Do not invent files, do not drop files, do not touch file contents. \
Output the new index as plain markdown inside a single ```markdown code \
fence and nothing else. Stay within {line_budget} lines."
    )
}

/// Strip the outer code fence from a model response. Accepts ```` ```markdown ``
/// or plain ```` ``` ```` fences; returns the inner content. Unfenced input is
/// returned trimmed — the validator makes the final call either way.
pub(crate) fn strip_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    let Some(open_end) = trimmed.find("```") else {
        return trimmed.to_string();
    };
    let after_open = &trimmed[open_end + 3..];
    // Skip an optional language tag on the opening fence line.
    let body_start = after_open
        .find('\n')
        .map(|nl| open_end + 3 + nl + 1)
        .unwrap_or(open_end + 3);
    let body = &trimmed[body_start..];
    match body.rfind("```") {
        Some(close) => body[..close].trim().to_string(),
        None => body.trim().to_string(),
    }
}

/// Ask the model for a consolidated index. `None` on empty response,
/// transport error, or timeout — callers fall back to the deterministic
/// result. Usage flows through the cost side-channel (doctor-llm and
/// workshop precedent).
pub(crate) async fn propose_consolidated_index(
    client: &LlmClientHandle,
    model: &str,
    cleaned_index: &str,
    topic_headers_json: &str,
    line_budget: usize,
) -> Option<String> {
    use crate::models::{ContentBlock, Message, MessageRequest};

    let request = MessageRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: format!(
                    "Current index (post deterministic cleanup):\n\n{cleaned_index}\n\n\
Topic files on disk (frontmatter):\n\n{topic_headers_json}"
                ),
                cache_control: None,
            }],
        }],
        max_tokens: PROPOSAL_MAX_TOKENS,
        system: Some(crate::models::SystemPrompt::Text(
            consolidation_system_prompt(line_budget),
        )),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: Some(false),
        temperature: Some(0.2),
        top_p: None,
    };
    let timeout = std::time::Duration::from_secs(PROPOSAL_TIMEOUT_SECS);
    let response = match tokio::time::timeout(timeout, client.create_message(request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(_)) | Err(_) => return None,
    };
    codesmith_agent_runtime::cost_status::report(model, &response.usage);
    let text: String = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let stripped = strip_code_fence(&text);
    let stripped = stripped.trim();
    if stripped.is_empty() {
        None
    } else {
        Some(format!("{stripped}\n"))
    }
}

/// Entry point for `codesmith memory consolidate [--apply]
/// [--deterministic-only]`. Prints the deterministic report, optionally
/// the validated LLM proposal as a unified diff, and writes only under
/// `--apply` (with a `.bak` backup).
pub(crate) async fn run_memory_consolidate(
    config: &crate::config::Config,
    apply: bool,
    deterministic_only: bool,
) -> Result<()> {
    use colored::Colorize;

    let memory_dir = config.memory_dir();
    println!("{}", "codesmith memory consolidate".bold());
    println!("========================");
    if !memory_dir.exists() {
        println!(
            "  · no memory directory at {} — nothing to consolidate \
(enable [memory] kod_enabled or create the directory)",
            crate::utils::display_path(&memory_dir)
        );
        return Ok(());
    }

    let original = std::fs::read_to_string(memory_dir.join("MEMORY.md")).unwrap_or_default();
    let report = codesmith_agent_runtime::knowledge::curator::curate_index(&memory_dir);
    if report.index_missing {
        println!(
            "  · no MEMORY.md at {} — nothing to consolidate",
            crate::utils::display_path(&report.index_path)
        );
        return Ok(());
    }

    println!("  · pointer lines: {}", report.pointer_lines_total);
    println!(
        "  · exact duplicates removed: {}",
        report.duplicate_pointers_removed
    );
    println!(
        "  · stale pointers removed: {}",
        report.stale_pointers_removed.len()
    );
    for stale in &report.stale_pointers_removed {
        println!("      {stale}");
    }
    if !report.orphan_topic_files.is_empty() {
        println!(
            "  ! orphan topic files (on disk, unreferenced — not deleted): {}",
            report.orphan_topic_files.join(", ")
        );
    }
    if report.over_line_budget || report.over_byte_budget {
        println!(
            "  ! index exceeds the entrypoint budget after cleanup — the LLM merge pass is recommended"
        );
    }

    // The deterministic result is the floor; a valid LLM proposal may
    // supersede it, nothing else can.
    let mut final_index = report.cleaned_index.clone();
    if !deterministic_only {
        match crate::doctor_llm::resolve_analysis_target(config) {
            Ok((client, model)) => {
                print!("  · Requesting merge proposal from {model}...");
                use std::io::Write;
                std::io::stdout().flush().ok();
                let headers: Vec<serde_json::Value> =
                    codesmith_agent_runtime::knowledge::scan::scan_memory_files(&memory_dir)
                        .into_iter()
                        .map(|header| {
                            serde_json::json!({
                                "file": header.filename,
                                "description": header.description.clone().unwrap_or_default(),
                            })
                        })
                        .collect();
                let headers_json = serde_json::to_string(&headers).unwrap_or_default();
                let proposed = propose_consolidated_index(
                    &client,
                    &model,
                    &report.cleaned_index,
                    &headers_json,
                    codesmith_agent_runtime::knowledge::budget::MAX_ENTRYPOINT_LINES,
                )
                .await;
                match proposed {
                    Some(proposed) => {
                        match codesmith_agent_runtime::knowledge::curator::validate_proposed_index(
                            &memory_dir,
                            &proposed,
                        ) {
                            Ok(()) => {
                                println!(
                                    "\r  ✓ merge proposal validated — every topic file still referenced"
                                );
                                final_index = proposed;
                            }
                            Err(reason) => {
                                println!(
                                    "\r  ! merge proposal rejected ({reason}) — keeping the deterministic result"
                                );
                            }
                        }
                    }
                    None => {
                        println!(
                            "\r  · {model} returned no proposal (empty/error/timeout) — keeping the deterministic result"
                        );
                    }
                }
            }
            Err(reason) => {
                // The resolver's error can carry multi-line auth help;
                // one line is enough here.
                let first_line = reason.lines().next().unwrap_or("no client");
                println!("  · LLM proposal skipped ({first_line}) — deterministic passes only");
            }
        }
    }

    if final_index == original {
        println!("  ✓ index is already clean — no changes");
        return Ok(());
    }

    println!();
    println!("{}", "Proposed index (unified diff):".bold());
    let diff = codesmith_agent_runtime::tools::diff_format::make_unified_diff(
        "MEMORY.md",
        &original,
        &final_index,
    );
    for line in diff.lines() {
        if line.starts_with('-') {
            println!("{}", line.red());
        } else if line.starts_with('+') {
            println!("{}", line.green());
        } else {
            println!("{line}");
        }
    }

    if !apply {
        println!();
        println!(
            "  · dry run — re-run with --apply to write (a MEMORY.md.bak backup is taken first)"
        );
        return Ok(());
    }
    let backup = memory_dir.join("MEMORY.md.bak");
    std::fs::copy(&report.index_path, &backup)?;
    codesmith_agent_runtime::utils::write_atomic(&report.index_path, final_index.as_bytes())?;
    println!();
    println!(
        "  ✓ index written — backup at {}",
        crate::utils::display_path(&backup)
    );
    println!(
        "  · prompt-cache note: the next session's system prompt refresh picks the new index up automatically"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::mock::{MockLlmClient, canned};

    #[test]
    fn strip_code_fence_extracts_markdown_block() {
        let fenced = "Some preamble\n```markdown\n# Agent Memory\n\n- [x](y.md) — d\n```\ntrailing";
        assert_eq!(
            strip_code_fence(fenced),
            "# Agent Memory\n\n- [x](y.md) — d"
        );
    }

    #[test]
    fn strip_code_fence_handles_plain_and_unfenced() {
        assert_eq!(strip_code_fence("```\nline\n```"), "line");
        assert_eq!(strip_code_fence("  bare text  "), "bare text");
    }

    #[tokio::test]
    async fn propose_consolidated_index_returns_fenced_content() {
        let mock = MockLlmClient::new(vec![canned::simple_text_turn(
            "```markdown\n# Agent Memory\n\n- [Rust role](rust-role.md) — Rust dev\n```",
        )]);
        let handle: LlmClientHandle = std::sync::Arc::new(mock);
        let proposed = propose_consolidated_index(
            &handle,
            "mock-model",
            "# Agent Memory\n",
            r#"[{"file":"rust-role.md","description":"Rust"}]"#,
            200,
        )
        .await
        .expect("proposal");
        assert!(proposed.contains("rust-role.md"));
        assert!(!proposed.contains("```"), "fence must be stripped");
    }

    #[tokio::test]
    async fn propose_consolidated_index_returns_none_on_error() {
        let handle: LlmClientHandle = std::sync::Arc::new(MockLlmClient::new(Vec::new()));
        assert!(
            propose_consolidated_index(&handle, "mock-model", "idx", "[]", 200)
                .await
                .is_none()
        );
    }
}
