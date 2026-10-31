use serde::Deserialize;
use serde::Serialize;

use super::error::Error;
use super::error::Result;

/// Variant order is load-bearing: it declares ascending restrictiveness
/// (`Allow` < `Prompt` < `Forbidden`), so a derived `max()` — or
/// [`Decision::most_restrictive`] — picks the most restrictive outcome.
/// Reordering variants or inserting a new one silently changes that
/// precedence; keep the order and the helper in sync.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    /// Command may run without further approval.
    Allow,
    /// Request explicit user approval; rejected outright when running with `approval_policy="never"`.
    Prompt,
    /// Command is blocked without further consideration.
    Forbidden,
}

impl Decision {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "allow" => Ok(Self::Allow),
            "prompt" => Ok(Self::Prompt),
            "forbidden" => Ok(Self::Forbidden),
            other => Err(Error::InvalidDecision(other.to_string())),
        }
    }

    /// The more restrictive of two decisions. The precedence rule lives in
    /// exactly one place instead of being implied by the enum's derived
    /// `Ord` at every aggregation site.
    #[must_use]
    pub fn most_restrictive(self, other: Self) -> Self {
        if self >= other { self } else { other }
    }
}
