//! `commands/list` + `commands/execute` — the '/' command catalog, bridged
//! onto the runtime's manual compaction route.
//!
//! Wire contracts (vendored `api-remotes`): the client sends
//! `list {agentId}` and `execute {agentId, line, submittedAttachments}`
//! (the AbortSignal maps to HTTP cancellation, not a field), and reads
//! `execute`'s result without strict decoding — so an admission miss is
//! `{"ok":true}` with NO `value` member (a present `null` would crash the
//! client's `result.value.result` read), while an admitted execution is
//! `{commandId, result:{kind…}}`.
//!
//! Admission mirrors the upstream registry's order (`dsh-commands`
//! `execute`): parse the line, resolve the name, refuse attachments
//! (compact declares no `input.attachments`), refuse arguments — each
//! refusal settles as a command error result, exactly the sentences the
//! upstream `command-compact` plugin produces.
//!
//! Known limitations:
//! - the catalog is static for the life of the bridge process (the runtime
//!   registers no dynamic commands), so no `commands/change` emit ever
//!   rides `$events`;
//! - no `command/run`/`command/done` wire events are synthesized — the
//!   compaction turn's own events already render it (a turn bracket plus
//!   the mapped `context_compaction` system message), the same vocabulary
//!   an automatic compaction uses mid-turn;
//! - the success result claims acceptance, not completion: the runtime's
//!   compact route is 202-then-async, so the outcome (counts, failure)
//!   arrives with the turn, not in this RPC's text.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::workspace::OpError;
use super::HostState;

/// The vendored builtin definition id for `/compact` — the rebrand of
/// upstream `@deepseek-ai/dsh-command-compact`. The client's builtin table
/// matches it, giving the menu row its localized face and resolving the
/// zh alias 压缩 to this command.
const COMPACT_DEFINITION_ID: &str = "@codesmith/command-compact";

/// `commands/list` — the one-entry catalog (upstream's own host registers
/// this exact name and description).
pub fn catalog() -> Value {
    json!([{
        "definitionId": COMPACT_DEFINITION_ID,
        "name": "compact",
        "description": "Compact older conversation history",
    }])
}

/// One execute's answer to the dispatch table.
pub enum Outcome {
    /// Admission miss (no command syntax, or a name the catalog lacks):
    /// `ok` with no `value` — the client reads the absent value as its
    /// "unknown or malformed command" outcome.
    Miss,
    /// An admitted execution: the `CommandExecution` value.
    Executed(Value),
}

/// The upstream registry's admission decision for a line, before any
/// runtime call.
enum Admission {
    Miss,
    /// A settled `kind:'error'` result (attachment or usage refusal).
    Error(String),
    Admitted,
}

/// Upstream `parseCommand`: `/name` where name is `[a-z][a-z0-9_-]*` and
/// the next character is end-of-line or whitespace; the rest is `rawInput`,
/// separator whitespace included. A name followed by anything else (`/x/y`,
/// `/xé`) resolves to nothing, even under backtracking.
fn parse_command(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('/')?;
    let bytes = rest.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_lowercase) {
        return None;
    }
    let end = bytes
        .iter()
        .position(|b| !matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
        .unwrap_or(bytes.len());
    let after = &rest[end..];
    if !(after.is_empty() || after.starts_with(['\t', '\n', '\r', ' '])) {
        return None;
    }
    Some((&rest[..end], after))
}

/// Admit one invocation against the compact definition (name resolution,
/// attachment refusal, usage refusal — in the registry's order).
fn admit(line: &str, attachment_count: usize) -> Admission {
    let Some((name, raw_input)) = parse_command(line) else {
        return Admission::Miss;
    };
    if name != "compact" {
        return Admission::Miss;
    }
    if attachment_count > 0 {
        return Admission::Error("/compact does not accept attachments".into());
    }
    if !raw_input.trim().is_empty() {
        return Admission::Error("Usage: /compact (no arguments)".into());
    }
    Admission::Admitted
}

/// Mint the next lifecycle pairing id (monotonic; instance-token-prefixed
/// so a restarted bridge never repeats one over the same journal). No
/// `command/run`/`command/done` pair is written today — the id keeps the
/// upstream shape for the day one is.
fn mint_command_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    static TOKEN: OnceLock<String> = OnceLock::new();
    let token = TOKEN.get_or_init(|| uuid::Uuid::new_v4().to_string()[..8].to_string());
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    format!("cmd-{token}-{seq}")
}

/// A settled `kind:'error'` execution (upstream settles refusals and
/// handler failures as results, not RPC errors).
fn command_error(text: &str) -> Value {
    json!({
        "commandId": mint_command_id(),
        "result": { "kind": "error", "text": text }
    })
}

fn bad_args(err: anyhow::Error) -> OpError {
    OpError {
        code: "gateway/arguments-invalid",
        message: format!("commands/execute: {err:#}"),
    }
}

fn internal(err: anyhow::Error) -> OpError {
    OpError {
        code: "gateway/internal",
        message: format!("commands/execute: {err:#}"),
    }
}

/// `commands/execute` — admit the line, then hand an admitted invocation
/// to the runtime's manual compaction route.
pub async fn execute(host: &HostState, args: &Value) -> Result<Outcome, OpError> {
    let session_id = args
        .get("agentId")
        .and_then(Value::as_str)
        .context("commands/execute without agentId")
        .map_err(bad_args)?
        .to_string();
    let line = args
        .get("line")
        .and_then(Value::as_str)
        .context("commands/execute without line")
        .map_err(bad_args)?;
    let attachment_count = args
        .get("submittedAttachments")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    match admit(line, attachment_count) {
        Admission::Miss => Ok(Outcome::Miss),
        Admission::Error(text) => Ok(Outcome::Executed(command_error(&text))),
        Admission::Admitted => {
            // The runtime claims the compaction turn synchronously, so its
            // verdict — quoted from the runtime's own error body — settles
            // as the command's error result (an upstream `busy` refusal
            // settles the same way).
            let path = format!("/v1/threads/{session_id}/compact");
            let posted = host
                .backend
                .post_with_error_message::<Value>(&path, &json!({}))
                .await
                .map_err(internal)?;
            match posted {
                // 202 Accepted: the compaction runs as an async turn; the
                // transcript carries its outcome (see the module notes).
                Ok(_started) => Ok(Outcome::Executed(json!({
                    "commandId": mint_command_id(),
                    "result": { "kind": "success", "text": "Compaction requested." }
                }))),
                Err(runtime_message) => Ok(Outcome::Executed(command_error(&runtime_message))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_serves_the_compact_descriptor_with_the_vendored_builtin_id() {
        let list = catalog();
        let entry = list
            .get(0)
            .unwrap_or_else(|| panic!("the catalog carries the compact entry"));
        assert_eq!(entry["name"], "compact");
        assert_eq!(entry["definitionId"], "@codesmith/command-compact");
        // `name` and `description` are the descriptor's required wire
        // fields; the client's zod array rejects an entry without them.
        let description = entry["description"].as_str().unwrap_or_default();
        assert!(!description.is_empty());
    }

    #[test]
    fn parse_command_mirrors_the_upstream_admission_regex() {
        assert_eq!(parse_command("/compact"), Some(("compact", "")));
        // rawInput keeps its separator whitespace, verbatim.
        assert_eq!(
            parse_command("/compact keep this"),
            Some(("compact", " keep this"))
        );
        assert_eq!(
            parse_command("/compact\ttabbed"),
            Some(("compact", "\ttabbed"))
        );
        // Any syntactically valid name parses; resolution happens later.
        assert_eq!(parse_command("/model"), Some(("model", "")));
        // The name must end at end-of-line or whitespace: "/compact/" and
        // "/compacté" resolve to nothing, even under backtracking.
        assert_eq!(parse_command("/compact/"), None);
        assert_eq!(parse_command("/compacté"), None);
        // The first name character must be [a-z].
        assert_eq!(parse_command("/Compact"), None);
        assert_eq!(parse_command("/1compact"), None);
        assert_eq!(parse_command("/"), None);
        assert_eq!(parse_command("compact"), None);
        assert_eq!(parse_command(""), None);
    }

    #[test]
    fn admission_mirrors_the_upstream_compact_registry() {
        assert!(matches!(admit("/compact", 0), Admission::Admitted));
        // An attachment settles as a command error before the usage check.
        let Admission::Error(text) = admit("/compact", 1) else {
            panic!("an attachment-carrying invocation must refuse");
        };
        assert_eq!(text, "/compact does not accept attachments");
        let Admission::Error(text) = admit("/compact keep this", 0) else {
            panic!("an argued invocation must refuse");
        };
        assert_eq!(text, "Usage: /compact (no arguments)");
        // Unknown names and non-command syntax are admission misses.
        assert!(matches!(admit("/model", 0), Admission::Miss));
        assert!(matches!(admit("/compact/", 0), Admission::Miss));
        assert!(matches!(admit("plain text", 0), Admission::Miss));
    }

    #[test]
    fn command_error_results_carry_the_settled_shape() {
        let value = command_error("boom");
        assert_eq!(value["result"]["kind"], "error");
        assert_eq!(value["result"]["text"], "boom");
        assert!(value["commandId"].as_str().is_some_and(|id| !id.is_empty()));
    }
}
