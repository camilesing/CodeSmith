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
        let mut seen_ids = std::collections::HashSet::new();
        for q in &self.questions {
            if !seen_ids.insert(q.id.as_str()) {
                return Err(ToolError::invalid_input(
                    "request_user_input.questions.id values must be unique",
                ));
            }
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
            let mut seen_labels = std::collections::HashSet::new();
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
                if !seen_labels.insert(opt.label.as_str()) {
                    return Err(ToolError::invalid_input(
                        "request_user_input option labels must be unique within a question",
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

impl UserInputResponse {
    /// Cross-validate the answers against the request that produced them:
    /// every requested question id must be answered exactly once, every
    /// answer must reference a requested id, and every selected label must
    /// be one of the corresponding question's offered options. Call this
    /// where a host-submitted response enters the runtime so unchecked
    /// answers never reach the model.
    pub fn validate_against(&self, request: &UserInputRequest) -> Result<(), ToolError> {
        let mut answered = std::collections::HashSet::new();
        for answer in &self.answers {
            let Some(question) = request.questions.iter().find(|q| q.id == answer.id) else {
                return Err(ToolError::invalid_input(format!(
                    "request_user_input answer references unknown question id `{}`",
                    answer.id
                )));
            };
            if !answered.insert(answer.id.as_str()) {
                return Err(ToolError::invalid_input(format!(
                    "request_user_input question `{}` answered more than once",
                    answer.id
                )));
            }
            if !question.options.iter().any(|opt| opt.label == answer.label) {
                return Err(ToolError::invalid_input(format!(
                    "request_user_input answer label `{}` was not offered for question `{}`",
                    answer.label, answer.id
                )));
            }
        }
        for question in &request.questions {
            if !answered.contains(question.id.as_str()) {
                return Err(ToolError::invalid_input(format!(
                    "request_user_input question `{}` was not answered",
                    question.id
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_option_question(id: &str) -> UserInputQuestion {
        UserInputQuestion {
            header: "Pick".to_string(),
            id: id.to_string(),
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
        }
    }

    fn answer(id: &str, label: &str) -> UserInputAnswer {
        UserInputAnswer {
            id: id.to_string(),
            label: label.to_string(),
            value: label.to_string(),
        }
    }

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

    #[test]
    fn rejects_duplicate_question_ids() {
        let question = UserInputQuestion {
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
        };
        let request = UserInputRequest {
            questions: vec![question.clone(), question],
        };
        assert!(request.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_option_labels() {
        let request = UserInputRequest {
            questions: vec![UserInputQuestion {
                header: "Pick".to_string(),
                id: "choice".to_string(),
                question: "Which option?".to_string(),
                options: vec![
                    UserInputOption {
                        label: "A".to_string(),
                        description: "First".to_string(),
                    },
                    UserInputOption {
                        label: "A".to_string(),
                        description: "Second".to_string(),
                    },
                ],
            }],
        };
        assert!(request.validate().is_err());
    }

    #[test]
    fn validate_against_accepts_complete_offered_answers() {
        let request = UserInputRequest {
            questions: vec![two_option_question("choice")],
        };
        let response = UserInputResponse {
            answers: vec![answer("choice", "A")],
        };
        assert!(response.validate_against(&request).is_ok());
    }

    #[test]
    fn validate_against_rejects_unknown_or_missing_or_duplicate_ids() {
        let request = UserInputRequest {
            questions: vec![two_option_question("choice")],
        };
        let unknown = UserInputResponse {
            answers: vec![answer("other", "A")],
        };
        assert!(unknown.validate_against(&request).is_err());

        let unanswered = UserInputResponse { answers: vec![] };
        assert!(unanswered.validate_against(&request).is_err());

        let duplicated = UserInputResponse {
            answers: vec![answer("choice", "A"), answer("choice", "B")],
        };
        assert!(duplicated.validate_against(&request).is_err());
    }

    #[test]
    fn validate_against_rejects_labels_never_offered() {
        let request = UserInputRequest {
            questions: vec![two_option_question("choice")],
        };
        let response = UserInputResponse {
            answers: vec![answer("choice", "C")],
        };
        let err = response.validate_against(&request).unwrap_err();
        assert!(err.to_string().contains("was not offered"));
    }
}
