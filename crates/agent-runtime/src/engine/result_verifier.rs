//! Result claim verifier — P3-8 ("持续进化闭环") step 1, the first domino.
//!
//! Saving an experience is not learning from it: Slop Ledger records and
//! MEMORY.md stores, but nothing compared a claim against fact. This module
//! gives the online execution loop its *result verifier* (笔记9: "最应优先建
//! 立" — the one validator to build first): when a turn ends with the model
//! asserting "tests pass / build succeeds", the engine re-runs the
//! verification-class command the model itself executed during that turn and
//! checks the claim against the exit code.
//!
//! The verdict is pure evidence, never an instruction (safety boundary #1).
//! It flows back the same way LSP diagnostics do: appended to a pending
//! buffer at turn end, flushed as a synthetic `<codesmith:runtime_event>`
//! user message right before the next API request — appended at the tail,
//! never rewriting history (the KV-cache append-only discipline holds).
//!
//! Safety contract (P3-8 boundary #4, non-negotiable):
//!
//! * **Approved-replay only.** The re-run goes through the session's own
//!   [`ToolDispatcher::execute`](crate::tool_dispatch::ToolDispatcher::execute)
//!   and replays *exactly* the input the model already sent (same command,
//!   same cwd, same timeout) during this turn — an input that already passed
//!   the approval gate once. Only commands starting with a known test/build
//!   prefix are eligible; arbitrary `exec_shell` history is never replayed.
//!   Verification grants no authority the turn didn't already have. (Same
//!   philosophy as the capacity controller's read-only tool replay, extended
//!   from read-only tools to already-approved verification-class commands.)
//! * **Read-only over capabilities.** The verifier never writes files, never
//!   edits prompts, never changes the tool surface, and never touches the
//!   approval gate, validators, or release thresholds — 安全机制不可自我修改.
//! * **Online loop records only.** Verdicts are appended as transcript
//!   evidence and an `Event::ResultVerification`; nothing here rewrites any
//!   durable capability (双循环分离 — extraction and gated publishing stay
//!   offline, human-in-the-loop).
//!
//! Output shape (笔记9 four elements): every verdict carries 结论 (`verdict`),
//! 维度 (`dimension: result`), 证据位置 (`evidence`: command + exit code +
//! originating `tool_use_id` + output head), and 失败类型 (`failure_type`).

use std::sync::Arc;

use codesmith_agent::models::{ContentBlock, Message};
use codesmith_tools::ToolResult;

use super::context::summarize_text;

/// Claim phrases asserting a passing test suite. Matched as contiguous
/// substrings against the lowercased text of the turn's final assistant
/// message. Phrase matching (not regexes) is what keeps negated forms out:
/// "测试未通过" does not contain "测试通过" (未 breaks the phrase) and
/// "tests failed" does not contain "tests pass". Known residual false
/// positive: "not all tests pass" — rare, and the cost is one redundant
/// re-run that reports the truthful exit code.
const TEST_CLAIM_PHRASES: &[&str] = &[
    "测试通过",
    "测试都通过",
    "测试已通过",
    "测试成功",
    "tests pass",
    "tests all pass",
    "tests are passing",
    "test suite pass",
];

/// Claim phrases asserting a successful build/compile.
const BUILD_CLAIM_PHRASES: &[&str] = &[
    "构建成功",
    "构建通过",
    "编译成功",
    "编译通过",
    "编译无误",
    "build succeeded",
    "build succeeds",
    "build passed",
    "build passes",
    "build is successful",
    "compiles successfully",
    "compiled successfully",
    "compilation succeeded",
];

/// Command prefixes that make an `exec_shell` call eligible as a *test*
/// verification command. A command is eligible when any `&&`/`;`-separated
/// segment (after stripping leading `VAR=value` assignments) starts with one
/// of these prefixes — so `cd crate && cargo test` qualifies, as does
/// `cargo test -- --version`.
const TEST_COMMAND_PREFIXES: &[&str] = &[
    "cargo test",
    "cargo nextest",
    "cargo t",
    "npm test",
    "npm run test",
    "yarn test",
    "pnpm test",
    "bun test",
    "pytest",
    "python -m pytest",
    "python -m unittest",
    "go test",
    "make test",
    "jest",
    "vitest",
    "rake test",
    "mvn test",
    "gradle test",
    "dotnet test",
    "ctest",
];

/// Command prefixes that make an `exec_shell` call eligible as a *build*
/// verification command. Deliberately excludes bare `make` (matches
/// `make install` / `make clean`) and `cargo clippy` (a lint, not a build).
const BUILD_COMMAND_PREFIXES: &[&str] = &[
    "cargo build",
    "cargo check",
    "npm run build",
    "yarn build",
    "pnpm build",
    "bun run build",
    "go build",
    "make build",
    "make all",
    "cmake --build",
    "tsc",
    "gradle build",
    "mvn package",
    "dotnet build",
];

/// Cap on the output head carried in the verdict evidence. Enough to show
/// failing tests / compile errors, small enough to keep the injected message
/// in the low hundreds of tokens.
const EVIDENCE_EXCERPT_LIMIT: usize = 2000;

/// Fallback timeout for the verification re-run when the original tool call
/// carried none: five minutes — generous for `cargo test` on a cold cache,
/// bounded so a hung command cannot pin a background task forever.
const DEFAULT_VERIFICATION_TIMEOUT_MS: u64 = 300_000;

/// Which kind of result the claim asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimKind {
    Test,
    Build,
}

impl ClaimKind {
    fn failure_label(self) -> &'static str {
        match self {
            ClaimKind::Test => "test-failure",
            ClaimKind::Build => "build-failure",
        }
    }
}

/// A success claim detected in the turn's final assistant message.
#[derive(Debug, Clone)]
pub(crate) struct DetectedClaim {
    pub(crate) kind: ClaimKind,
    /// The matched phrase (e.g. "测试通过") — quoted in the verdict so the
    /// reader sees what triggered the check.
    pub(crate) phrase: String,
}

/// Scan the turn's transcript slice for a success claim in the **final**
/// assistant message. The final message is where turn summaries live; earlier
/// intermediate texts are not claims about the turn's end state.
pub(crate) fn detect_claim(turn_messages: &[Message]) -> Option<DetectedClaim> {
    let last_assistant = turn_messages.iter().rev().find(|m| m.role == "assistant")?;
    let text: String = last_assistant
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let lower = text.to_lowercase();
    for phrase in TEST_CLAIM_PHRASES {
        if lower.contains(phrase) {
            return Some(DetectedClaim {
                kind: ClaimKind::Test,
                phrase: (*phrase).to_string(),
            });
        }
    }
    for phrase in BUILD_CLAIM_PHRASES {
        if lower.contains(phrase) {
            return Some(DetectedClaim {
                kind: ClaimKind::Build,
                phrase: (*phrase).to_string(),
            });
        }
    }
    None
}

/// A verification-class command the model executed during the turn, selected
/// for replay. Carries the original tool-call identity so the verdict's
/// evidence can point at the exact transcript position (笔记9: 证据位置).
#[derive(Debug, Clone)]
pub(crate) struct VerificationCommand {
    /// The tool input verbatim, as the model sent it — replayed unchanged
    /// (same command/cwd/timeout) so the rerun is a faithful reproduction.
    pub(crate) replay_input: serde_json::Value,
    /// Display form of the command, for the verdict text.
    pub(crate) display_command: String,
    pub(crate) tool_name: String,
    pub(crate) tool_use_id: String,
}

/// Find the most recent verification-class command of `kind` in the turn's
/// transcript. Only foreground `exec_shell` calls and `run_tests` calls are
/// eligible: background task flows (`task_shell_start` + wait) have a
/// lifecycle of their own and replaying the start would re-spawn a detached
/// process — out of scope for the minimal verifier.
pub(crate) fn find_verification_command(
    turn_messages: &[Message],
    kind: ClaimKind,
) -> Option<VerificationCommand> {
    for msg in turn_messages.iter().rev() {
        if msg.role != "assistant" {
            continue;
        }
        for block in msg.content.iter().rev() {
            let ContentBlock::ToolUse {
                id, name, input, ..
            } = block
            else {
                continue;
            };
            match name.as_str() {
                "exec_shell" => {
                    let background = input
                        .get("background")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let Some(command) = input.get("command").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    if background || !command_matches_kind(command, kind) {
                        continue;
                    }
                    return Some(VerificationCommand {
                        replay_input: input.clone(),
                        display_command: command.to_string(),
                        tool_name: name.clone(),
                        tool_use_id: id.clone(),
                    });
                }
                "run_tests" => {
                    if kind != ClaimKind::Test {
                        continue;
                    }
                    let Some(command) = reconstruct_run_tests_command(input) else {
                        continue;
                    };
                    let mut replay_input = serde_json::Map::new();
                    replay_input.insert("command".into(), command.clone().into());
                    replay_input
                        .insert("timeout_ms".into(), DEFAULT_VERIFICATION_TIMEOUT_MS.into());
                    return Some(VerificationCommand {
                        replay_input: serde_json::Value::Object(replay_input),
                        display_command: command,
                        tool_name: name.clone(),
                        tool_use_id: id.clone(),
                    });
                }
                _ => {}
            }
        }
    }
    None
}

/// Rebuild a `run_tests` invocation as an equivalent `cargo test` command
/// string (`--all-features` + verbatim args), for replay through exec_shell.
fn reconstruct_run_tests_command(input: &serde_json::Value) -> Option<String> {
    let mut command = String::from("cargo test");
    if input
        .get("all_features")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        command.push_str(" --all-features");
    }
    if let Some(args) = input.get("args").and_then(|v| v.as_array()) {
        for arg in args {
            if let Some(arg) = arg.as_str() {
                command.push(' ');
                command.push_str(arg);
            }
        }
    }
    Some(command)
}

/// Whether any `&&`/`;`-separated segment of `command` starts with a prefix
/// of the claimed kind, after stripping leading `VAR=value` assignments.
fn command_matches_kind(command: &str, kind: ClaimKind) -> bool {
    let prefixes = match kind {
        ClaimKind::Test => TEST_COMMAND_PREFIXES,
        ClaimKind::Build => BUILD_COMMAND_PREFIXES,
    };
    let lower = command.to_lowercase();
    lower
        .split("&&")
        .flat_map(|seg| seg.split(';'))
        .any(|segment| {
            let mut segment = segment.trim();
            // Strip leading env assignments: `RUSTFLAGS="..." cargo test`.
            while let Some(rest) = strip_env_assignment(segment) {
                segment = rest;
            }
            prefixes.iter().any(|prefix| segment.starts_with(prefix))
        })
}

/// Strip one leading `NAME=value` token (quoted or bare); `None` when the
/// segment does not start with one.
fn strip_env_assignment(segment: &str) -> Option<&str> {
    let mut chars = segment.char_indices();
    if !chars
        .next()
        .is_some_and(|(_, c)| c.is_ascii_alphabetic() || c == '_')
    {
        return None;
    }
    let mut eq = None;
    for (i, c) in chars {
        if c.is_ascii_alphanumeric() || c == '_' {
            continue;
        }
        if c == '=' {
            eq = Some(i);
        }
        break;
    }
    let eq = eq?;
    // Value runs to the matching close quote (quoted) or next whitespace.
    // For the quoted forms the in-segment layout is `NAME="value" rest`, so
    // the remainder starts one past the *closing* quote (eq + 1 opening +
    // value_len + 1 closing); for the bare form it is `NAME=value rest`.
    let after_eq = &segment[eq + 1..];
    let (value_len, quoted) = if let Some(rest) = after_eq.strip_prefix('"') {
        (rest.find('"').unwrap_or(rest.len()), true)
    } else if let Some(rest) = after_eq.strip_prefix('\'') {
        (rest.find('\'').unwrap_or(rest.len()), true)
    } else {
        (
            after_eq.find(char::is_whitespace).unwrap_or(after_eq.len()),
            false,
        )
    };
    let rest_start = if quoted {
        eq + value_len + 3
    } else {
        eq + value_len + 1
    };
    segment.get(rest_start..).map(str::trim_start)
}

/// Verdict outcome. Labels double as the wire vocabulary used by the
/// injected message and `Event::ResultVerification`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerdictKind {
    VerifiedPass,
    VerifiedFail,
    /// The re-run itself did not complete (timeout / sandbox denial / tool
    /// unavailable). Never reported as pass or fail — an unverifiable check
    /// must not masquerade as either.
    VerifyError,
    /// The model claimed success but executed no verification-class command
    /// during the turn — the highest-value signal this checker produces
    /// (a claim with no execution evidence behind it).
    Unsubstantiated,
}

impl VerdictKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            VerdictKind::VerifiedPass => "verified-pass",
            VerdictKind::VerifiedFail => "verified-fail",
            VerdictKind::VerifyError => "verify-error",
            VerdictKind::Unsubstantiated => "unsubstantiated",
        }
    }
}

/// One recorded verdict — the unit the pending buffer accumulates and the
/// pre-request flush renders. Field set mirrors 笔记9's four elements.
#[derive(Debug, Clone)]
pub(crate) struct VerdictBlock {
    pub(crate) kind: VerdictKind,
    pub(crate) claim_kind: ClaimKind,
    pub(crate) phrase: String,
    pub(crate) command: Option<String>,
    pub(crate) tool_use_id: Option<String>,
    pub(crate) exit_code: Option<i64>,
    /// Why the check could not run (VerifyError only).
    pub(crate) error_reason: Option<String>,
    /// Head of the re-run output, already length-capped.
    pub(crate) evidence_excerpt: String,
}

impl VerdictBlock {
    pub(crate) fn failure_type(&self) -> Option<&'static str> {
        match self.kind {
            VerdictKind::VerifiedFail => Some(self.claim_kind.failure_label()),
            VerdictKind::VerifyError => Some("verify-error"),
            VerdictKind::Unsubstantiated => Some("unsubstantiated-claim"),
            VerdictKind::VerifiedPass => None,
        }
    }

    /// Render the four-element verdict text (结论/维度/证据位置/失败类型).
    fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("verdict: {}", self.kind.label()));
        if self.kind == VerdictKind::VerifiedFail {
            out.push_str(" (claim mismatch)");
        }
        out.push('\n');
        out.push_str("dimension: result\n");
        out.push_str(&format!(
            "claim: \"{}\" — asserted in the previous turn's final assistant message\n",
            self.phrase
        ));
        if let Some(command) = &self.command {
            out.push_str(&format!(
                "evidence: re-ran `{command}` (from tool call {}) → exit code {}\n",
                self.tool_use_id.as_deref().unwrap_or("?"),
                self.exit_code
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string())
            ));
            if !self.evidence_excerpt.is_empty() {
                out.push_str("output head:\n");
                out.push_str(&self.evidence_excerpt);
                out.push('\n');
            }
            if let Some(reason) = &self.error_reason {
                out.push_str(&format!("note: {reason}\n"));
            }
        } else {
            out.push_str(
                "evidence: no verification-class command was executed during the turn; \
nothing to re-run\n",
            );
        }
        if let Some(failure_type) = self.failure_type() {
            out.push_str(&format!("failure_type: {failure_type}\n"));
        }
        out
    }
}

/// Execute the verification re-run and produce the verdict. Pure with respect
/// to engine state — the caller decides what to do with the returned block
/// (push to the pending buffer, emit the event).
pub(crate) async fn verify_claim(
    dispatcher: Option<Arc<dyn crate::tool_dispatch::ToolDispatcher>>,
    claim: DetectedClaim,
    command: Option<VerificationCommand>,
) -> VerdictBlock {
    let Some(command) = command else {
        return VerdictBlock {
            kind: VerdictKind::Unsubstantiated,
            claim_kind: claim.kind,
            phrase: claim.phrase,
            command: None,
            tool_use_id: None,
            exit_code: None,
            error_reason: None,
            evidence_excerpt: String::new(),
        };
    };
    let Some(dispatcher) = dispatcher else {
        return error_verdict(claim, &command, "tool dispatcher unavailable this turn");
    };
    if !dispatcher.has_tool("exec_shell") {
        return error_verdict(claim, &command, "exec_shell is not in this turn's tool set");
    }
    // `run_tests` is replayed through the equivalent exec_shell command —
    // the same surface the reconstructed input was built for.
    let replay_name = if command.tool_name == "run_tests" {
        "exec_shell"
    } else {
        command.tool_name.as_str()
    };
    let result = dispatcher
        .execute(replay_name, command.replay_input.clone(), None)
        .await;
    match result {
        Ok(tool_result) => verdict_from_tool_result(claim, &command, tool_result),
        Err(err) => error_verdict(claim, &command, &format!("replay error: {err}")),
    }
}

fn error_verdict(
    claim: DetectedClaim,
    command: &VerificationCommand,
    reason: &str,
) -> VerdictBlock {
    VerdictBlock {
        kind: VerdictKind::VerifyError,
        claim_kind: claim.kind,
        phrase: claim.phrase,
        command: Some(command.display_command.clone()),
        tool_use_id: Some(command.tool_use_id.clone()),
        exit_code: None,
        error_reason: Some(summarize_text(reason, 300)),
        evidence_excerpt: String::new(),
    }
}

/// Map the replayed tool result onto a verdict via its structured metadata
/// (`exit_code`, `status`, `sandbox_denied`) with `success` as the fallback.
fn verdict_from_tool_result(
    claim: DetectedClaim,
    command: &VerificationCommand,
    result: ToolResult,
) -> VerdictBlock {
    let metadata = result.metadata.as_ref();
    let exit_code = metadata
        .and_then(|m| m.get("exit_code"))
        .and_then(serde_json::Value::as_i64);
    let status = metadata
        .and_then(|m| m.get("status"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let sandbox_denied = metadata
        .and_then(|m| m.get("sandbox_denied"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let evidence_excerpt = summarize_text(result.content.trim(), EVIDENCE_EXCERPT_LIMIT);

    let kind = if sandbox_denied {
        VerdictKind::VerifyError
    } else if exit_code == Some(0) {
        VerdictKind::VerifiedPass
    } else if exit_code.is_some() {
        VerdictKind::VerifiedFail
    } else if result.success {
        VerdictKind::VerifiedPass
    } else if status.contains("timeout") {
        VerdictKind::VerifyError
    } else {
        // No exit code and no success flag: treat as a failed run rather
        // than pass — the checker fails closed like every other validator.
        VerdictKind::VerifiedFail
    };
    let error_reason = sandbox_denied.then(|| "sandbox denied the replayed command".to_string());
    VerdictBlock {
        kind,
        claim_kind: claim.kind,
        phrase: claim.phrase,
        command: Some(command.display_command.clone()),
        tool_use_id: Some(command.tool_use_id.clone()),
        exit_code,
        error_reason,
        evidence_excerpt,
    }
}

/// Render pending verdicts into the payload for the synthetic message.
/// `None` when there is nothing to inject (empty buffer).
pub(crate) fn render_verdicts(blocks: &[VerdictBlock]) -> Option<String> {
    if blocks.is_empty() {
        return None;
    }
    Some(
        blocks
            .iter()
            .map(VerdictBlock::render)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Build the `<codesmith:runtime_event kind="result_verification">` user
/// message that carries the verdicts back into the transcript. Mirrors
/// `subagent_completion_runtime_message` (`turn::postprocess`): role is
/// `"user"` for strict chat-template compatibility (a mid-conversation
/// system message breaks vLLM/Qwen3 templates with a 400), and the
/// `visibility="internal"` tag marks the payload as runtime data, never
/// user input — the evidence/instruction isolation boundary.
pub(crate) fn verdict_runtime_message(rendered: &str) -> Message {
    Message {
        role: "user".to_string(),
        content: vec![ContentBlock::Text {
            text: format!(
                "<codesmith:runtime_event kind=\"result_verification\" visibility=\"internal\">\n\
This is an internal runtime event, not user input. The engine re-ran a verification command \
from the previous turn and checked it against a claim made there. Treat the exit code \
reported below as ground truth and reconcile your earlier claim with it; if the verdict \
contradicts what you said, say so and fix it. Do not quote the raw XML unless the user \
asks to debug engine internals.\n\n\
{rendered}\n\
</codesmith:runtime_event>"
            ),
            cache_control: None,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant_text(text: &str) -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn assistant_tool_use(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
                caller: None,
            }],
        }
    }

    fn user_text(text: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    // ── claim detection ────────────────────────────────────────────────

    #[test]
    fn detects_chinese_test_claim() {
        let msgs = [assistant_text("修复完成,所有测试通过,可以合入。")];
        let claim = detect_claim(&msgs).expect("claim detected");
        assert_eq!(claim.kind, ClaimKind::Test);
        assert_eq!(claim.phrase, "测试通过");
    }

    #[test]
    fn detects_english_test_claim_case_insensitively() {
        let msgs = [assistant_text("All tests PASS now.")];
        let claim = detect_claim(&msgs).expect("claim detected");
        assert_eq!(claim.kind, ClaimKind::Test);
    }

    #[test]
    fn detects_build_claims_in_both_languages() {
        assert_eq!(
            detect_claim(&[assistant_text("构建成功,产物在 target/ 下。")]).map(|c| c.kind),
            Some(ClaimKind::Build)
        );
        assert_eq!(
            detect_claim(&[assistant_text("The build succeeded.")]).map(|c| c.kind),
            Some(ClaimKind::Build)
        );
    }

    #[test]
    fn negated_claims_do_not_match() {
        assert!(detect_claim(&[assistant_text("测试未通过,还需要修复。")]).is_none());
        assert!(detect_claim(&[assistant_text("tests failed, see the log")]).is_none());
        assert!(detect_claim(&[assistant_text("构建失败了")]).is_none());
    }

    #[test]
    fn only_the_final_assistant_message_is_scanned() {
        let msgs = [
            assistant_text("cargo test 跑过了,tests pass"),
            user_text("所有测试通过了吗?"),
            assistant_text("还有一个小问题,继续修。"),
        ];
        assert!(detect_claim(&msgs).is_none());
    }

    #[test]
    fn no_assistant_message_yields_none() {
        let msgs = [user_text("tests pass?")];
        assert!(detect_claim(&msgs).is_none());
    }

    // ── command extraction ─────────────────────────────────────────────

    #[test]
    fn picks_the_most_recent_matching_exec_shell() {
        let msgs = [
            assistant_tool_use(
                "t1",
                "exec_shell",
                json!({"command": "cargo test --workspace"}),
            ),
            assistant_tool_use("t2", "exec_shell", json!({"command": "cargo test --lib"})),
        ];
        let cmd = find_verification_command(&msgs, ClaimKind::Test).expect("found");
        assert_eq!(cmd.tool_use_id, "t2");
        assert_eq!(cmd.display_command, "cargo test --lib");
    }

    #[test]
    fn chained_commands_qualify_via_segment_match() {
        let msgs = [assistant_tool_use(
            "t1",
            "exec_shell",
            json!({"command": "cd crates/tui && cargo test"}),
        )];
        let cmd = find_verification_command(&msgs, ClaimKind::Test).expect("found");
        assert_eq!(cmd.tool_use_id, "t1");
    }

    #[test]
    fn env_prefixed_commands_qualify() {
        let msgs = [assistant_tool_use(
            "t1",
            "exec_shell",
            json!({"command": "RUSTFLAGS=\"-D warnings\" cargo test"}),
        )];
        assert!(find_verification_command(&msgs, ClaimKind::Test).is_some());
    }

    #[test]
    fn non_verification_commands_and_background_calls_are_skipped() {
        let msgs = [
            assistant_tool_use("t1", "exec_shell", json!({"command": "ls -la"})),
            assistant_tool_use(
                "t2",
                "exec_shell",
                json!({"command": "cargo test", "background": true}),
            ),
            assistant_tool_use("t3", "grep_files", json!({"pattern": "tests pass"})),
        ];
        assert!(find_verification_command(&msgs, ClaimKind::Test).is_none());
    }

    #[test]
    fn build_claims_do_not_replay_test_commands_and_vice_versa() {
        let msgs = [assistant_tool_use(
            "t1",
            "exec_shell",
            json!({"command": "cargo build"}),
        )];
        assert!(find_verification_command(&msgs, ClaimKind::Test).is_none());
        assert_eq!(
            find_verification_command(&msgs, ClaimKind::Build)
                .expect("build command found")
                .tool_use_id,
            "t1"
        );
    }

    #[test]
    fn run_tests_calls_are_reconstructed_as_exec_shell_replay() {
        let msgs = [assistant_tool_use(
            "t9",
            "run_tests",
            json!({"args": ["--lib"], "all_features": true}),
        )];
        let cmd = find_verification_command(&msgs, ClaimKind::Test).expect("found");
        assert_eq!(cmd.display_command, "cargo test --all-features --lib");
        assert_eq!(
            cmd.replay_input,
            json!({"command": "cargo test --all-features --lib", "timeout_ms": 300_000})
        );
    }

    // ── verdict rendering ──────────────────────────────────────────────

    fn sample_verdict(kind: VerdictKind) -> VerdictBlock {
        VerdictBlock {
            kind,
            claim_kind: ClaimKind::Test,
            phrase: "所有测试通过".to_string(),
            command: Some("cargo test".to_string()),
            tool_use_id: Some("toolu_01".to_string()),
            exit_code: Some(1),
            error_reason: None,
            evidence_excerpt: "test result: FAILED. 1 passed; 2 failed".to_string(),
        }
    }

    #[test]
    fn verdict_render_carries_the_four_elements() {
        let rendered = sample_verdict(VerdictKind::VerifiedFail).render();
        assert!(rendered.contains("verdict: verified-fail (claim mismatch)"));
        assert!(rendered.contains("dimension: result"));
        assert!(rendered.contains("evidence: re-ran `cargo test`"));
        assert!(rendered.contains("toolu_01"));
        assert!(rendered.contains("exit code 1"));
        assert!(rendered.contains("failure_type: test-failure"));
    }

    #[test]
    fn unsubstantiated_verdict_names_the_missing_evidence() {
        let block = VerdictBlock {
            kind: VerdictKind::Unsubstantiated,
            claim_kind: ClaimKind::Test,
            phrase: "tests pass".to_string(),
            command: None,
            tool_use_id: None,
            exit_code: None,
            error_reason: None,
            evidence_excerpt: String::new(),
        };
        let rendered = block.render();
        assert!(rendered.contains("verdict: unsubstantiated"));
        assert!(rendered.contains("no verification-class command"));
        assert!(rendered.contains("failure_type: unsubstantiated-claim"));
    }

    #[test]
    fn pass_verdict_has_no_failure_type() {
        let rendered = sample_verdict(VerdictKind::VerifiedPass).render();
        assert!(rendered.contains("verdict: verified-pass"));
        assert!(!rendered.contains("failure_type"));
    }

    #[test]
    fn runtime_message_is_a_user_role_wrapped_event() {
        let msg = verdict_runtime_message("verdict: verified-pass\n");
        assert_eq!(msg.role, "user");
        match &msg.content[0] {
            ContentBlock::Text { text, .. } => {
                assert!(text.contains(r#"kind="result_verification""#));
                assert!(text.contains(r#"visibility="internal""#));
                assert!(text.contains("verdict: verified-pass"));
            }
            _ => panic!("expected a text block"),
        }
    }

    #[test]
    fn render_verdicts_joins_blocks_and_handles_empty() {
        assert!(render_verdicts(&[]).is_none());
        let joined =
            render_verdicts(&[sample_verdict(VerdictKind::VerifiedPass)]).expect("rendered");
        assert!(joined.contains("verdict: verified-pass"));
    }

    // ── tool-result mapping ────────────────────────────────────────────

    #[test]
    fn exit_code_zero_is_pass_nonzero_is_fail() {
        let claim = DetectedClaim {
            kind: ClaimKind::Test,
            phrase: "tests pass".to_string(),
        };
        let command = VerificationCommand {
            replay_input: json!({"command": "cargo test"}),
            display_command: "cargo test".to_string(),
            tool_name: "exec_shell".to_string(),
            tool_use_id: "t1".to_string(),
        };
        let pass = verdict_from_tool_result(
            claim.clone(),
            &command,
            ToolResult {
                canonical: None,
                content: "ok".to_string(),
                success: true,
                metadata: Some(json!({"exit_code": 0})),
            },
        );
        assert_eq!(pass.kind, VerdictKind::VerifiedPass);
        let fail = verdict_from_tool_result(
            claim,
            &command,
            ToolResult {
                canonical: None,
                content: "FAILED".to_string(),
                success: false,
                metadata: Some(json!({"exit_code": 1})),
            },
        );
        assert_eq!(fail.kind, VerdictKind::VerifiedFail);
        assert_eq!(fail.failure_type(), Some("test-failure"));
    }

    #[test]
    fn sandbox_denial_is_an_error_never_a_fail_verdict() {
        let claim = DetectedClaim {
            kind: ClaimKind::Build,
            phrase: "build succeeded".to_string(),
        };
        let command = VerificationCommand {
            replay_input: json!({"command": "cargo build"}),
            display_command: "cargo build".to_string(),
            tool_name: "exec_shell".to_string(),
            tool_use_id: "t2".to_string(),
        };
        let block = verdict_from_tool_result(
            claim,
            &command,
            ToolResult {
                canonical: None,
                content: String::new(),
                success: false,
                metadata: Some(json!({"sandbox_denied": true})),
            },
        );
        assert_eq!(block.kind, VerdictKind::VerifyError);
        assert_eq!(block.failure_type(), Some("verify-error"));
    }
}
