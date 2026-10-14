use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use codesmith_protocol::{ToolKind, ToolOutput, ToolPayload};
use codesmith_tools::{
    ToolCall, ToolCallSource, ToolHandler, ToolInvocation, ToolRegistry, ToolSpec,
};
use serde_json::json;
use tokio::sync::Notify;

struct EchoHandler;

#[async_trait]
impl ToolHandler for EchoHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> std::result::Result<ToolOutput, codesmith_tools::FunctionCallError> {
        Ok(ToolOutput::Function {
            body: Some(json!({
                "tool": invocation.tool_name,
                "call_id": invocation.call_id
            })),
            success: true,
        })
    }
}

struct BlockingHandler {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

struct MutatingHandler;

#[async_trait]
impl ToolHandler for MutatingHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> std::result::Result<ToolOutput, codesmith_tools::FunctionCallError> {
        Ok(ToolOutput::Function {
            body: Some(json!({
                "tool": invocation.tool_name,
                "call_id": invocation.call_id
            })),
            success: true,
        })
    }
}

#[async_trait]
impl ToolHandler for BlockingHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> std::result::Result<ToolOutput, codesmith_tools::FunctionCallError> {
        self.started.notify_waiters();
        self.release.notified().await;
        Ok(ToolOutput::Function {
            body: Some(json!({
                "tool": invocation.tool_name,
                "call_id": invocation.call_id
            })),
            success: true,
        })
    }
}

struct ReentrantHandler {
    registry: Arc<OnceLock<Arc<ToolRegistry>>>,
}

#[async_trait]
impl ToolHandler for ReentrantHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(
        &self,
        _invocation: ToolInvocation,
    ) -> std::result::Result<ToolOutput, codesmith_tools::FunctionCallError> {
        let registry = self.registry.get().expect("registry initialized").clone();
        registry
            .dispatch(
                ToolCall {
                    name: "inner".to_string(),
                    payload: ToolPayload::Function {
                        arguments: "{}".to_string(),
                    },
                    source: ToolCallSource::Direct,
                    raw_tool_call_id: Some("inner-call".to_string()),
                },
                true,
            )
            .await
    }
}

#[tokio::test]
async fn dispatches_function_tool_with_allow_mutating() {
    let mut registry = ToolRegistry::default();
    registry
        .register(
            ToolSpec {
                name: "echo".to_string(),
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
                supports_parallel_tool_calls: true,
                timeout_ms: Some(1000),
            },
            Arc::new(EchoHandler),
        )
        .expect("register tool");

    let output = registry
        .dispatch(
            ToolCall {
                name: "echo".to_string(),
                payload: ToolPayload::Function {
                    arguments: "{\"message\":\"hi\"}".to_string(),
                },
                source: ToolCallSource::Direct,
                raw_tool_call_id: Some("call-1".to_string()),
            },
            true,
        )
        .await
        .expect("dispatch tool");
    match output {
        ToolOutput::Function { success, .. } => assert!(success),
        other => panic!("unexpected output: {other:?}"),
    }
}

#[test]
fn registering_duplicate_tool_name_errors_instead_of_replacing() {
    let mut registry = ToolRegistry::default();
    let spec = ToolSpec {
        name: "echo".to_string(),
        input_schema: json!({"type":"object"}),
        output_schema: json!({"type":"object"}),
        supports_parallel_tool_calls: true,
        timeout_ms: Some(1000),
    };
    registry
        .register(spec.clone(), Arc::new(EchoHandler))
        .expect("first registration");

    let err = registry
        .register(spec, Arc::new(EchoHandler))
        .expect_err("duplicate name must fail");
    assert!(err.to_string().contains("already registered"), "{err}");
    assert_eq!(registry.list_specs().len(), 1);
}

#[tokio::test]
async fn dispatch_returns_timed_out_when_handler_exceeds_timeout_ms() {
    let mut registry = ToolRegistry::default();
    registry
        .register(
            ToolSpec {
                name: "hangs".to_string(),
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
                supports_parallel_tool_calls: true,
                timeout_ms: Some(50),
            },
            Arc::new(BlockingHandler {
                started: Arc::new(Notify::new()),
                release: Arc::new(Notify::new()),
            }),
        )
        .expect("register hangs");

    let err = tokio::time::timeout(
        Duration::from_secs(2),
        registry.dispatch(
            ToolCall {
                name: "hangs".to_string(),
                payload: ToolPayload::Function {
                    arguments: "{}".to_string(),
                },
                source: ToolCallSource::Direct,
                raw_tool_call_id: Some("call-hangs".to_string()),
            },
            true,
        ),
    )
    .await
    .expect("dispatch settled")
    .expect_err("expected timeout");
    assert!(matches!(
        err,
        codesmith_tools::FunctionCallError::TimedOut { .. }
    ));
}

#[tokio::test]
async fn dispatch_rejects_mutating_tool_when_allow_mutating_is_false() {
    let mut registry = ToolRegistry::default();
    registry
        .register(
            ToolSpec {
                name: "write_file".to_string(),
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
                supports_parallel_tool_calls: false,
                timeout_ms: Some(1000),
            },
            Arc::new(MutatingHandler),
        )
        .expect("register write_file");

    let err = registry
        .dispatch(
            ToolCall {
                name: "write_file".to_string(),
                payload: ToolPayload::Function {
                    arguments: "{}".to_string(),
                },
                source: ToolCallSource::Direct,
                raw_tool_call_id: Some("call-write".to_string()),
            },
            false,
        )
        .await
        .expect_err("mutating tool rejected");
    assert!(matches!(
        err,
        codesmith_tools::FunctionCallError::MutatingToolRejected { .. }
    ));

    let output = registry
        .dispatch(
            ToolCall {
                name: "write_file".to_string(),
                payload: ToolPayload::Function {
                    arguments: "{}".to_string(),
                },
                source: ToolCallSource::Direct,
                raw_tool_call_id: Some("call-write".to_string()),
            },
            true,
        )
        .await
        .expect("mutating tool allowed");
    match output {
        ToolOutput::Function { success, .. } => assert!(success),
        other => panic!("unexpected output: {other:?}"),
    }
}

#[tokio::test]
async fn dispatch_returns_tool_not_found_for_unregistered_name() {
    let registry = ToolRegistry::default();
    let err = registry
        .dispatch(
            ToolCall {
                name: "missing".to_string(),
                payload: ToolPayload::Function {
                    arguments: "{}".to_string(),
                },
                source: ToolCallSource::Direct,
                raw_tool_call_id: Some("call-missing".to_string()),
            },
            true,
        )
        .await
        .expect_err("unregistered tool rejected");
    assert!(matches!(
        err,
        codesmith_tools::FunctionCallError::ToolNotFound { .. }
    ));
}

#[tokio::test]
async fn dispatch_returns_kind_mismatch_for_payload_handler_kind_mismatch() {
    let mut registry = ToolRegistry::default();
    registry
        .register(
            ToolSpec {
                name: "echo".to_string(),
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
                supports_parallel_tool_calls: true,
                timeout_ms: Some(1000),
            },
            Arc::new(EchoHandler),
        )
        .expect("register echo");

    let err = registry
        .dispatch(
            ToolCall {
                name: "echo".to_string(),
                payload: ToolPayload::Mcp {
                    server: "srv".to_string(),
                    tool: "echo".to_string(),
                    raw_arguments: json!({}),
                    raw_tool_call_id: None,
                },
                source: ToolCallSource::Direct,
                raw_tool_call_id: Some("call-mcp".to_string()),
            },
            true,
        )
        .await
        .expect_err("kind mismatch rejected");
    assert!(matches!(
        err,
        codesmith_tools::FunctionCallError::KindMismatch { .. }
    ));
}

#[tokio::test]
async fn serial_tool_waits_for_running_parallel_tool() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut registry = ToolRegistry::default();
    registry
        .register(
            ToolSpec {
                name: "slow_read".to_string(),
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
                supports_parallel_tool_calls: true,
                timeout_ms: Some(1000),
            },
            Arc::new(BlockingHandler {
                started: started.clone(),
                release: release.clone(),
            }),
        )
        .expect("register slow read");
    registry
        .register(
            ToolSpec {
                name: "serial".to_string(),
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
                supports_parallel_tool_calls: false,
                timeout_ms: Some(1000),
            },
            Arc::new(EchoHandler),
        )
        .expect("register serial");

    let registry = Arc::new(registry);
    let started_wait = started.notified();
    let parallel_registry = registry.clone();
    let parallel = tokio::spawn(async move {
        parallel_registry
            .dispatch(
                ToolCall {
                    name: "slow_read".to_string(),
                    payload: ToolPayload::Function {
                        arguments: "{}".to_string(),
                    },
                    source: ToolCallSource::Direct,
                    raw_tool_call_id: Some("parallel-call".to_string()),
                },
                true,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), started_wait)
        .await
        .expect("parallel tool started");

    let serial_registry = registry.clone();
    let mut serial = tokio::spawn(async move {
        serial_registry
            .dispatch(
                ToolCall {
                    name: "serial".to_string(),
                    payload: ToolPayload::Function {
                        arguments: "{}".to_string(),
                    },
                    source: ToolCallSource::Direct,
                    raw_tool_call_id: Some("serial-call".to_string()),
                },
                true,
            )
            .await
    });

    tokio::select! {
        res = &mut serial => panic!(
            "serial dispatch resolved while a parallel tool was still running: {:?}",
            res
        ),
        () = tokio::time::sleep(Duration::from_millis(200)) => {}
    }

    release.notify_waiters();
    tokio::time::timeout(Duration::from_secs(2), async {
        serial
            .await
            .expect("serial task panicked")
            .expect("serial ran");
        parallel
            .await
            .expect("parallel task panicked")
            .expect("parallel ran");
    })
    .await
    .expect("serial and parallel dispatches completed after release");
}

#[tokio::test]
async fn serial_tool_can_reenter_registry_without_deadlock() {
    let registry_cell = Arc::new(OnceLock::new());
    let mut registry = ToolRegistry::default();
    registry
        .register(
            ToolSpec {
                name: "outer".to_string(),
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
                supports_parallel_tool_calls: false,
                timeout_ms: Some(1000),
            },
            Arc::new(ReentrantHandler {
                registry: registry_cell.clone(),
            }),
        )
        .expect("register outer");
    registry
        .register(
            ToolSpec {
                name: "inner".to_string(),
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
                supports_parallel_tool_calls: false,
                timeout_ms: Some(1000),
            },
            Arc::new(EchoHandler),
        )
        .expect("register inner");

    let registry = Arc::new(registry);
    assert!(registry_cell.set(registry.clone()).is_ok());

    let output = tokio::time::timeout(
        Duration::from_secs(1),
        registry.dispatch(
            ToolCall {
                name: "outer".to_string(),
                payload: ToolPayload::Function {
                    arguments: "{}".to_string(),
                },
                source: ToolCallSource::Direct,
                raw_tool_call_id: Some("outer-call".to_string()),
            },
            true,
        ),
    )
    .await
    .expect("outer dispatch timed out")
    .expect("outer dispatch failed");

    match output {
        ToolOutput::Function { success, .. } => assert!(success),
        other => panic!("unexpected output: {other:?}"),
    }
}
