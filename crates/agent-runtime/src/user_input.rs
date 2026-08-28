//! Data types for requesting structured user input during a turn.
//!
//! These types are shared between the engine and the host UI so that the
//! `request_user_input` tool can validate payloads the same way regardless of
//! which crate drives the turn loop. [`UserInputRequest`] carries the
//! validation rules (non-empty questions, 1 to 3 questions, 2 or 3 options
//! each) and is used both by the tool's `validate_input` hook and by the
//! engine when it surfaces the request to the host UI.

use codesmith_tools::ToolError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInputOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInputQuestion {
    pub header: String,
    pub id: String,
    pub question: String,
    pub options: Vec<UserInputOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInputRequest {
    pub questions: Vec<UserInputQuestion>,
}

impl UserInputRequest {
    pub fn from_value(value: &Value) -> Result<Self, ToolError> {
        let request: UserInputRequest = serde_json::from_value(value.clone()).map_err(|e| {
            ToolError::invalid_input(format!("Invalid request_user_input payload: {e}"))
        })?;
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), ToolError> {
        if self.questions.is_empty() {
            return Err(ToolError::invalid_input(
                "request_user_input.questions must be non-empty",
            ));
        }
        if self.questions.len() > 3 {
            return Err(ToolError::invalid_input(
                "request_user_input.questions must contain 1 to 3 items",
            ));
        }
        for q in &self.questions {
            if q.header.trim().is_empty() {
                return Err(ToolError::invalid_input(
                    "request_user_input.questions.header cannot be empty",
                ));
            }
            if q.id.trim().is_empty() {
                return Err(ToolError::invalid_input(
                    "request_user_input.questions.id cannot be empty",
                ));
            }
            if q.question.trim().is_empty() {
                return Err(ToolError::invalid_input(
                    "request_user_input.questions.question cannot be empty",
                ));
            }
            if q.options.len() < 2 || q.options.len() > 3 {
                return Err(ToolError::invalid_input(
                    "request_user_input.questions.options must contain 2 or 3 items",
                ));
            }
            for opt in &q.options {
                if opt.label.trim().is_empty() {
                    return Err(ToolError::invalid_input(
                        "request_user_input option label cannot be empty",
                    ));
                }
                if opt.description.trim().is_empty() {
                    return Err(ToolError::invalid_input(
                        "request_user_input option description cannot be empty",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInputAnswer {
    pub id: String,
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInputResponse {
    pub answers: Vec<UserInputAnswer>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_request_shape() {
        let request = UserInputRequest {
            questions: vec![UserInputQuestion {
                header: "Pick".to_string(),
                id: "choice".to_string(),
                question: "Which option?".to_string(),
                options: vec![
                    UserInputOption {
                        label: "A".to_string(),
                        description: "Option A".to_string(),
                    },
                    UserInputOption {
                        label: "B".to_string(),
                        description: "Option B".to_string(),
                    },
                ],
            }],
        };
        assert!(request.validate().is_ok());
    }

    #[test]
    fn rejects_too_many_questions() {
        let request = UserInputRequest {
            questions: (0..4)
                .map(|idx| UserInputQuestion {
                    header: format!("Q{idx}"),
                    id: format!("choice_{idx}"),
                    question: "Pick one".to_string(),
                    options: vec![
                        UserInputOption {
                            label: "A".to_string(),
                            description: "Option A".to_string(),
                        },
                        UserInputOption {
                            label: "B".to_string(),
                            description: "Option B".to_string(),
                        },
                    ],
                })
                .collect(),
        };
        assert!(request.validate().is_err());
    }
}
