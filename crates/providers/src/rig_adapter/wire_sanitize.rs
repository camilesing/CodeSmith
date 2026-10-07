//! Outbound wire sanitization — the last line of defense between CodeSmith's
//! mutable conversation history and strict OpenAI-compatible providers.
//!
//! The engine mutates its transcript in several places (emergency hard-trim,
//! VerifyAndReplan reset, local trims, `/undo`, extension transforms, …), and
//! not every mutation keeps the OpenAI tool-call/tool-result pairing intact.
//! Lenient providers shrug; strict ones reject the whole request — GLM does so
//! mid-session with 400 1214 "messages 参数非法", killing benchmark runs that
//! had otherwise been running for hours.
//!
//! [`sanitize_wire_messages`] runs on the fully-converted rig message list in
//! `build_request` — after the provider shaper, before the last message is
//! popped as the prompt — for every rig-backed provider. It enforces,
//! idempotently and only on broken shapes:
//!
//! 1. every `ToolResult` answers a `ToolCall` **earlier** in the list
//!    (orphans and results preceding their call are dropped, whole
//!    messages included);
//! 2. a call id is answered at most once and requests at most once
//!    (duplicate results *and* duplicate calls keep the latest occurrence —
//!    the freshest information — so a reused id never travels twice);
//! 3. every `ToolCall` has a later result somewhere in the list (dangling
//!    calls are stripped; a message left without content is dropped);
//! 4. adjacent same-role messages merge — except user batches containing tool
//!    results, because rig's OpenAI serializer partitions user content and
//!    silently drops text when tool results are present, so folding a text
//!    batch into a tool-result batch would lose the text;
//! 5. empty text never travels as the sole content: empty text items beside
//!    tool calls are dropped, a message left without any content gets a
//!    one-space placeholder, and system messages cannot be empty;
//! 6. tool results whose content is entirely empty get an `(empty output)`
//!    placeholder;
//! 7. the conversation contains at least one plain user text message. GLM
//!    rejects a userless history outright — after an emergency front-trim
//!    removed every leading user text, `[system, assistant, tool, …]` still
//!    failed with 1214 "messages 参数非法" while replaying the byte-identical
//!    body with one user note inserted after the system prompt succeeded.
//!    A synthetic continuation note is inserted behind the leading system
//!    block when none survives. (Mixed user messages don't count: rig's
//!    OpenAI serializer drops text from a user message that also carries
//!    tool results, so only a tool-result-free user message produces a
//!    `role:"user"` message on the wire.)
//!
//! Reasoning items are never touched here: the provider shaper owns the
//! strip/inject decision (#1542 / #1739), and DeepSeek thinking-mode requires
//! reasoning on assistant turns. Valid histories pass through unchanged.

use std::collections::HashSet;

use rig_core::OneOrMany;
use rig_core::completion::Message as RigMessage;
use rig_core::completion::message::{AssistantContent, ToolResultContent, UserContent};

/// The synthetic user note inserted by invariant 7. Proven accepted by GLM in
/// the forensic replay (probe B).
const CONTINUATION_NOTE: &str = "[context note] Earlier conversation context is no longer \
 included in this request. Continue the task from the latest messages below.";

/// Sweep the message list into a wire-valid shape. See the module docs for
/// the enforced invariants. Runs to a fixpoint — dropping or merging items
/// can expose further violations in degenerate histories — bounded by
/// [`FIXPOINT_PASSES`]: every changed pass strictly shrinks the list or its
/// items (placeholder items are exempt from the empty filter, so a second
/// pass over a placeholder is a no-op and the bound is only a guard against
/// logic slips, not part of the convergence argument).
pub(crate) fn sanitize_wire_messages(messages: &mut Vec<RigMessage>) {
    let mut changed_any = false;
    for _ in 0..FIXPOINT_PASSES {
        let mut changed = repair_tool_pairs(messages);
        changed |= normalize_empty_content(messages);
        changed |= merge_adjacent_same_role(messages);
        changed_any |= changed;
        if !changed {
            break;
        }
    }
    // Insert-only, no interplay with the fixpoint passes above.
    let note_inserted = ensure_user_text_message(messages);
    if changed_any || note_inserted {
        // Aggregate counts only — never content. When the model suddenly
        // behaves differently after an emergency trim or `/undo`, this line
        // is the evidence that its history was rewritten on the wire.
        tracing::warn!(
            target: "codesmith_wire_sanitize",
            messages = messages.len(),
            note_inserted,
            "rewrote outbound history to satisfy wire invariants"
        );
    }
}

/// Upper bound on fixpoint iterations. Each changing pass removes at least one
/// item, so this comfortably exceeds anything a real history needs.
const FIXPOINT_PASSES: usize = 8;

/// Tool-call ids requested by an assistant message.
fn call_ids_of(msg: &RigMessage) -> Vec<String> {
    match msg {
        RigMessage::Assistant { content, .. } => content
            .iter()
            .filter_map(|item| match item {
                AssistantContent::ToolCall(call) => Some(call.id.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Tool-result ids answered by a user message.
fn result_ids_of(msg: &RigMessage) -> Vec<String> {
    match msg {
        RigMessage::User { content } => content
            .iter()
            .filter_map(|item| match item {
                UserContent::ToolResult(result) => Some(result.id.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn user_has_tool_result(msg: &RigMessage) -> bool {
    match msg {
        RigMessage::User { content } => content
            .iter()
            .any(|item| matches!(item, UserContent::ToolResult(_))),
        _ => false,
    }
}

/// Enforce pairing invariants 1–3. In-place via take-and-restore so the
/// per-request sweep never deep-copies tool-result payloads. Liveness is
/// position-aware: a result lives only when its call sits at a strictly
/// earlier index, a call only when a (surviving) result sits strictly later
/// — a transcript mutation that reorders a pair (`/undo`, an extension
/// transform) must not ship the invalid ordering to a strict provider.
fn repair_tool_pairs(messages: &mut Vec<RigMessage>) -> bool {
    use std::collections::HashMap;

    let mut changed = false;
    let mut first_call_pos: HashMap<String, usize> = HashMap::new();
    for (idx, msg) in messages.iter().enumerate() {
        for id in call_ids_of(msg) {
            first_call_pos.entry(id).or_insert(idx);
        }
    }

    // Reverse walk over user messages: drop orphan results, keep only the
    // latest result per call id, and drop user messages left with no items.
    // Reverse order makes the dedupe keep the *last* occurrence of an id.
    let mut seen_results: HashSet<String> = HashSet::new();
    for idx in (0..messages.len()).rev() {
        let RigMessage::User { content } = &mut messages[idx] else {
            continue;
        };
        let items: Vec<UserContent> =
            std::mem::replace(content, OneOrMany::one(UserContent::text(String::new())))
                .into_iter()
                .collect();
        let original_len = items.len();
        let kept: Vec<UserContent> = items
            .into_iter()
            .filter(|item| match item {
                UserContent::ToolResult(result) => {
                    let live = first_call_pos
                        .get(&result.id)
                        .is_some_and(|&call_idx| call_idx < idx)
                        && seen_results.insert(result.id.clone());
                    changed |= !live;
                    live
                }
                _ => true,
            })
            .collect();
        if kept.is_empty() {
            messages.remove(idx);
            changed = true;
        } else {
            changed |= kept.len() != original_len;
            *content = OneOrMany::many(kept).expect("kept user content is non-empty");
        }
    }

    // Drop dangling calls: a `ToolCall` without a surviving later result is
    // stripped (OpenAI tool APIs only make sense with their result present);
    // duplicate calls keep the latest occurrence, mirroring the result
    // dedupe; an assistant message left with no items is dropped.
    let mut last_result_pos: HashMap<String, usize> = HashMap::new();
    for (idx, msg) in messages.iter().enumerate() {
        for id in result_ids_of(msg) {
            last_result_pos.insert(id, idx); // overwrite: keep the max
        }
    }
    let mut seen_calls: HashSet<String> = HashSet::new();
    for idx in (0..messages.len()).rev() {
        let RigMessage::Assistant { content, .. } = &mut messages[idx] else {
            continue;
        };
        let items: Vec<AssistantContent> = std::mem::replace(
            content,
            OneOrMany::one(AssistantContent::text(String::new())),
        )
        .into_iter()
        .collect();
        let kept: Vec<AssistantContent> = items
            .into_iter()
            .filter(|item| match item {
                AssistantContent::ToolCall(call) => {
                    let live = last_result_pos
                        .get(&call.id)
                        .is_some_and(|&result_idx| result_idx > idx)
                        && seen_calls.insert(call.id.clone());
                    changed |= !live;
                    live
                }
                _ => true,
            })
            .collect();
        if kept.is_empty() {
            messages.remove(idx);
            changed = true;
        } else {
            *content = OneOrMany::many(kept).expect("kept assistant content is non-empty");
        }
    }

    changed
}

/// Enforce content-shape invariants 5–6. The exact `" "` placeholder is
/// exempt from the empty-text filter so it survives every pass — without
/// the exemption it was dropped and re-inserted each round, and any
/// history tripping invariant 5 burned all [`FIXPOINT_PASSES`] without
/// converging.
fn normalize_empty_content(messages: &mut [RigMessage]) -> bool {
    /// The exact placeholder text is stable under this pass.
    const PLACEHOLDER: &str = " ";

    let mut changed = false;
    for msg in messages.iter_mut() {
        match msg {
            RigMessage::System { content } => {
                if content.trim().is_empty() && content.as_str() != PLACEHOLDER {
                    *content = PLACEHOLDER.to_string();
                    changed = true;
                }
            }
            RigMessage::Assistant { content, .. } => {
                let items: Vec<AssistantContent> = std::mem::replace(
                    content,
                    OneOrMany::one(AssistantContent::text(String::new())),
                )
                .into_iter()
                .collect();
                let original_len = items.len();
                let mut kept: Vec<AssistantContent> = items
                    .into_iter()
                    .filter(|item| match item {
                        AssistantContent::Text(text) => {
                            text.text.as_str() == PLACEHOLDER || !text.text.trim().is_empty()
                        }
                        _ => true,
                    })
                    .collect();
                changed |= kept.len() != original_len;
                // `content: ""` alone (or with only reasoning) on the wire is
                // at best noise and at worst a strict-provider 400; a
                // one-space placeholder keeps the message valid.
                if kept.is_empty() {
                    kept.push(AssistantContent::text(PLACEHOLDER.to_string()));
                    changed = true;
                }
                *content = OneOrMany::many(kept).expect("assistant content is non-empty");
            }
            RigMessage::User { content } => {
                let items: Vec<UserContent> =
                    std::mem::replace(content, OneOrMany::one(UserContent::text(String::new())))
                        .into_iter()
                        .collect();
                let mut kept: Vec<UserContent> = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        UserContent::Text(text) => {
                            let keep =
                                text.text.as_str() == PLACEHOLDER || !text.text.trim().is_empty();
                            changed |= !keep;
                            if keep {
                                kept.push(UserContent::Text(text));
                            }
                        }
                        mut other => {
                            if let UserContent::ToolResult(result) = &mut other {
                                let all_empty = result.content.iter().all(|c| match c {
                                    ToolResultContent::Text(text) => text.text.trim().is_empty(),
                                    _ => false,
                                });
                                if all_empty {
                                    result.content =
                                        OneOrMany::one(ToolResultContent::text("(empty output)"));
                                    changed = true;
                                }
                            }
                            kept.push(other);
                        }
                    }
                }
                if kept.is_empty() {
                    kept.push(UserContent::text(PLACEHOLDER.to_string()));
                    changed = true;
                }
                *content = OneOrMany::many(kept).expect("kept user content is non-empty");
            }
        }
    }
    changed
}

/// Enforce invariant 4: merge adjacent same-role messages.
fn merge_adjacent_same_role(messages: &mut Vec<RigMessage>) -> bool {
    let mut changed = false;
    let mut idx = 1;
    while idx < messages.len() {
        let mergeable = match (&messages[idx - 1], &messages[idx]) {
            (RigMessage::System { .. }, RigMessage::System { .. }) => true,
            (RigMessage::Assistant { .. }, RigMessage::Assistant { .. }) => true,
            (RigMessage::User { .. }, RigMessage::User { .. }) => {
                // Only fold text/image batches; see the module docs for why a
                // tool-result batch must stay separate from a text batch.
                !user_has_tool_result(&messages[idx - 1]) && !user_has_tool_result(&messages[idx])
            }
            _ => false,
        };
        if !mergeable {
            idx += 1;
            continue;
        }
        let next = messages.remove(idx);
        match (&mut messages[idx - 1], next) {
            (RigMessage::System { content: a }, RigMessage::System { content: b }) => {
                a.push_str("\n\n");
                a.push_str(&b);
            }
            (RigMessage::User { content: a }, RigMessage::User { content: b }) => {
                let items: Vec<UserContent> = a.iter().cloned().chain(b).collect();
                *a = OneOrMany::many(items).expect("merged user content is non-empty");
            }
            (
                RigMessage::Assistant { content: a, .. },
                RigMessage::Assistant { content: b, .. },
            ) => {
                let items: Vec<AssistantContent> = a.iter().cloned().chain(b).collect();
                *a = OneOrMany::many(items).expect("merged assistant content is non-empty");
            }
            _ => unreachable!("pair was checked mergeable"),
        }
        changed = true;
        // Stay on idx: the next message may merge with the combined one too.
    }
    changed
}

/// Enforce invariant 7: the history must carry at least one plain user text
/// message. After an emergency front-trim has removed every leading user
/// turn, the remaining `[system, assistant, tool, …]` shape is structurally
/// valid but GLM still rejects it with 1214 — replaying the identical body
/// with a single user note behind the system prompt succeeded (forensic
/// probes, 2026-10). Insert one when none survives.
fn ensure_user_text_message(messages: &mut Vec<RigMessage>) -> bool {
    let has_user_text = messages.iter().any(|msg| match msg {
        RigMessage::User { content } => {
            let has_text = content
                .iter()
                .any(|item| matches!(item, UserContent::Text(_)));
            // A message that also carries tool results serializes as
            // `role:"tool"` only — its text never reaches the wire.
            !user_has_tool_result(msg) && has_text
        }
        _ => false,
    });
    if has_user_text {
        return false;
    }
    let at = messages
        .iter()
        .take_while(|msg| matches!(msg, RigMessage::System { .. }))
        .count();
    messages.insert(
        at,
        RigMessage::User {
            content: OneOrMany::one(UserContent::text(CONTINUATION_NOTE.to_string())),
        },
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::{
        ToolCall as RigToolCall, ToolFunction, ToolResult as RigToolResult,
    };

    fn assistant_text(text: &str) -> RigMessage {
        RigMessage::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::text(text.to_string())),
        }
    }

    fn assistant_call(id: &str) -> RigMessage {
        RigMessage::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(RigToolCall::new(
                id.to_string(),
                ToolFunction::new("tool".to_string(), serde_json::json!({})),
            ))),
        }
    }

    fn assistant_text_and_call(text: &str, id: &str) -> RigMessage {
        RigMessage::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::text(text.to_string()),
                AssistantContent::ToolCall(RigToolCall::new(
                    id.to_string(),
                    ToolFunction::new("tool".to_string(), serde_json::json!({})),
                )),
            ])
            .expect("non-empty"),
        }
    }

    fn assistant_reasoning(text: &str) -> RigMessage {
        RigMessage::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::reasoning(text.to_string())),
        }
    }

    fn user_text(text: &str) -> RigMessage {
        RigMessage::User {
            content: OneOrMany::one(UserContent::text(text.to_string())),
        }
    }

    fn user_result(id: &str, content: &str) -> RigMessage {
        RigMessage::User {
            content: OneOrMany::one(UserContent::ToolResult(RigToolResult {
                id: id.to_string(),
                call_id: None,
                content: OneOrMany::one(ToolResultContent::text(content.to_string())),
            })),
        }
    }

    fn call_ids(msg: &RigMessage) -> Vec<String> {
        call_ids_of(msg)
    }

    fn result_ids(msg: &RigMessage) -> Vec<String> {
        result_ids_of(msg)
    }

    fn assistant_texts(msg: &RigMessage) -> Vec<String> {
        match msg {
            RigMessage::Assistant { content, .. } => content
                .iter()
                .filter_map(|item| match item {
                    AssistantContent::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn result_content(msg: &RigMessage, id: &str) -> Option<String> {
        match msg {
            RigMessage::User { content } => content.iter().find_map(|item| match item {
                UserContent::ToolResult(result) if result.id == id => {
                    result.content.iter().find_map(|c| match c {
                        ToolResultContent::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                }
                _ => None,
            }),
            _ => None,
        }
    }

    #[test]
    fn orphan_tool_result_is_dropped() {
        let mut messages = vec![
            user_text("go"),
            assistant_call("call_1"),
            user_result("call_1", "ok"),
            user_result("call_2", "orphan"),
        ];
        sanitize_wire_messages(&mut messages);
        assert_eq!(messages.len(), 3, "orphan result message must be dropped");
        assert_eq!(result_ids(&messages[2]), vec!["call_1".to_string()]);
    }

    #[test]
    fn duplicate_results_keep_the_latest() {
        let mut messages = vec![
            user_text("go"),
            assistant_call("call_1"),
            user_result("call_1", "original"),
            user_result("call_1", "[verification replay] conflict"),
        ];
        sanitize_wire_messages(&mut messages);
        assert_eq!(messages.len(), 3, "superseded result message must go");
        assert_eq!(
            result_content(&messages[2], "call_1").as_deref(),
            Some("[verification replay] conflict")
        );
    }

    #[test]
    fn dangling_call_message_is_dropped() {
        let mut messages = vec![user_text("hi"), assistant_call("call_9"), user_text("next")];
        sanitize_wire_messages(&mut messages);
        assert!(
            messages
                .iter()
                .all(|m| !matches!(m, RigMessage::Assistant { .. })),
            "assistant with only a dangling call must be dropped"
        );
    }

    #[test]
    fn dangling_call_is_stripped_but_text_is_kept() {
        let mut messages = vec![
            assistant_text_and_call("working on it", "call_9"),
            user_text("go on"),
        ];
        sanitize_wire_messages(&mut messages);
        assert_eq!(messages.len(), 2);
        assert_eq!(assistant_texts(&messages[0]), vec!["working on it"]);
        assert!(call_ids(&messages[0]).is_empty());
    }

    #[test]
    fn valid_history_passes_through_unchanged() {
        let messages = vec![
            user_text("do the thing"),
            assistant_text_and_call("on it", "call_1"),
            user_result("call_1", "done"),
            assistant_text("finished"),
        ];
        let mut sanitized = messages.clone();
        sanitize_wire_messages(&mut sanitized);
        assert_eq!(sanitized, messages);
    }

    #[test]
    fn adjacent_user_text_messages_merge() {
        let mut messages = vec![user_text("first"), user_text("second")];
        sanitize_wire_messages(&mut messages);
        assert_eq!(messages.len(), 1);
        match &messages[0] {
            RigMessage::User { content } => {
                let texts: Vec<&str> = content
                    .iter()
                    .map(|item| match item {
                        UserContent::Text(text) => text.text.as_str(),
                        other => panic!("unexpected item: {other:?}"),
                    })
                    .collect();
                assert_eq!(texts, vec!["first", "second"]);
            }
            other => panic!("expected user message, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_batches_stay_separate_from_text() {
        // Merging these would lose the text: rig's OpenAI serializer drops
        // user text from a message that also carries tool results.
        let mut messages = vec![
            assistant_call("call_1"),
            user_result("call_1", "ok"),
            user_text("run the tests too"),
        ];
        sanitize_wire_messages(&mut messages);
        assert_eq!(messages.len(), 3);
        assert!(matches!(messages[1], RigMessage::User { .. }));
        assert!(matches!(messages[2], RigMessage::User { .. }));
    }

    #[test]
    fn adjacent_assistant_messages_merge() {
        let mut messages = vec![
            user_text("go"),
            assistant_text("part one"),
            assistant_call("call_1"),
            user_result("call_1", "ok"),
        ];
        sanitize_wire_messages(&mut messages);
        assert_eq!(messages.len(), 3);
        assert_eq!(assistant_texts(&messages[1]), vec!["part one"]);
        assert_eq!(call_ids(&messages[1]), vec!["call_1".to_string()]);
    }

    #[test]
    fn empty_assistant_text_gets_placeholder() {
        let mut messages = vec![assistant_text(""), user_text("hello")];
        sanitize_wire_messages(&mut messages);
        assert_eq!(assistant_texts(&messages[0]), vec![" "]);
    }

    #[test]
    fn empty_text_beside_tool_call_is_dropped() {
        let mut messages = vec![
            user_text("go"),
            assistant_text_and_call("", "call_1"),
            user_result("call_1", "ok"),
        ];
        sanitize_wire_messages(&mut messages);
        assert!(assistant_texts(&messages[1]).is_empty());
        assert_eq!(call_ids(&messages[1]), vec!["call_1".to_string()]);
    }

    #[test]
    fn empty_tool_result_content_is_padded() {
        let mut messages = vec![
            user_text("go"),
            assistant_call("call_1"),
            user_result("call_1", ""),
        ];
        sanitize_wire_messages(&mut messages);
        assert_eq!(
            result_content(&messages[2], "call_1").as_deref(),
            Some("(empty output)")
        );
    }

    #[test]
    fn reasoning_items_are_never_touched() {
        let messages = vec![assistant_reasoning("thinking…"), user_text("hello")];
        let mut sanitized = messages.clone();
        sanitize_wire_messages(&mut sanitized);
        assert_eq!(sanitized, messages);
    }

    #[test]
    fn sanitization_is_idempotent() {
        let mut messages = vec![
            user_text("start"),
            user_text("again"),
            assistant_call("call_1"),
            user_result("call_1", "old"),
            user_result("call_2", "orphan"),
            assistant_text(""),
            assistant_call("call_3"),
            user_text("after"),
        ];
        sanitize_wire_messages(&mut messages);
        let once = messages.clone();
        sanitize_wire_messages(&mut messages);
        assert_eq!(messages, once);
    }

    #[test]
    fn userless_history_gets_a_continuation_note() {
        // The post-hard-trim forensic shape: structurally valid, but GLM
        // rejects a history with no user message (1214).
        let mut messages = vec![
            RigMessage::System {
                content: "system prompt".to_string(),
            },
            assistant_call("call_1"),
            user_result("call_1", "ok"),
        ];
        sanitize_wire_messages(&mut messages);
        assert_eq!(messages.len(), 4);
        assert!(
            matches!(&messages[0], RigMessage::System { .. }),
            "note must land behind the system block"
        );
        match &messages[1] {
            RigMessage::User { content } => match content.first_ref() {
                UserContent::Text(text) => {
                    assert!(text.text.contains("[context note]"));
                }
                other => panic!("expected text note, got {other:?}"),
            },
            other => panic!("expected user message, got {other:?}"),
        }
    }

    #[test]
    fn mixed_user_message_does_not_count_as_user_text() {
        // rig's OpenAI serializer drops text from a user message that also
        // carries tool results, so it cannot satisfy the user-message rule.
        let mut messages = vec![
            assistant_call("call_1"),
            RigMessage::User {
                content: OneOrMany::many(vec![
                    UserContent::text("looked at it"),
                    UserContent::ToolResult(RigToolResult {
                        id: "call_1".to_string(),
                        call_id: None,
                        content: OneOrMany::one(ToolResultContent::text("ok")),
                    }),
                ])
                .expect("non-empty"),
            },
        ];
        sanitize_wire_messages(&mut messages);
        assert_eq!(messages.len(), 3, "note + call + mixed message");
        assert!(matches!(messages[0], RigMessage::User { .. }));
    }

    #[test]
    fn history_with_plain_user_message_is_left_alone() {
        let messages = vec![
            RigMessage::System {
                content: "system".to_string(),
            },
            assistant_call("call_1"),
            user_result("call_1", "ok"),
            user_text("please continue"),
        ];
        let mut sanitized = messages.clone();
        sanitize_wire_messages(&mut sanitized);
        assert_eq!(sanitized, messages);
    }

    #[test]
    fn duplicate_calls_keep_only_the_latest() {
        // Two assistant messages reusing one call id with a single result is
        // the duplicate-id shape strict OpenAI-compatible endpoints reject.
        let mut messages = vec![
            user_text("go"),
            assistant_text_and_call("first attempt", "call_1"),
            assistant_text_and_call("retry", "call_1"),
            user_result("call_1", "ok"),
        ];
        sanitize_wire_messages(&mut messages);
        // Adjacent assistants merge (invariant 4); the point here is that
        // exactly ONE call id survives — the superseded call is stripped
        // while its text legally stays.
        let calls: Vec<String> = messages.iter().flat_map(call_ids_of).collect();
        assert_eq!(calls, vec!["call_1".to_string()]);
        assert_eq!(
            assistant_texts(&messages[1]),
            vec!["first attempt", "retry"]
        );
    }

    #[test]
    fn result_preceding_its_call_is_dropped_on_both_sides() {
        // A /undo-style mutation can leave a result before its call;
        // presence-only liveness would ship the invalid ordering and eat
        // the strict-provider 400 this module exists to prevent.
        let mut messages = vec![
            user_text("go"),
            user_result("call_1", "early"),
            assistant_call("call_1"),
            user_text("after"),
        ];
        sanitize_wire_messages(&mut messages);
        assert!(
            messages.iter().all(|m| call_ids_of(m).is_empty()),
            "the late call must be stripped"
        );
        assert!(
            messages.iter().all(|m| result_ids_of(m).is_empty()),
            "the early result must be dropped as an orphan"
        );
    }

    #[test]
    fn placeholder_survives_a_second_pass() {
        // The one-space placeholder is stable under the empty filter; a
        // history that trips invariant 5 must converge (a single pass over
        // the sanitized output reports no further change).
        let mut messages = vec![assistant_text(""), user_text("hello")];
        sanitize_wire_messages(&mut messages);
        assert_eq!(assistant_texts(&messages[0]), vec![" "]);
        let once = messages.clone();
        sanitize_wire_messages(&mut messages);
        assert_eq!(messages, once, "second pass is a no-op");
    }
}
