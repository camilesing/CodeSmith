use starlark::Error as StarlarkError;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid decision: {0}")]
    InvalidDecision(String),
    #[error("invalid pattern element: {0}")]
    InvalidPattern(String),
    #[error("invalid example: {0}")]
    InvalidExample(String),
    #[error("invalid rule: {0}")]
    InvalidRule(String),
    #[error(
        "expected every example to match at least one rule. rules: {rules:?}; unmatched examples: \
         {examples:?}"
    )]
    ExampleDidNotMatch {
        rules: Vec<String>,
        examples: Vec<String>,
    },
    #[error("expected example to not match rule `{rule}`: {example}")]
    ExampleDidMatch { rule: String, example: String },
    // `starlark::Error` does not implement `std::error::Error`, so
    // thiserror `#[from]`/`#[source]` cannot apply here — the diagnostic is
    // deliberately flattened into the Display string instead.
    #[error("starlark error: {0}")]
    Starlark(StarlarkError),
}
