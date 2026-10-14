use codesmith_protocol::{
    EventFrame, ThreadListParams, ThreadRequest, ThreadResumeParams,
    runtime::{RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION, RuntimeEventEnvelope},
};
use serde_json::{Value, json};

#[test]
fn thread_resume_params_round_trip() {
    let request = ThreadRequest::Resume(ThreadResumeParams {
        thread_id: "thread-123".to_string(),
        history: None,
        path: None,
        model: Some("deepseek-v4-pro".to_string()),
        model_provider: Some("deepseek".to_string()),
        cwd: None,
        approval_policy: Some("on-request".to_string()),
        sandbox: Some("workspace-write".to_string()),
        config: None,
        base_instructions: Some("base".to_string()),
        developer_instructions: Some("dev".to_string()),
        personality: Some("default".to_string()),
        persist_extended_history: true,
    });

    let encoded = serde_json::to_string(&request).expect("serialize request");
    let decoded: ThreadRequest = serde_json::from_str(&encoded).expect("deserialize request");
    match decoded {
        ThreadRequest::Resume(params) => {
            assert_eq!(params.thread_id, "thread-123");
            assert_eq!(params.model.as_deref(), Some("deepseek-v4-pro"));
            assert_eq!(params.model_provider.as_deref(), Some("deepseek"));
            assert_eq!(params.approval_policy.as_deref(), Some("on-request"));
            assert_eq!(params.sandbox.as_deref(), Some("workspace-write"));
            assert_eq!(params.base_instructions.as_deref(), Some("base"));
            assert_eq!(params.developer_instructions.as_deref(), Some("dev"));
            assert_eq!(params.personality.as_deref(), Some("default"));
            assert!(params.history.is_none());
            assert!(params.path.is_none());
            assert!(params.cwd.is_none());
            assert!(params.config.is_none());
            assert!(params.persist_extended_history);
        }
        other => panic!("unexpected request: {other:?}"),
    }
}

#[test]
fn thread_list_params_defaults_are_serializable() {
    let request = ThreadRequest::List(ThreadListParams {
        include_archived: false,
        limit: Some(20),
    });
    let encoded = serde_json::to_string_pretty(&request).expect("serialize list request");
    assert!(encoded.contains("\"kind\": \"list\""));
    let decoded: ThreadRequest = serde_json::from_str(&encoded).expect("deserialize list request");
    match decoded {
        ThreadRequest::List(params) => {
            assert!(!params.include_archived);
            assert_eq!(params.limit, Some(20));
        }
        other => panic!("unexpected request: {other:?}"),
    }

    // Defaults: deserializing a bare list body should yield the documented
    // defaults (`include_archived` has `#[serde(default)]`, `limit` is
    // `Option`).
    let defaulted = json!({ "kind": "list" });
    let decoded: ThreadRequest =
        serde_json::from_value(defaulted).expect("deserialize list request defaults");
    match decoded {
        ThreadRequest::List(params) => {
            assert!(!params.include_archived);
            assert_eq!(params.limit, None);
        }
        other => panic!("unexpected request: {other:?}"),
    }
}

#[test]
fn event_frame_serialization_contains_expected_tag() {
    let frame = EventFrame::TurnComplete {
        turn_id: "turn-1".to_string(),
    };
    let encoded = serde_json::to_string(&frame).expect("serialize frame");
    let value: Value = serde_json::from_str(&encoded).expect("parse serialized frame");
    assert_eq!(value["event"], "turn_complete");
    assert_eq!(value["turn_id"], "turn-1");
    let decoded: EventFrame = serde_json::from_str(&encoded).expect("deserialize frame");
    match decoded {
        EventFrame::TurnComplete { turn_id } => assert_eq!(turn_id, "turn-1"),
        other => panic!("unexpected frame: {other:?}"),
    }
}

#[test]
fn runtime_event_envelope_roundtrip() {
    let input = json!({
        "schema_version": 1,
        "seq": 12,
        "event": "item.delta",
        "kind": "item.delta",
        "thread_id": "thr_123",
        "turn_id": "turn_456",
        "item_id": "item_789",
        "timestamp": "2026-02-11T20:18:49.123Z",
        "created_at": "2026-02-11T20:18:49.123Z",
        "payload": { "delta": "ok", "kind": "agent_message" },
    });
    let envelope: RuntimeEventEnvelope =
        serde_json::from_value(input).expect("deserialize runtime event envelope");
    assert_eq!(envelope.schema_version, Some(1));
    assert_eq!(envelope.seq, 12);
    assert_eq!(envelope.event, "item.delta");
    assert_eq!(envelope.kind, "item.delta");
    assert_eq!(envelope.thread_id, "thr_123");

    let encoded = serde_json::to_value(&envelope).expect("serialize runtime event envelope");
    assert_eq!(encoded["event"], encoded["kind"]);
    assert_eq!(encoded["schema_version"], 1);
    assert_eq!(encoded["seq"], 12);
    assert_eq!(encoded["thread_id"], "thr_123");
    assert_eq!(encoded["turn_id"], "turn_456");
    assert_eq!(encoded["item_id"], "item_789");
    assert_eq!(encoded["timestamp"], "2026-02-11T20:18:49.123Z");
    assert_eq!(encoded["created_at"], "2026-02-11T20:18:49.123Z");
    assert_eq!(
        encoded["payload"],
        json!({ "delta": "ok", "kind": "agent_message" })
    );
}

#[test]
fn runtime_event_envelope_preserves_conflicting_event_and_kind() {
    let input = json!({
        "schema_version": 1,
        "seq": 16,
        "event": "item.delta",
        "kind": "turn.completed",
        "thread_id": "thr_conflict",
        "timestamp": "2026-02-11T20:18:49.123Z",
        "payload": {},
    });
    let envelope: RuntimeEventEnvelope =
        serde_json::from_value(input).expect("deserialize runtime event envelope");
    assert_eq!(envelope.event, "item.delta");
    assert_eq!(envelope.kind, "turn.completed");

    let encoded = serde_json::to_value(&envelope).expect("serialize runtime event envelope");
    assert_eq!(encoded["event"], "item.delta");
    assert_eq!(encoded["kind"], "turn.completed");
}

#[test]
fn runtime_event_envelope_without_schema_version_is_legacy_none() {
    let input = json!({
        "seq": 15,
        "event": "thread.started",
        "kind": "thread.started",
        "thread_id": "thr_default_version",
        "timestamp": "2026-02-11T20:18:49.123Z",
        "payload": {},
    });
    let envelope: RuntimeEventEnvelope = serde_json::from_value(input)
        .expect("deserialize runtime event envelope without schema version");

    // Absence is observable: defaulting it to the current
    // RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION would silently re-interpret
    // legacy payloads once the version bumps past 1.
    assert_eq!(envelope.schema_version, None);
    let _ = RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION;
}

#[test]
fn runtime_event_envelope_accepts_and_surfaces_newer_schema_version() {
    let input = json!({
        "schema_version": RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION + 1,
        "seq": 17,
        "event": "item.delta",
        "kind": "item.delta",
        "thread_id": "thr_future",
        "timestamp": "2026-02-11T20:18:49.123Z",
        "payload": {},
    });
    let envelope: RuntimeEventEnvelope =
        serde_json::from_value(input).expect("deserialize newer-schema envelope");
    assert_eq!(
        envelope.schema_version,
        Some(RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION + 1)
    );

    let encoded = serde_json::to_value(&envelope).expect("serialize newer-schema envelope");
    assert_eq!(
        encoded["schema_version"],
        RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION + 1
    );
}

#[test]
fn runtime_event_envelope_thread_level_keeps_turn_and_item_ids() {
    let input = json!({
        "schema_version": 1,
        "seq": 14,
        "event": "thread.started",
        "kind": "thread.started",
        "thread_id": "thr_thread",
        "timestamp": "2026-02-11T20:18:49.123Z",
        "payload": { "thread": { "id": "thr_thread" } },
    });
    let envelope: RuntimeEventEnvelope = serde_json::from_value(input)
        .expect("deserialize runtime event envelope without thread-level turn/item ids");
    assert!(envelope.turn_id.is_none());
    assert!(envelope.item_id.is_none());

    let encoded = serde_json::to_value(envelope).expect("serialize runtime event envelope");
    // Wire contract: turn_id/item_id are always present (explicit null when
    // thread-level), while created_at is omitted entirely when absent.
    assert!(encoded.get("turn_id").is_some());
    assert!(encoded.get("item_id").is_some());
    assert!(encoded["turn_id"].is_null());
    assert!(encoded["item_id"].is_null());
    assert!(encoded.get("created_at").is_none());
}

#[test]
fn runtime_event_envelope_preserves_unknown_fields() {
    let input: Value = json!({
        "schema_version": 1,
        "seq": 13,
        "event": "turn.completed",
        "kind": "turn.completed",
        "thread_id": "thr_unknown",
        "timestamp": "2026-02-11T20:18:49.123Z",
        "payload": {},
        "forward_compatibility_hint": "v2-ready",
    });
    let envelope: RuntimeEventEnvelope = serde_json::from_value(input)
        .expect("deserialize runtime event envelope with unknown field");
    assert!(envelope.extra.contains_key("forward_compatibility_hint"));

    let encoded = serde_json::to_value(envelope).expect("serialize runtime event envelope");
    assert_eq!(encoded["forward_compatibility_hint"], "v2-ready");
    assert_eq!(encoded["schema_version"], 1);
    assert_eq!(encoded["seq"], 13);
    assert_eq!(encoded["event"], "turn.completed");
    assert_eq!(encoded["kind"], "turn.completed");
    assert_eq!(encoded["thread_id"], "thr_unknown");
    assert!(encoded["turn_id"].is_null());
    assert!(encoded["item_id"].is_null());
}
