//! Capacity controller re-exported from `codesmith-agent-runtime`.
//!
//! The types and logic live in the agent-runtime crate; this module
//! re-exports them so existing `crate::core::capacity::` references keep
//! resolving. The `from_app_config` constructor stays here as a free
//! function because `crate::config::Config` is tui-side — an inherent `impl`
//! here would violate the orphan rule (`CapacityControllerConfig` is now
//! defined in agent-runtime).

pub use codesmith_agent_runtime::capacity::*;

/// Build effective capacity config from app config.
///
/// Kept in tui as a free function: `Config` is defined in tui while
/// `CapacityControllerConfig` is defined in agent-runtime, so an inherent
/// `from_app_config` impl cannot live in tui (orphan rule).
#[must_use]
pub fn capacity_controller_config_from_app(
    config: &crate::config::Config,
) -> CapacityControllerConfig {
    let mut out = CapacityControllerConfig::default();
    let Some(capacity) = config.capacity.as_ref() else {
        return out;
    };

    if let Some(v) = capacity.enabled {
        out.enabled = v;
    }
    if let Some(v) = capacity.low_risk_max {
        out.low_risk_max = v;
    }
    if let Some(v) = capacity.medium_risk_max {
        out.medium_risk_max = v;
    }
    if let Some(v) = capacity.severe_min_slack {
        out.severe_min_slack = v;
    }
    if let Some(v) = capacity.severe_violation_ratio {
        out.severe_violation_ratio = v;
    }
    if let Some(v) = capacity.refresh_cooldown_turns {
        out.refresh_cooldown_turns = v;
    }
    if let Some(v) = capacity.replan_cooldown_turns {
        out.replan_cooldown_turns = v;
    }
    if let Some(v) = capacity.max_replay_per_turn {
        out.max_replay_per_turn = v;
    }
    if let Some(v) = capacity.min_turns_before_guardrail {
        out.min_turns_before_guardrail = v;
    }
    if let Some(v) = capacity.profile_window {
        out.profile_window = v.max(2);
    }

    if let Some(v) = capacity.deepseek_v3_2_chat_prior {
        out.model_priors.insert("deepseek_v3_2_chat".to_string(), v);
    }
    if let Some(v) = capacity.deepseek_v3_2_reasoner_prior {
        out.model_priors
            .insert("deepseek_v3_2_reasoner".to_string(), v);
    }
    if let Some(v) = capacity.deepseek_v4_pro_prior {
        out.model_priors.insert("deepseek_v4_pro".to_string(), v);
    }
    if let Some(v) = capacity.deepseek_v4_flash_prior {
        out.model_priors.insert("deepseek_v4_flash".to_string(), v);
    }
    if let Some(v) = capacity.fallback_default_prior {
        out.fallback_default = v;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_config_without_capacity_uses_default_disabled() {
        let cfg = capacity_controller_config_from_app(&crate::config::Config::default());
        // v0.8.11: default is disabled. No capacity section in config
        // means the controller stays inert; users opt in deliberately.
        assert!(!cfg.enabled);
        assert_eq!(cfg.low_risk_max, 0.50);
        assert_eq!(cfg.refresh_cooldown_turns, 6);
        assert_eq!(cfg.min_turns_before_guardrail, 4);
        assert_eq!(cfg.model_priors.get("deepseek_v4_pro"), Some(&3.5));
        assert_eq!(cfg.model_priors.get("deepseek_v4_flash"), Some(&4.2));
    }
}
