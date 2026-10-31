//! Execution and approval modes for the agent runtime.
//!
//! These were relocated from the TUI (`tui::app::AppMode`,
//! `tui::app::ReasoningEffort`, and `tui::approval::ApprovalMode`) so that
//! the runtime can mode-switch toolsets and sandbox policy without depending
//! on the terminal binary. The CLI re-exports them at their historical paths
//! for backwards compatibility.

/// Supported application modes for the runtime.
///
/// Drives per-turn toolset selection (`ToolRegistryFactory::build`) and the
/// default sandbox policy (`Agent`/`Plan` → workspace-write, `Yolo` → full
/// access).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Agent,
    Yolo,
    Plan,
    /// Coordinator mode — the model acts as orchestrator only and delegates
    /// work to worker sub-agents. It cannot directly read/write files or run
    /// commands.
    Coordinator,
}

impl AppMode {
    /// Parse a config/UI string. `None` for unrecognized spellings — a typo
    /// must be reported by the caller, not silently mapped to `Agent` (full
    /// tool access).
    #[must_use]
    pub fn from_setting(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "agent" => Some(Self::Agent),
            "plan" => Some(Self::Plan),
            "yolo" => Some(Self::Yolo),
            "coordinator" | "coordinator_mode" => Some(Self::Coordinator),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Yolo => "yolo",
            Self::Plan => "plan",
            Self::Coordinator => "coordinator",
        }
    }

    /// Short label used in the UI footer.
    pub fn label(self) -> &'static str {
        match self {
            AppMode::Agent => "AGENT",
            AppMode::Yolo => "YOLO",
            AppMode::Plan => "PLAN",
            AppMode::Coordinator => "COORDINATOR",
        }
    }

    /// Description shown in help or onboarding text.
    pub fn description(self) -> &'static str {
        match self {
            AppMode::Agent => "Agent mode - autonomous task execution with tools",
            AppMode::Yolo => "YOLO mode - full tool access without approvals",
            AppMode::Plan => "Plan mode - design before implementing",
            AppMode::Coordinator => {
                "Coordinator mode - orchestrator only, delegates work to workers"
            }
        }
    }

    /// The line REPL's Shift+Tab ring: `Agent → Plan → Yolo → Agent`, with
    /// `Coordinator` as the fourth stop only while its experimental feature
    /// is enabled — the gate the startup path applies, so an unopted user
    /// cannot land in the orchestrator-only toolset by pressing a key once
    /// too often. `/mode coordinator` remains the ungated door.
    #[must_use]
    pub fn cycle_next(self, include_coordinator: bool) -> Self {
        match self {
            Self::Agent => Self::Plan,
            Self::Plan => Self::Yolo,
            Self::Yolo if include_coordinator => Self::Coordinator,
            Self::Yolo | Self::Coordinator => Self::Agent,
        }
    }
}

/// Determines when tool executions require user approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// Auto-approve all tools (YOLO mode / `--yolo` flag).
    Auto,
    /// Suggest approval for non-safe tools (non-YOLO modes).
    #[default]
    Suggest,
    /// Never execute tools requiring approval.
    Never,
}

impl ApprovalMode {
    pub fn label(self) -> &'static str {
        match self {
            ApprovalMode::Auto => "AUTO",
            ApprovalMode::Suggest => "SUGGEST",
            ApprovalMode::Never => "NEVER",
        }
    }

    pub fn from_config_value(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(ApprovalMode::Auto),
            "suggest" | "suggested" | "on-request" | "untrusted" => Some(ApprovalMode::Suggest),
            "never" | "deny" | "denied" => Some(ApprovalMode::Never),
            _ => None,
        }
    }
}

/// DeepSeek reasoning-effort tier, mirrored on ChatGPT/Claude effort pickers.
///
/// The config file accepts all five string values for forward-compat with
/// providers that expose the full spectrum; DeepSeek currently collapses
/// `Low`/`Medium` → `high` and `Max` → `max` at the API boundary. The
/// keyboard cycler (Shift+Tab) walks only the three behaviorally distinct
/// tiers: `Off` → `High` → `Max` → `Off`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    Off,
    Low,
    Medium,
    High,
    Auto,
    #[default]
    Max,
}

impl ReasoningEffort {
    /// Parse a config-file string into an effort tier. `None` for
    /// unrecognized spellings — callers warn and fall back to the default
    /// (`Max`) instead of the parse silently choosing a tier.
    #[must_use]
    pub fn from_setting(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "disabled" | "none" | "false" => Some(Self::Off),
            "low" | "minimal" => Some(Self::Low),
            "medium" | "mid" => Some(Self::Medium),
            "high" => Some(Self::High),
            "auto" | "automatic" => Some(Self::Auto),
            "max" | "maximum" | "xhigh" => Some(Self::Max),
            _ => None,
        }
    }

    /// Canonical lowercase label used for config storage and UI hints.
    #[must_use]
    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Auto => "auto",
            Self::Max => "max",
        }
    }

    /// Short label for the header chip.
    #[must_use]
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "med",
            Self::High => "high",
            Self::Auto => "auto",
            Self::Max => "max",
        }
    }

    /// Value forwarded to the engine/client. `None` means "provider default"
    /// (for `Off` we still emit `"off"` so the client can inject
    /// `thinking = {"type": "disabled"}`).
    #[must_use]
    pub fn api_value(self) -> Option<&'static str> {
        Some(self.as_setting())
    }

    /// Cycle through the three behaviorally distinct tiers.
    #[must_use]
    pub fn cycle_next(self) -> Self {
        match self {
            Self::Off => Self::High,
            Self::Auto => Self::Off,
            Self::Low | Self::Medium | Self::High => Self::Max,
            Self::Max => Self::Off,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AppMode, ReasoningEffort};

    #[test]
    fn cycle_next_walks_the_repl_ring() {
        // Default: three stops, the experimental coordinator skipped.
        assert_eq!(AppMode::Agent.cycle_next(false), AppMode::Plan);
        assert_eq!(AppMode::Plan.cycle_next(false), AppMode::Yolo);
        assert_eq!(AppMode::Yolo.cycle_next(false), AppMode::Agent);
        // Feature enabled: coordinator is the fourth stop.
        assert_eq!(AppMode::Yolo.cycle_next(true), AppMode::Coordinator);
        assert_eq!(AppMode::Coordinator.cycle_next(true), AppMode::Agent);
        // Sitting in coordinator with the feature off still exits the ring.
        assert_eq!(AppMode::Coordinator.cycle_next(false), AppMode::Agent);
    }

    #[test]
    fn from_setting_rejects_typos_instead_of_defaulting() {
        assert_eq!(AppMode::from_setting("agent"), Some(AppMode::Agent));
        assert_eq!(AppMode::from_setting("PLAN"), Some(AppMode::Plan));
        assert_eq!(AppMode::from_setting("yolo"), Some(AppMode::Yolo));
        // A typo must not land the session in full-access Agent silently.
        assert_eq!(AppMode::from_setting("yoloo"), None);
        assert_eq!(AppMode::from_setting("agentic"), None);
        assert_eq!(ReasoningEffort::from_setting("hgh"), None);
        assert_eq!(
            ReasoningEffort::from_setting("max "),
            Some(ReasoningEffort::Max)
        );
    }
}
