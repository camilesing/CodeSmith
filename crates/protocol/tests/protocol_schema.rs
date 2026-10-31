//! The published JSON Schema of the app-server protocol (round-9 TODO-61):
//! `docs/schemas/protocol.schema.json` is generated from the `JsonSchema`
//! derives on this crate's types, so the wire contract an external client
//! reads cannot drift from the serde types the server decodes. Regenerate
//! with `CODESMITH_UPDATE_SCHEMA=1 cargo test -p codesmith-protocol --test
//! protocol_schema`.

use codesmith_protocol::{
    AppRequest, AppResponse, ApprovalDecisionRequest, EventFrame, PromptRequest, PromptResponse,
    ThreadRequest, ThreadResponse, runtime::RuntimeEventEnvelope,
};

/// The protocol's top-level messages, one `$defs` entry each (their nested
/// types land in `$defs` too).
fn protocol_schema() -> serde_json::Value {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let roots = [
        ("ThreadRequest", generator.subschema_for::<ThreadRequest>()),
        (
            "ThreadResponse",
            generator.subschema_for::<ThreadResponse>(),
        ),
        ("AppRequest", generator.subschema_for::<AppRequest>()),
        ("AppResponse", generator.subschema_for::<AppResponse>()),
        ("PromptRequest", generator.subschema_for::<PromptRequest>()),
        (
            "PromptResponse",
            generator.subschema_for::<PromptResponse>(),
        ),
        (
            "ApprovalDecisionRequest",
            generator.subschema_for::<ApprovalDecisionRequest>(),
        ),
        ("EventFrame", generator.subschema_for::<EventFrame>()),
        (
            "RuntimeEventEnvelope",
            generator.subschema_for::<RuntimeEventEnvelope>(),
        ),
    ];
    let messages: serde_json::Map<String, serde_json::Value> = roots
        .into_iter()
        .map(|(name, schema)| (name.to_string(), schema.to_value()))
        .collect();
    let defs: serde_json::Map<String, serde_json::Value> =
        generator.take_definitions(true).into_iter().collect();
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "CodeSmith app-server protocol",
        "description": "Generated from codesmith-protocol — do not hand-edit.",
        "messages": messages,
        "$defs": defs,
    })
}

#[test]
fn published_protocol_schema_is_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schemas/protocol.schema.json");
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&protocol_schema()).expect("render schema")
    );
    if std::env::var_os("CODESMITH_UPDATE_SCHEMA").is_some() {
        std::fs::create_dir_all(path.parent().expect("schema dir")).expect("create schema dir");
        std::fs::write(&path, &rendered).expect("write schema");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == rendered,
        "docs/schemas/protocol.schema.json is stale — regenerate with \
         `CODESMITH_UPDATE_SCHEMA=1 cargo test -p codesmith-protocol --test protocol_schema`"
    );
}

/// The schema describes what serde actually emits: a real `EventFrame`
/// carries the `event` tag the schema names for it.
#[test]
fn event_frame_schema_names_the_serde_tag() {
    let schema = protocol_schema();
    let rendered = serde_json::to_string(&schema["$defs"]["EventFrame"]).expect("render");
    let frame = serde_json::to_value(EventFrame::ResponseStart {
        response_id: "r1".to_string(),
    })
    .expect("serialize frame");
    assert_eq!(frame["event"], "response_start");
    assert!(rendered.contains("response_start"), "{rendered}");
}
