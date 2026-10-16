//! In-tree sample extension (`scratchpad`) — the reference sample for the
//! extension system (mirrors `crates/providers/src/mock.rs` for providers).
//!
//! Contributes all three slice-1 contribution points: a [`ToolDefinition`]
//! (`scratch`), a [`CommandDefinition`] (`/scratch`), and a [`Handler`]
//! (`TurnStartLogger`). Compiled in via [`inventory::submit!`] so
//! [`discover_static`](crate::discover_static) finds it without any runtime
//! registration call — slice 1's only exercise of the full discover → load →
//! configure → bind_core → emit path. `/extension list` shows it; §F2 wires
//! live reload so `/extension disable scratchpad` + reload actually drops it.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codesmith_agent::extension::*;
use codesmith_tools::{ToolCapability, ToolResult};
use serde_json::{Value, json};

use crate::ExtensionMetadata;
use crate::discovery::ExtensionRegistration;

/// Instance-scoped scratchpad string: created once per `ScratchpadExtension`
/// in `configure` and shared by that instance's tool + command, so concurrent
/// sessions never read or overwrite each other's scratch data (a real
/// per-session store scoped via [`ExtensionContext`] is §F2). Guarded by a
/// `std::sync::Mutex` because extension handlers/tools may be called from any
/// runtime worker.
type Scratch = Arc<Mutex<Option<String>>>;

/// The sample extension. [`Extension::configure`] registers all three
/// contribution kinds against the (stub) [`ExtensionApi`]; the runner flushes
/// them into the live registries at `bind_core`.
pub struct ScratchpadExtension;

#[async_trait]
impl Extension for ScratchpadExtension {
    fn metadata(&self) -> &ExtensionMetadata {
        static M: ExtensionMetadata = ExtensionMetadata::new("scratchpad");
        &M
    }
    async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
        let scratch: Scratch = Arc::new(Mutex::new(None));
        api.register_tool(Box::new(ScratchTool {
            scratch: Arc::clone(&scratch),
        }))?;
        api.register_command(Box::new(ScratchCommand { scratch }))?;
        api.on(Arc::new(TurnStartLogger))?;
        Ok(())
    }
}

/// `scratch` tool — writes or reads the extension instance's scratch string.
pub struct ScratchTool {
    scratch: Scratch,
}

#[async_trait]
impl ToolDefinition for ScratchTool {
    fn name(&self) -> &str {
        "scratch"
    }
    fn description(&self) -> &str {
        "Write or read a scratch string. Pass {\"op\":\"set\",\"text\":\"...\"} to set, or {\"op\":\"get\"} to read."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["get", "set"] },
                "text": { "type": "string" }
            },
            "required": ["op"]
        })
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        // `set` mutates the shared scratch state: declaring ReadOnly would
        // let capability-based gating (approval skipping, speculative
        // early dispatch) treat mutations as side-effect free. No existing
        // capability describes in-memory mutation — declare none.
        vec![]
    }
    async fn execute(
        &self,
        input: Value,
        _ctx: &dyn ExtensionContext,
    ) -> Result<ToolResult, ExtensionError> {
        // The schema declares `op` required with a closed enum: a missing,
        // non-string, or unknown op is a caller mistake, not a read.
        let op = input
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| ExtensionError::Tool {
                tool: "scratch".to_string(),
                message: format!(
                    "missing or non-string field 'op' (expected \"get\" or \"set\"); \
                     input provided: {}",
                    input
                ),
            })?;
        match op {
            "set" => {
                // `text` is not optional for set: silently writing "" would
                // irreversibly destroy the previous scratchpad contents.
                let text = input.get("text").and_then(Value::as_str).ok_or_else(|| {
                    ExtensionError::Tool {
                        tool: "scratch".to_string(),
                        message: "field 'text' must be a string when op is \"set\"".to_string(),
                    }
                })?;
                let msg = format!("scratch set to {text:?}");
                // Recover from poisoning: the payload is a plain String with
                // no invariant a panic could corrupt, and a poisoned mutex
                // would turn every later scratch call into a panic.
                *self.scratch.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.to_string());
                Ok(ToolResult::success(msg))
            }
            "get" => {
                let val = self
                    .scratch
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .unwrap_or_default();
                Ok(ToolResult::success(val))
            }
            other => Err(ExtensionError::Tool {
                tool: "scratch".to_string(),
                message: format!("unknown op {other:?} (expected \"get\" or \"set\")"),
            }),
        }
    }
}

/// `/scratch` command — prints the current scratchpad contents.
pub struct ScratchCommand {
    scratch: Scratch,
}

#[async_trait]
impl CommandDefinition for ScratchCommand {
    fn name(&self) -> &str {
        "scratch"
    }
    fn description(&self) -> &str {
        "Print the current scratchpad contents."
    }
    async fn run(
        &self,
        _ctx: &dyn ExtensionCommandContext,
        _args: &str,
    ) -> Result<CommandOutput, ExtensionError> {
        let val = self
            .scratch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| "(empty)".into());
        Ok(CommandOutput::Message(format!("scratchpad: {val}")))
    }
}

/// `TurnStart` observer — proves the handler dispatch path is wired (the
/// runner's `emit` fans out to this). Slice 1 just logs; §F2 handlers may
/// return `HandlerOutcome` to cancel/transform the turn.
pub struct TurnStartLogger;

#[async_trait]
impl Handler for TurnStartLogger {
    async fn handle(
        &self,
        event: &ExtensionEvent,
        _ctx: &dyn ExtensionContext,
    ) -> Result<HandlerOutcome, ExtensionError> {
        if let ExtensionEvent::TurnStart { turn_id } = event {
            tracing::debug!("[scratchpad] TurnStart turn_id={turn_id}");
        }
        Ok(HandlerOutcome::Continue)
    }
}

inventory::submit! {
    ExtensionRegistration {
        factory: || Box::new(ScratchpadExtension),
        metadata: ExtensionMetadata::new("scratchpad"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::discover_static;

    #[test]
    fn scratchpad_is_discoverable() {
        let all = discover_static();
        assert!(all.iter().any(|r| r.metadata.id == "scratchpad"));
    }

    #[tokio::test]
    async fn scratch_tool_round_trips() {
        // Fresh instance-scoped state — no reset (and no process-global
        // shared with other tests) needed.
        let tool = ScratchTool {
            scratch: Arc::new(Mutex::new(None)),
        };
        struct Ctx;
        #[async_trait]
        impl ExtensionContext for Ctx {
            fn cwd(&self) -> &std::path::Path {
                std::path::Path::new(".")
            }
            fn mode(&self) -> ExtensionMode {
                ExtensionMode::Interactive
            }
            fn is_idle(&self) -> bool {
                true
            }
            fn signal(&self) -> tokio_util::sync::CancellationToken {
                tokio_util::sync::CancellationToken::new()
            }
            fn generation(&self) -> u64 {
                0
            }
        }
        let set = tool
            .execute(json!({"op":"set","text":"hello"}), &Ctx)
            .await
            .unwrap();
        assert!(set.success);
        let get = tool.execute(json!({"op":"get"}), &Ctx).await.unwrap();
        assert_eq!(get.content, "hello");
    }
}
