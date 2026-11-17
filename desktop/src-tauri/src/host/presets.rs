//! `agentPresets/describe|apply` — thin forwards to the runtime's preset
//! center (`GET/POST /v1/presets[/apply]`, implemented in
//! `crates/cli/src/preset_center.rs`). The bridge holds no preset state of
//! its own; snapshot truth and write validation live in the serving
//! process, the same split as `session/modelCatalog`.
//!
//! Known limitations: no CAS or push between windows — a write returns the
//! fresh snapshot and last writer wins (the runtime's file writes are
//! atomic); `agentPresets/list` stays an empty stub for the vendored
//! caller that expects it.

use anyhow::Result;
use serde_json::{Value, json};

use super::{HostState, rpc_error, rpc_ok};

/// `agentPresets/describe` → `GET /v1/presets`.
pub async fn describe(host: &HostState) -> Value {
    match host.backend.get::<Value>("/v1/presets").await {
        Ok(snapshot) => rpc_ok(snapshot),
        Err(err) => rpc_error(
            "gateway/internal",
            format!("agentPresets/describe: {err:#}"),
        ),
    }
}

/// `agentPresets/apply` → `POST /v1/presets/apply` with the ops array
/// verbatim. The runtime's own 400 message (`[code] …`) surfaces as the
/// RPC error message so the UI can show why a batch was refused.
pub async fn apply(host: &HostState, args: &Value) -> Value {
    let Some(ops) = args.get("ops").and_then(Value::as_array).cloned() else {
        return rpc_error(
            "gateway/bad-args",
            "agentPresets/apply: missing `ops` array",
        );
    };
    match forward_apply(host, &ops).await {
        Ok(Ok(snapshot)) => rpc_ok(snapshot),
        Ok(Err(message)) => rpc_error("presets/apply-refused", message),
        Err(err) => rpc_error(
            "gateway/internal",
            format!("agentPresets/apply: {err:#}"),
        ),
    }
}

async fn forward_apply(host: &HostState, ops: &[Value]) -> Result<Result<Value, String>> {
    host.backend
        .post_with_error_message::<Value>("/v1/presets/apply", &json!(ops))
        .await
}
