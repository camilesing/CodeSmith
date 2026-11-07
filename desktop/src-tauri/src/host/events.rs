//! `$events` waterfall: approvals and user questions.
//!
//! The runtime surfaces `approval.required` / `user_input.required` on the
//! thread event stream; the client answers them through the `$events`
//! waterfall (frames `{type:'waterfall', event, eventId, agentId, request}`
//! pushed to every open `$events` sink; the reply arrives as the unary
//! `$events/result` call `{clientId, eventId, outcome}` — see the vendored
//! api-gateway `stream-protocol.ts` for the wire shapes).
//!
//! Outcome mapping: `'allowed-once'` → POST allow; `'rejected'`/`'cancelled'`
//! → POST deny; an unclaimed waterfall (`next`, no listener answered) is
//! denied too — leaving it pending would hang the turn until the runtime's
//! own approval timeout, and deny is the runtime's timeout decision anyway.
//!
//! Known limitations: user questions have no HTTP cancel route, so an
//! unclaimed or rejected question waterfall is answered `{delivered:false}`
//! and left to the runtime's deadline; the approval `callId` correlation is
//! absent (the runtime event carries no tool call id) and `reason` carries
//! the description.

use serde_json::{json, Value};

/// A pending waterfall awaiting its `$events/result`.
#[derive(Debug, Clone)]
pub struct PendingInput {
    pub session_id: String,
    /// `approval` POSTs `/v1/approvals/{id}`; `questions` POSTs
    /// `/v1/user-input/{session}/{input_id}`.
    pub input_id: String,
    pub kind: InputKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Approval,
    Questions,
}

/// `approval.required` payload → (eventId, waterfall frame).
pub fn approval_waterfall(session_id: &str, payload: &Value) -> Option<(String, Value)> {
    let id = payload
        .get("id")
        .or_else(|| payload.get("approval_id"))
        .and_then(Value::as_str)?;
    let tool_name = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let mut request = json!({ "toolName": tool_name });
    // Optional fields must be ABSENT, not null — a null reaches the client as
    // a present typed value.
    for (field, key) in [
        ("description", "reason"),
        ("intent_summary", "displayReason"),
    ] {
        if let Some(text) = payload.get(field).and_then(Value::as_str) {
            if !text.is_empty() {
                request[key] = json!(text);
            }
        }
    }
    Some((
        id.to_string(),
        json!({
            "type": "waterfall",
            "event": "approval/request",
            "eventId": id,
            "agentId": session_id,
            "request": request,
        }),
    ))
}

/// `user_input.required` payload → (input_id, waterfall frame) with the
/// runtime's question shape folded to the client's `AskUserQuestionItem[]`.
pub fn questions_waterfall(session_id: &str, payload: &Value) -> Option<(String, Value)> {
    let id = payload.get("id").and_then(Value::as_str)?;
    let questions = payload
        .pointer("/request/questions")
        .and_then(Value::as_array)?;
    let items: Vec<Value> = questions
        .iter()
        .map(|question| {
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .map(|option| {
                            json!({
                                "label": option.get("label").and_then(Value::as_str).unwrap_or_default(),
                                "description": option.get("description").and_then(Value::as_str).unwrap_or_default(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            json!({
                "id": question.get("id").and_then(Value::as_str).unwrap_or_default(),
                "question": question.get("question").and_then(Value::as_str).unwrap_or_default(),
                "header": question.get("header").and_then(Value::as_str).unwrap_or_default(),
                "options": options,
                "multiSelect": false,
            })
        })
        .collect();
    Some((
        id.to_string(),
        json!({
            "type": "waterfall",
            "event": "user-questions/request",
            "eventId": id,
            "agentId": session_id,
            "request": { "questions": items },
        }),
    ))
}

/// The cancellation frame for a waterfall that ended without a client reply.
pub fn cancel_frame(event_id: &str) -> Value {
    json!({ "type": "cancel", "eventId": event_id })
}

/// Approval outcome → the runtime decision body (`allow`/`deny`).
pub fn approval_decision(outcome: &Value) -> &'static str {
    if outcome.get("kind").and_then(Value::as_str) == Some("result")
        && outcome.get("value").and_then(Value::as_str) == Some("allowed-once")
    {
        "allow"
    } else {
        // 'rejected', 'cancelled', an unclaimed 'next', or any malformed
        // outcome: deny is the safe terminal answer.
        "deny"
    }
}

/// Question outcome → the runtime answers body for
/// POST /v1/user-input/{thread}/{input}. `None` when nobody answered.
pub fn question_answers(outcome: &Value) -> Option<Vec<Value>> {
    if outcome.get("kind").and_then(Value::as_str) != Some("result") {
        return None;
    }
    let answers = outcome
        .pointer("/value/answers")
        .and_then(Value::as_array)?;
    Some(
        answers
            .iter()
            .map(|answer| {
                let selected = answer
                    .get("selected")
                    .and_then(Value::as_array)
                    .map(|labels| labels.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                    .unwrap_or_default();
                let label = selected.first().copied().unwrap_or_default();
                let custom = answer.get("custom").and_then(Value::as_str);
                json!({
                    "id": answer.get("id").and_then(Value::as_str).unwrap_or_default(),
                    "label": label,
                    "value": custom.unwrap_or(label),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_waterfall_maps_payload_and_skips_empty_optionals() {
        let (id, frame) = approval_waterfall(
            "sess_1",
            &json!({ "id": "appr_1", "tool_name": "bash", "description": "run ls", "intent_summary": "" }),
        )
        .expect("waterfall");
        assert_eq!(id, "appr_1");
        assert_eq!(frame["type"], "waterfall");
        assert_eq!(frame["event"], "approval/request");
        assert_eq!(frame["agentId"], "sess_1");
        assert_eq!(frame["request"]["toolName"], "bash");
        assert_eq!(frame["request"]["reason"], "run ls");
        assert!(
            frame["request"].get("displayReason").is_none(),
            "empty optional must be absent, not null"
        );
    }

    #[test]
    fn approval_decision_maps_every_outcome_kind() {
        assert_eq!(
            approval_decision(&json!({"kind":"result","value":"allowed-once"})),
            "allow"
        );
        assert_eq!(
            approval_decision(&json!({"kind":"result","value":"rejected"})),
            "deny"
        );
        assert_eq!(approval_decision(&json!({"kind":"next"})), "deny");
        assert_eq!(
            approval_decision(&json!({"kind":"rejected","error":{"name":"Error","message":"x"}})),
            "deny"
        );
    }

    #[test]
    fn question_answers_map_selected_labels_and_custom_text() {
        let answers = question_answers(&json!({
            "kind": "result",
            "value": { "answers": [
                { "id": "q1", "selected": ["Option A"] },
                { "id": "q2", "selected": ["Other"], "custom": "free text" },
            ]}
        }))
        .expect("answers");
        assert_eq!(answers[0]["label"], "Option A");
        assert_eq!(answers[0]["value"], "Option A");
        assert_eq!(answers[1]["label"], "Other");
        assert_eq!(answers[1]["value"], "free text");
        assert!(question_answers(&json!({"kind":"next"})).is_none());
    }

    #[test]
    fn questions_waterfall_folds_runtime_questions() {
        let (id, frame) = questions_waterfall(
            "sess_1",
            &json!({ "id": "in_1", "request": { "questions": [{
                "id": "q1", "header": "Scope", "question": "Which?",
                "options": [
                    { "label": "A", "description": "first" },
                    { "label": "B", "description": "second" },
                ],
            }]}}),
        )
        .expect("waterfall");
        assert_eq!(id, "in_1");
        assert_eq!(frame["event"], "user-questions/request");
        let question = &frame["request"]["questions"][0];
        assert_eq!(question["id"], "q1");
        assert_eq!(question["header"], "Scope");
        assert_eq!(question["options"][1]["label"], "B");
        assert_eq!(question["multiSelect"], false);
    }
}
