//! Tool and types for requesting user input via the TUI.
//!
//! The shared data types ([`UserInputOption`], [`UserInputQuestion`],
//! [`UserInputRequest`], [`UserInputAnswer`], [`UserInputResponse`]) now live
//! in `codesmith_agent_runtime::user_input` and are re-exported here so that
//! existing `crate::tools::user_input` references keep resolving. The
//! [`RequestUserInputTool`] [`ToolSpec`](codesmith_agent_runtime::tools::spec::ToolSpec) implementation
//! stays in the TUI because it is bound to the TUI-local tool trait.

use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use async_trait::async_trait;
use serde_json::{Value, json};

pub use codesmith_agent_runtime::user_input::*;

pub struct RequestUserInputTool;

#[async_trait]
impl ToolSpec for RequestUserInputTool {
    fn name(&self) -> &'static str {
        "request_user_input"
    }

    fn description(&self) -> &'static str {
        "Ask the user 1-3 short questions and return their selections."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "header": { "type": "string" },
                            "id": { "type": "string" },
                            "question": { "type": "string" },
                            "options": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string" },
                                        "description": { "type": "string" }
                                    },
                                    "required": ["label", "description"]
                                },
                                "minItems": 2,
                                "maxItems": 3
                            }
                        },
                        "required": ["header", "id", "question", "options"]
                    },
                    "minItems": 1,
                    "maxItems": 3
                }
            },
            "required": ["questions"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn validate_input(&self, input: &Value, _context: &ToolContext) -> Result<(), ToolError> {
        UserInputRequest::from_value(input).map(|_| ())
    }

    fn is_interactive(&self, _input: &Value) -> bool {
        true
    }

    async fn execute(
        &self,
        _input: Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        Err(ToolError::execution_failed(
            "request_user_input must be handled by the engine",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_spec_validate_input_uses_request_shape() {
        let tool = RequestUserInputTool;
        let context = ToolContext::new(std::env::temp_dir());
        assert!(
            tool.validate_input(&json!({ "questions": [] }), &context)
                .is_err()
        );
        assert!(tool.is_interactive(&json!({})));
    }
}
