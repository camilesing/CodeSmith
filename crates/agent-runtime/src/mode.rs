//! Execution and approval modes for the agent runtime.
//!
//! These were relocated from the TUI (`tui::app::AppMode`,
//! `tui::app::ReasoningEffort`, and `tui::approval::ApprovalMode`) so that
//! the runtime can mode-switch toolsets and sandbox policy without depending
//! on the terminal binary. The TUI re-exports them at their historical paths
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
    #[must_use]
    pub fn from_setting(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "plan" => Self::Plan,
            "yolo" => Self::Yolo,
            "coordinator" | "coordinator_mode" => Self::Coordinator,
            _ => Self::Agent,
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
    /// Parse a config-file string into an effort tier. Unknown values fall
    /// back to the default (`Max`) rather than erroring out.
    #[must_use]
    pub fn from_setting(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "disabled" | "none" | "false" => Self::Off,
            "low" | "minimal" => Self::Low,
            "medium" | "mid" => Self::Medium,
            "high" => Self::High,
            "auto" | "automatic" => Self::Auto,
            "max" | "maximum" | "xhigh" => Self::Max,
            _ => Self::default(),
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
