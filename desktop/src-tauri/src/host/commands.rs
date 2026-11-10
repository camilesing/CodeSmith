//! `commands/list` + `commands/execute` — the '/' command catalog: the
//! runtime's manual compaction route plus its per-thread mode switches.
//!
//! Wire contracts (vendored `api-remotes`): the client sends
//! `list {agentId}` and `execute {agentId, line, submittedAttachments}`
//! (the AbortSignal maps to HTTP cancellation, not a field), and reads
//! `execute`'s result without strict decoding — so an admission miss is
//! `{"ok":true}` with NO `value` member (a present `null` would crash the
//! client's `result.value.result` read), while an admitted execution is
//! `{commandId, result:{kind…}}`.
//!
//! Admission mirrors the upstream registry's order (the commands registry's
//! `execute`): parse the line, resolve the name, refuse attachments
//! (no entry declares `input.attachments`), refuse or demand arguments —
//! each refusal settles as a command error result, exactly the sentences
//! the upstream `command-compact` plugin produces.
//!
//! The mode commands bridge the runtime's per-thread `mode` field
//! (`PATCH /v1/threads/{id}`, `AppMode::from_setting` spellings): `/mode`
//! takes the mode name as its argument, `/plan` and `/yolo` are the REPL's
//! own shortcuts for the two most-used switches. `/model` needs no catalog
//! row — the vendored client registers it as a client contribution whose
//! popup drives `session/selectModel` (already bridged).
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

use super::permissions;
use super::sessions;
use super::workspace::OpError;
use super::HostState;

/// The vendored builtin definition id for `/compact` — the rebrand of the
/// upstream command-compact plugin (provenance in `ui/vendor/VENDOR.json`).
/// The client's builtin table matches it, giving the menu row its localized
/// face and resolving the zh alias 压缩 to this command.
const COMPACT_DEFINITION_ID: &str = "@codesmith/command-compact";

/// The runtime's per-thread mode spellings (`AppMode::from_setting`;
/// `coordinator_mode` stays an alias only the CLI accepts — one spelling
/// per row keeps the menu honest).
const MODES: [&str; 4] = ["agent", "plan", "yolo", "coordinator"];

/// The thread PATCH a mode switch sends. `yolo` and `coordinator` require
/// the thread's auto-approve flag — the prompt assembly asserts that pairing
/// (a yolo/coordinator turn with `auto_approve` off panics in debug builds
/// and would describe approval behavior the runtime does not follow). Every
/// other mode clears it: entering `plan` from `yolo` must still ask the user
/// to approve `exit_plan_mode`. The shell default is untouched (the bridge
/// never widens it).
pub(super) fn mode_patch(mode: &str) -> Value {
    json!({
        "mode": mode,
        "auto_approve": matches!(mode, "yolo" | "coordinator"),
    })
}

/// `commands/list` — the host catalog: compaction (upstream's own host
/// registers this exact name and description) plus CodeSmith's per-thread
/// mode switches.
pub fn catalog() -> Value {
    json!([
        {
            "definitionId": COMPACT_DEFINITION_ID,
            "name": "compact",
            "description": "Compact older conversation history",
        },
        {
            "name": "mode",
            "description": "Switch this session's mode (agent, plan, yolo, coordinator)",
            "input": { "hint": "agent | plan | yolo | coordinator" },
        },
        {
            "name": "plan",
            "description": "Plan mode — design before implementing",
        },
        {
            "name": "permission",
            "description": "Switch this session's permission preset",
            "input": { "hint": "agent | plan | danger-full-access | coordinator" },
        },
        {
            "name": "yolo",
            "description": "YOLO mode — full tool access, no approvals",
        },
    ])
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

/// One admitted invocation: the resolved command and its settled argument.
#[derive(Debug)]
enum Admitted {
    Compact,
    /// The mode value (already validated against [`MODES`]).
    Mode(&'static str),
}

/// The upstream registry's admission decision for a line, before any
/// runtime call.
#[derive(Debug)]
enum Admission {
    Miss,
    /// A settled `kind:'error'` result (attachment or usage refusal).
    Error(String),
    Admitted(Admitted),
}

/// Admit one invocation against the catalog (name resolution, attachment
/// refusal, argument usage — in the registry's order). `/mode` demands its
/// argument; `/compact`, `/plan` and `/yolo` refuse one.
fn admit(line: &str, attachment_count: usize) -> Admission {
    let Some((name, raw_input)) = parse_command(line) else {
        return Admission::Miss;
    };
    if attachment_count > 0
        && matches!(name, "compact" | "mode" | "plan" | "permission" | "yolo")
    {
        return Admission::Error(format!("/{name} does not accept attachments"));
    }
    match name {
        "compact" => {
            if !raw_input.trim().is_empty() {
                return Admission::Error("Usage: /compact (no arguments)".into());
            }
            Admission::Admitted(Admitted::Compact)
        }
        "mode" => {
            let value = raw_input.trim();
            match MODES.iter().find(|candidate| **candidate == value) {
                Some(mode) => Admission::Admitted(Admitted::Mode(mode)),
                None => Admission::Error(format!("Usage: /mode <{}>", MODES.join("|"))),
            }
        }
        "plan" => {
            // The plan chip exits through `/plan off`; a bare `/plan` enters
            // the mode. Anything else is a usage error, not a mode switch.
            match raw_input.trim() {
                "" => Admission::Admitted(Admitted::Mode("plan")),
                "off" => Admission::Admitted(Admitted::Mode("agent")),
                _ => Admission::Error("Usage: /plan [off]".into()),
            }
        }
        "permission" => {
            match permissions::mode_of_preset(raw_input.trim()) {
                Some(mode) => Admission::Admitted(Admitted::Mode(mode)),
                None => Admission::Error(format!(
                    "Usage: /permission <{}>",
                    permissions::PRESETS.join("|")
                )),
            }
        }
        "yolo" => {
            if !raw_input.trim().is_empty() {
                return Admission::Error("Usage: /yolo (no arguments)".into());
            }
            Admission::Admitted(Admitted::Mode("yolo"))
        }
        _ => Admission::Miss,
    }
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

/// `commands/execute` — admit the line, then hand an admitted invocation to
/// the runtime: compaction to the manual compact route, a mode switch to the
/// thread PATCH.
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
        Admission::Admitted(admitted) => match admitted {
            Admitted::Compact => {
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
            Admitted::Mode(mode) => {
                // The mode switch settles synchronously in the thread record;
                // a runtime refusal (unknown spelling, busy write) settles as
                // the command's error result, quoting the runtime's text.
                // `sessions::set_mode` also keeps the plan-return stash, so a
                // later plan approval hands the session back to this mode.
                let patched = sessions::set_mode(host, &session_id, mode)
                    .await
                    .map_err(internal)?;
                match patched {
                    Ok(()) => {
                        // The plan/permission chips read the control
                        // projections; publish the new mode now instead of
                        // waiting for the next settled turn.
                        sessions::push_control_projection(host, &session_id).await;
                        Ok(Outcome::Executed(json!({
                            "commandId": mint_command_id(),
                            "result": { "kind": "success", "text": format!("Mode set to {mode}.") }
                        })))
                    }
                    Err((_status, runtime_message)) => {
                        Ok(Outcome::Executed(command_error(&runtime_message)))
                    }
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_switches_keep_the_auto_approve_pairing() {
        // The runtime's prompt assembly asserts this pairing; a yolo thread
        // with auto_approve off panics in debug builds.
        assert_eq!(
            mode_patch("yolo"),
            json!({ "mode": "yolo", "auto_approve": true })
        );
        assert_eq!(
            mode_patch("coordinator"),
            json!({ "mode": "coordinator", "auto_approve": true })
        );
        // Leaving yolo for agent/plan must make plan approval ask again.
        assert_eq!(
            mode_patch("agent"),
            json!({ "mode": "agent", "auto_approve": false })
        );
        assert_eq!(
            mode_patch("plan"),
            json!({ "mode": "plan", "auto_approve": false })
        );
    }

    #[test]
    fn catalog_serves_the_host_descriptors() {
        let list = catalog();
        let names: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["compact", "mode", "plan", "permission", "yolo"]);
        let compact = &list[0];
        assert_eq!(compact["definitionId"], "@codesmith/command-compact");
        // `name` and `description` are the descriptor's required wire
        // fields; the client's zod array rejects an entry without them.
        for entry in list.as_array().unwrap() {
            assert!(!entry["description"].as_str().unwrap_or_default().is_empty());
        }
        // The mode entry advertises its argument so capable composers hint it.
        assert_eq!(
            list[1]["input"]["hint"], "agent | plan | yolo | coordinator"
        );
        // The permission row advertises the picker's preset values; the
        // vendored picker decorates this exact command name.
        assert_eq!(
            list[3]["input"]["hint"],
            "agent | plan | danger-full-access | coordinator"
        );
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
    fn admission_mirrors_the_registry_order() {
        assert!(matches!(
            admit("/compact", 0),
            Admission::Admitted(Admitted::Compact)
        ));
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
    fn mode_commands_admit_their_spellings_and_refuse_everything_else() {
        for (line, mode) in [
            ("/mode agent", "agent"),
            ("/mode plan", "plan"),
            ("/mode yolo", "yolo"),
            ("/mode coordinator", "coordinator"),
            ("/plan", "plan"),
            ("/plan off", "agent"),
            ("/permission agent", "agent"),
            ("/permission plan", "plan"),
            ("/permission danger-full-access", "yolo"),
            ("/permission coordinator", "coordinator"),
            ("/permission yolo", "yolo"),
            ("/yolo", "yolo"),
        ] {
            match admit(line, 0) {
                Admission::Admitted(Admitted::Mode(admitted)) => assert_eq!(admitted, mode),
                other => panic!("{line} must admit mode {mode}, got {other:?}"),
            }
        }
        // A bare or misspelled /mode fails loud with the valid spellings.
        let Admission::Error(text) = admit("/mode", 0) else {
            panic!("a bare /mode must refuse");
        };
        assert_eq!(text, "Usage: /mode <agent|plan|yolo|coordinator>");
        let Admission::Error(text) = admit("/mode full-access", 0) else {
            panic!("a misspelled mode must refuse");
        };
        assert_eq!(text, "Usage: /mode <agent|plan|yolo|coordinator>");
        let Admission::Error(text) = admit("/permission", 0) else {
            panic!("a bare /permission must refuse");
        };
        assert_eq!(
            text,
            "Usage: /permission <agent|plan|danger-full-access|coordinator>"
        );
        // The shortcuts refuse arguments and attachments like /compact.
        for line in ["/plan now", "/yolo now"] {
            let Admission::Error(text) = admit(line, 0) else {
                panic!("{line} must refuse its argument");
            };
            assert!(text.starts_with("Usage: /"), "{text}");
        }
        for line in ["/plan", "/yolo", "/mode plan", "/permission plan"] {
            let Admission::Error(text) = admit(line, 1) else {
                panic!("{line} with an attachment must refuse");
            };
            assert!(text.ends_with("does not accept attachments"), "{text}");
        }
    }

    #[test]
    fn command_error_results_carry_the_settled_shape() {
        let value = command_error("boom");
        assert_eq!(value["result"]["kind"], "error");
        assert_eq!(value["result"]["text"], "boom");
        assert!(value["commandId"].as_str().is_some_and(|id| !id.is_empty()));
    }
}
