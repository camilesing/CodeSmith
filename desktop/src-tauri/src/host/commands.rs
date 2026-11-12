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
//! `execute`): parse the line, resolve the name, refuse attachments unless
//! the entry declares `input.attachments` (`/plan` is the one that does),
//! refuse or demand arguments — each refusal settles as a command error
//! result, exactly the sentences the upstream `command-compact` plugin
//! produces.
//!
//! The mode commands bridge the runtime's per-thread `mode` field
//! (`PATCH /v1/threads/{id}`, `AppMode::from_setting` spellings): `/mode`
//! takes the mode name as its argument, `/plan` and `/yolo` are the REPL's
//! own shortcuts for the two most-used switches. `/plan <message>` enters
//! plan and hands the message (with composer attachments) to the runtime
//! as an upstream `agent.steer`: a running turn takes a steer, an idle
//! session starts a turn. `/model` needs no catalog row — the vendored
//! client registers it as a client contribution whose popup drives
//! `session/selectModel` (already bridged).
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
            "definitionId": "@codesmith/plan-mode",
            "name": "plan",
            "description": "Enter or leave plan mode",
            "input": { "hint": "[off|message]", "attachments": true },
        },
        {
            "definitionId": "@codesmith/permission-presets",
            "name": "permission",
            "description": "Switch this session's permission preset (sandbox + approval)",
            "input": { "hint": "workspace-write | danger-full-access" },
        },
        {
            "definitionId": "@codesmith/command-goal",
            "name": "goal",
            "description": "Create or replace this session's goal",
            "input": { "hint": "<objective> [budget: N]" },
        },
        {
            "name": "yolo",
            "description": "YOLO mode — full tool access, no approvals",
        },
        {
            "definitionId": "@codesmith/command-feedback",
            "name": "feedback",
            "description": "Record feedback for this session",
            "input": { "hint": "<text>" },
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

/// The REPL's `[budget: N]` suffix parser (port of `parse_hunt_budget`): the
/// marker only matches at a word boundary, and a marker whose number fails to
/// parse denies the whole invocation (an empty objective) rather than sending
/// `budget: ten` to the runtime as part of the objective.
fn parse_goal_budget(text: &str) -> (String, Option<u32>) {
    const MARKER: &str = "budget:";
    if let Some(index) = text
        .to_ascii_lowercase()
        .rfind(MARKER)
        .filter(|index| *index == 0 || text[..*index].ends_with(char::is_whitespace))
    {
        let head = text[..index].trim_end();
        let tail = text[index + MARKER.len()..].trim_start();
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(budget) = digits.parse::<u32>() {
            return (head.trim().to_string(), Some(budget));
        }
        return (String::new(), None);
    }
    (text.trim().to_string(), None)
}

/// One admitted invocation: the resolved command and its settled argument.
#[derive(Debug)]
enum Admitted {
    Compact,
    /// A `/mode` (or shortcut) switch.
    Mode(&'static str),
    /// `/plan off`: leave plan and restore the permission mode it came from.
    PlanOff,
    /// `/plan [message]`: enter plan, optionally steering a message (and its
    /// attachments) as the next request — upstream plan-mode's own handler.
    PlanMessage {
        message: String,
        attachments: Vec<Value>,
    },
    /// A permission-preset switch; the value is the thread mode it writes.
    Permission(&'static str),
    /// `/goal <objective> [budget: N]`: create or replace the runtime goal.
    Goal {
        objective: String,
        token_budget: Option<u32>,
    },
    /// `/feedback <text>`: record one session-level remark.
    Feedback(String),
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
/// admission, argument usage — in the registry's order). `/mode` and
/// `/permission` demand their argument; `/compact` and `/yolo` refuse one;
/// `/plan` admits an optional message and composer attachments.
fn admit(line: &str, attachments: &[Value]) -> Admission {
    let Some((name, raw_input)) = parse_command(line) else {
        return Admission::Miss;
    };
    if !attachments.is_empty()
        && matches!(
            name,
            "compact" | "mode" | "permission" | "yolo" | "goal" | "feedback"
        )
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
            // `/plan` enters the separate plan state; `/plan off` leaves it
            // and restores the permission mode the session came from; any
            // other text enters plan and steers that message (with its
            // attachments) as the next request — upstream's handler.
            match raw_input.trim() {
                "" => Admission::Admitted(if attachments.is_empty() {
                    Admitted::Mode("plan")
                } else {
                    Admitted::PlanMessage {
                        message: String::new(),
                        attachments: attachments.to_vec(),
                    }
                }),
                "off" => {
                    if !attachments.is_empty() {
                        return Admission::Error("Attachments cannot accompany /plan off.".into());
                    }
                    Admission::Admitted(Admitted::PlanOff)
                }
                message => Admission::Admitted(Admitted::PlanMessage {
                    message: message.to_string(),
                    attachments: attachments.to_vec(),
                }),
            }
        }
        "permission" => match permissions::mode_of_preset(raw_input.trim()) {
            Some(mode) => Admission::Admitted(Admitted::Permission(mode)),
            None => Admission::Error(format!(
                "Usage: /permission <{}>",
                permissions::PRESETS.join("|")
            )),
        },
        "yolo" => {
            if !raw_input.trim().is_empty() {
                return Admission::Error("Usage: /yolo (no arguments)".into());
            }
            Admission::Admitted(Admitted::Mode("yolo"))
        }
        "goal" => {
            let (objective, token_budget) = parse_goal_budget(raw_input);
            if objective.is_empty() {
                return Admission::Error("Usage: /goal <objective> [budget: N]".into());
            }
            // The GoalBar owns pause/resume/clear; a goal literally named
            // "pause" would be a surprise, so point at the bar instead.
            if matches!(objective.as_str(), "pause" | "resume" | "clear") {
                return Admission::Error(
                    "Use the GoalBar to pause, resume, or clear a goal.".into(),
                );
            }
            Admission::Admitted(Admitted::Goal {
                objective,
                token_budget,
            })
        }
        "feedback" => {
            let text = raw_input.trim();
            if text.is_empty() {
                return Admission::Error(
                    "Usage: /feedback <text> (or open the feedback dialog from the / menu)".into(),
                );
            }
            Admission::Admitted(Admitted::Feedback(text.to_string()))
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
pub async fn execute(host: &super::SharedHost, args: &Value) -> Result<Outcome, OpError> {
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
    let submitted_attachments = args
        .get("submittedAttachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    match admit(line, &submitted_attachments) {
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
                let text = if mode == "plan" {
                    "Plan mode on. Use /plan off to leave.".to_string()
                } else {
                    format!("Mode set to {mode}.")
                };
                settle_mode_switch(host, &session_id, mode, text).await
            }
            Admitted::Permission(mode) => {
                // Plan is a separate state, not a permission: switching the
                // sandbox/approval bundle would silently leave it. Make the
                // user leave plan explicitly, matching the general-agent model where
                // the two controls are independent.
                if sessions::current_mode(host, &session_id).await.as_deref() == Some("plan") {
                    return Ok(Outcome::Executed(command_error(
                        "Plan mode is active; run /plan off before switching permission presets.",
                    )));
                }
                let preset = permissions::preset_of_mode(mode);
                settle_mode_switch(
                    host,
                    &session_id,
                    mode,
                    format!("Permission set to {preset}."),
                )
                .await
            }
            Admitted::PlanOff => {
                match sessions::leave_plan(host, &session_id)
                    .await
                    .map_err(internal)?
                {
                    Ok(sessions::PlanExit::Left) => {
                        sessions::push_control_projection(host, &session_id).await;
                        Ok(Outcome::Executed(command_success("Plan mode off.")))
                    }
                    Ok(sessions::PlanExit::NotActive) => Ok(Outcome::Executed(command_success(
                        "Plan mode is already inactive.",
                    ))),
                    Err((_status, runtime_message)) => {
                        Ok(Outcome::Executed(command_error(&runtime_message)))
                    }
                }
            }
            Admitted::PlanMessage {
                message,
                attachments,
            } => {
                // Resolve the submitted attachments before touching the
                // thread: a bad receipt or image is an admission failure and
                // must leave the session in its previous mode. The runtime
                // registry admits durable blocks before the handler runs for
                // the same reason.
                let runtime_attachments =
                    match command_attachments(host, &session_id, &attachments).await {
                        Ok(attachments) => attachments,
                        Err(message) => {
                            return Ok(Outcome::Executed(command_error(&message)));
                        }
                    };
                // Enter plan, then hand the message to the runtime the way
                // upstream's command handler calls `agent.steer`: a running
                // turn takes a steer, an idle session starts a turn.
                match sessions::set_mode(host, &session_id, "plan")
                    .await
                    .map_err(internal)?
                {
                    Ok(()) => {}
                    Err((_status, runtime_message)) => {
                        return Ok(Outcome::Executed(command_error(&runtime_message)));
                    }
                }
                if !message.is_empty() || !runtime_attachments.is_empty() {
                    if let Err(err) = sessions::deliver_prompt(
                        host,
                        &session_id,
                        &message,
                        &runtime_attachments,
                        None,
                        sessions::Delivery::Auto,
                    )
                    .await
                    {
                        return Ok(Outcome::Executed(command_error(&format!(
                            "Plan message failed: {err:#}"
                        ))));
                    }
                    let receipt_ids: Vec<String> = attachments
                        .iter()
                        .filter(|item| item.get("type").and_then(Value::as_str) == Some("file"))
                        .filter_map(|item| {
                            item.get("receiptId")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .collect();
                    super::uploads::retire_receipts(&host.uploads, &receipt_ids).await;
                }
                sessions::push_control_projection(host, &session_id).await;
                Ok(Outcome::Executed(command_success(
                    "Plan mode on. Use /plan off to leave.",
                )))
            }
            Admitted::Goal {
                objective,
                token_budget,
            } => {
                // The runtime's goal route creates a goal when none exists
                // (`GoalState::edit` calls `create` for a new objective),
                // loads an evicted engine from the journal first, and shares
                // the state the round driver reads. A refusal (blank
                // objective) settles as the command's error result.
                let path = format!("/v1/threads/{session_id}/goal");
                let mut body = json!({ "action": "edit", "objective": &objective });
                if let Some(budget) = token_budget {
                    body["token_budget"] = json!(budget);
                }
                match host
                    .backend
                    .post_with_error_message::<Value>(&path, &body)
                    .await
                    .map_err(internal)?
                {
                    Ok(_) => {
                        // A successful edit supersedes a desktop pause/clear
                        // and persists the snapshot an eviction replays.
                        super::goals::note_edit(host, &session_id, &objective, token_budget).await;
                        super::goals::push(host, &session_id).await;
                        Ok(Outcome::Executed(command_success("Goal set.")))
                    }
                    Err(runtime_message) => Ok(Outcome::Executed(command_error(&runtime_message))),
                }
            }
            Admitted::Feedback(text) => {
                match super::feedback::record_text(host, &session_id, &text).await {
                    Ok(()) => Ok(Outcome::Executed(command_success("Feedback recorded."))),
                    Err(message) => Ok(Outcome::Executed(command_error(&message))),
                }
            }
        },
    }
}

/// Resolve a command invocation's submitted attachments — inline images and
/// staged file receipts — into runtime turn attachments, preserving
/// submission order. A bad entry fails the whole invocation with the
/// helper's own message; the caller owns receipt retirement after the
/// runtime accepts the prompt.
async fn command_attachments(
    host: &HostState,
    session_id: &str,
    submitted: &[Value],
) -> Result<Vec<Value>, String> {
    let mut resolved = Vec::new();
    for item in submitted {
        match item.get("type").and_then(Value::as_str) {
            Some("image") => {
                let attachment = super::uploads::inline_image_attachment(
                    item.get("mediaType").and_then(Value::as_str),
                    item.get("data").and_then(Value::as_str),
                    item.get("name").and_then(Value::as_str),
                )
                .map_err(|err| err.message)?;
                resolved.push(attachment);
            }
            Some("file") => {
                let receipt_id =
                    item.get("receiptId")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            "commands/execute: file attachment without receiptId".to_string()
                        })?;
                let mut item = super::uploads::resolve_prompt_attachments(
                    &host.uploads,
                    session_id,
                    &[receipt_id.to_string()],
                )
                .await
                .map_err(|err| err.message)?;
                resolved.append(&mut item);
            }
            other => {
                return Err(format!(
                    "commands/execute: unknown attachment type {other:?}"
                ))
            }
        }
    }
    Ok(resolved)
}

/// A settled `kind:'success'` execution in the upstream shape.
fn command_success(text: &str) -> Value {
    json!({
        "commandId": mint_command_id(),
        "result": { "kind": "success", "text": text }
    })
}

/// Write one mode and publish the control projections the chips read now,
/// instead of waiting for the next settled turn. A runtime refusal (unknown
/// spelling, busy write) settles as the command's error result, quoting the
/// runtime's text.
async fn settle_mode_switch(
    host: &HostState,
    session_id: &str,
    mode: &str,
    success: String,
) -> Result<Outcome, OpError> {
    let patched = sessions::set_mode(host, session_id, mode)
        .await
        .map_err(internal)?;
    match patched {
        Ok(()) => {
            sessions::push_control_projection(host, session_id).await;
            Ok(Outcome::Executed(command_success(&success)))
        }
        Err((_status, runtime_message)) => Ok(Outcome::Executed(command_error(&runtime_message))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One submitted attachment, enough for admission's "carries attachments"
    /// gate; the runtime resolution path is exercised through dispatch tests.
    fn attached() -> [Value; 1] {
        [json!({ "type": "file", "receiptId": "receipt-1" })]
    }

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
        assert_eq!(
            names,
            vec![
                "compact",
                "mode",
                "plan",
                "permission",
                "goal",
                "yolo",
                "feedback"
            ]
        );
        let compact = &list[0];
        assert_eq!(compact["definitionId"], "@codesmith/command-compact");
        // `name` and `description` are the descriptor's required wire
        // fields; the client's zod array rejects an entry without them.
        for entry in list.as_array().unwrap() {
            assert!(!entry["description"].as_str().unwrap_or_default().is_empty());
        }
        // The mode entry advertises its argument so capable composers hint it.
        assert_eq!(
            list[1]["input"]["hint"],
            "agent | plan | yolo | coordinator"
        );
        // The permission row advertises the two sandbox+approval bundles; the
        // vendored picker decorates this exact command name.
        assert_eq!(
            list[3]["input"]["hint"],
            "workspace-write | danger-full-access"
        );
        assert_eq!(list[2]["input"]["hint"], "[off|message]");
        assert_eq!(list[2]["input"]["attachments"], true);
        assert_eq!(list[4]["input"]["hint"], "<objective> [budget: N]");
        assert_eq!(list[6]["input"]["hint"], "<text>");
        // The builtin definition ids the vendored command input maps to its
        // localized spellings (`/计划`, `/权限`, `/目标`, `/反馈`). A missing
        // id silently disables those aliases.
        assert_eq!(list[2]["definitionId"], "@codesmith/plan-mode");
        assert_eq!(list[3]["definitionId"], "@codesmith/permission-presets");
        assert_eq!(list[4]["definitionId"], "@codesmith/command-goal");
        assert_eq!(list[6]["definitionId"], "@codesmith/command-feedback");
        // `/feedback` exists exactly once: the vendored message-feedback
        // plugin decorates this host row's bare invocation with its dialog,
        // it does not register a second contribution.
        assert_eq!(names.iter().filter(|name| **name == "feedback").count(), 1);
    }

    #[test]
    fn goal_admission_requires_an_objective_and_leaves_bar_verbs_alone() {
        assert!(matches!(
            admit("/goal ship the desktop bridge", &[]),
            Admission::Admitted(Admitted::Goal { objective, token_budget: None })
                if objective == "ship the desktop bridge"
        ));
        assert!(matches!(admit("/goal", &[]), Admission::Error(_)));
        for verb in ["pause", "resume", "clear"] {
            assert!(
                matches!(admit(&format!("/goal {verb}"), &[]), Admission::Error(_)),
                "/goal {verb} must point at the GoalBar, not create a goal"
            );
        }
        assert!(matches!(
            admit("/goal ship it", &attached()),
            Admission::Error(_)
        ));
    }

    #[test]
    fn goal_budget_suffix_parses_at_a_word_boundary() {
        assert_eq!(
            parse_goal_budget("ship the bridge budget: 400"),
            ("ship the bridge".to_string(), Some(400))
        );
        assert_eq!(
            parse_goal_budget("ship budget:1200"),
            ("ship".to_string(), Some(1200))
        );
        // A marker inside a word is objective text, not a budget.
        assert_eq!(
            parse_goal_budget("overbudget: 5"),
            ("overbudget: 5".to_string(), None)
        );
        // A present but unparseable marker denies the invocation instead of
        // sending "budget: ten" to the runtime as the objective.
        assert_eq!(
            parse_goal_budget("ship it budget: ten"),
            (String::new(), None)
        );
        assert!(matches!(
            admit("/goal ship it budget: ten", &[]),
            Admission::Error(text) if text.contains("budget: N")
        ));
        assert!(matches!(
            admit("/goal ship it budget: 250", &[]),
            Admission::Admitted(Admitted::Goal { objective, token_budget: Some(250) })
                if objective == "ship it"
        ));
    }

    #[test]
    fn feedback_admission_requires_text() {
        assert!(matches!(
            admit("/feedback the tool row was unclear", &[]),
            Admission::Admitted(Admitted::Feedback(text)) if text == "the tool row was unclear"
        ));
        assert!(matches!(admit("/feedback", &[]), Admission::Error(_)));
        assert!(matches!(admit("/feedback  ", &[]), Admission::Error(_)));
        assert!(matches!(
            admit("/feedback note", &attached()),
            Admission::Error(_)
        ));
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
            admit("/compact", &[]),
            Admission::Admitted(Admitted::Compact)
        ));
        // An attachment settles as a command error before the usage check.
        let Admission::Error(text) = admit("/compact", &attached()) else {
            panic!("an attachment-carrying invocation must refuse");
        };
        assert_eq!(text, "/compact does not accept attachments");
        let Admission::Error(text) = admit("/compact keep this", &[]) else {
            panic!("an argued invocation must refuse");
        };
        assert_eq!(text, "Usage: /compact (no arguments)");
        // Unknown names and non-command syntax are admission misses.
        assert!(matches!(admit("/model", &[]), Admission::Miss));
        assert!(matches!(admit("/compact/", &[]), Admission::Miss));
        assert!(matches!(admit("plain text", &[]), Admission::Miss));
    }

    #[test]
    fn mode_commands_admit_their_spellings_and_refuse_everything_else() {
        for (line, mode) in [
            ("/mode agent", "agent"),
            ("/mode plan", "plan"),
            ("/mode yolo", "yolo"),
            ("/mode coordinator", "coordinator"),
            ("/plan", "plan"),
            ("/yolo", "yolo"),
        ] {
            match admit(line, &[]) {
                Admission::Admitted(Admitted::Mode(admitted)) => assert_eq!(admitted, mode),
                other => panic!("{line} must admit mode {mode}, got {other:?}"),
            }
        }
        assert!(matches!(
            admit("/plan off", &[]),
            Admission::Admitted(Admitted::PlanOff)
        ));
        for (line, mode) in [
            ("/permission workspace-write", "agent"),
            ("/permission danger-full-access", "yolo"),
            // Legacy spellings keep working for muscle memory from the
            // mode-shaped picker.
            ("/permission agent", "agent"),
            ("/permission yolo", "yolo"),
        ] {
            match admit(line, &[]) {
                Admission::Admitted(Admitted::Permission(admitted)) => {
                    assert_eq!(admitted, mode, "{line}");
                }
                other => panic!("{line} must admit permission {mode}, got {other:?}"),
            }
        }
        // A bare or misspelled /mode fails loud with the valid spellings.
        let Admission::Error(text) = admit("/mode", &[]) else {
            panic!("a bare /mode must refuse");
        };
        assert_eq!(text, "Usage: /mode <agent|plan|yolo|coordinator>");
        let Admission::Error(text) = admit("/mode full-access", &[]) else {
            panic!("a misspelled mode must refuse");
        };
        assert_eq!(text, "Usage: /mode <agent|plan|yolo|coordinator>");
        let Admission::Error(text) = admit("/permission", &[]) else {
            panic!("a bare /permission must refuse");
        };
        assert_eq!(
            text,
            "Usage: /permission <workspace-write|danger-full-access>"
        );
        // Plan and coordinator are not permissions.
        for line in [
            "/permission plan",
            "/permission coordinator",
            "/permission read-only",
        ] {
            let Admission::Error(text) = admit(line, &[]) else {
                panic!("{line} must refuse a non-permission state");
            };
            assert_eq!(
                text,
                "Usage: /permission <workspace-write|danger-full-access>"
            );
        }
        // `/plan <message>` is upstream's steer-into-plan gesture; it admits
        // an argument. `/yolo` is a plain shortcut and refuses one.
        let Admission::Error(text) = admit("/yolo now", &[]) else {
            panic!("/yolo must refuse its argument");
        };
        assert!(text.starts_with("Usage: /"), "{text}");
        match admit("/plan now", &[]) {
            Admission::Admitted(Admitted::PlanMessage {
                message,
                attachments,
            }) => {
                assert_eq!(message, "now");
                assert!(attachments.is_empty());
            }
            other => panic!("/plan <message> must admit a plan message, got {other:?}"),
        }
        match admit("/plan", &attached()) {
            Admission::Admitted(Admitted::PlanMessage {
                message,
                attachments,
            }) => {
                assert!(message.is_empty());
                assert_eq!(attachments.len(), 1);
            }
            other => panic!("/plan with attachments must admit a plan message, got {other:?}"),
        }
        let Admission::Error(text) = admit("/plan off", &attached()) else {
            panic!("/plan off with an attachment must refuse");
        };
        assert_eq!(text, "Attachments cannot accompany /plan off.");
        for line in ["/yolo", "/mode plan", "/permission workspace-write"] {
            let Admission::Error(text) = admit(line, &attached()) else {
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
