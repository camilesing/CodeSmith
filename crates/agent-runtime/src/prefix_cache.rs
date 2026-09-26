//! Prefix-cache stability manager (inspired by Reasonix's Pillar 1).
//!
//! DeepSeek's automatic prefix caching activates only when the *exact*
//! byte prefix of a request matches the prior request. Any system-prompt
//! drift, tool-list reordering, or message-rewriting busts the cache
//! for every token after the changed byte.
//!
//! This module provides a `PrefixStabilityManager` that:
//!
//! 1. **Fingerprints** the immutable prefix (system prompt + tool specs)
//!    at session start, using SHA-256 for strong collision resistance.
//! 2. **Detects drift** by comparing the current prefix against the
//!    pinned fingerprint before every request.
//! 3. **Diagnoses** the cause of drift — did the system prompt change?
//!    Did the tool set change? Both?
//! 4. **Emits events** so the TUI can surface stability to the user.
//!
//! ## Three-region model (from Reasonix)
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │ IMMUTABLE PREFIX                        │ ← fixed for session
//! │   system + tool_specs                    │   cache hit candidate
//! ├─────────────────────────────────────────┤
//! │ APPEND-ONLY HISTORY                     │ ← grows monotonically
//! │   [assistant₁][tool₁][assistant₂]...    │   preserves prefix of prior turns
//! ├─────────────────────────────────────────┤
//! │ LATEST USER TURN                        │ ← the only new content per request
//! └─────────────────────────────────────────┘
//! ```
//!
//! The fingerprint *is* [`FrozenPrefix`] from [`crate::prompt_zones`] — the
//! same type the request path freezes per step. One fingerprint
//! implementation, one source of truth: what `/cache zones` displays is
//! exactly what the request path verifies. Tool identity hashes the full
//! sorted JSON of every tool definition (not just names), so a description
//! or schema edit is detected as drift even when the tool's name and
//! catalog position are unchanged.

use serde::{Deserialize, Serialize};

use crate::prompt_zones::FrozenPrefix;

/// A change record describing what drifted in the prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefixChange {
    /// Combined SHA-256 of the pinned prefix before the change.
    pub old_sha256: String,
    /// Combined SHA-256 of the prefix after the change.
    pub new_sha256: String,
    /// Whether the system prompt component changed.
    pub system_changed: bool,
    /// Whether the tool set component changed.
    pub tools_changed: bool,
}

impl PrefixChange {
    /// Returns a human-readable description of what changed.
    #[must_use]
    pub fn description(&self) -> String {
        let mut parts = Vec::new();
        if self.system_changed {
            parts.push("system prompt");
        }
        if self.tools_changed {
            parts.push("tool set");
        }
        if parts.is_empty() {
            return "unknown (fingerprint mismatch but no component detected)".to_string();
        }
        format!("prefix cache invalidated: {} changed", parts.join(" and "))
    }

    /// Returns a short label for TUI chip display.
    #[must_use]
    #[allow(dead_code)] // surfaced via tests; kept for future TUI chip use
    pub fn label(&self) -> &'static str {
        match (self.system_changed, self.tools_changed) {
            (true, true) => "sys+tools",
            (true, false) => "sys",
            (false, true) => "tools",
            (false, false) => "prefix",
        }
    }
}

/// Monitors and manages prefix-cache stability across turns.
///
/// This is the core abstraction, mirroring Reasonix's `ImmutablePrefix`
/// concept but adapted to CodeSmith's existing architecture where the
/// system prompt is rebuilt each turn and tools are registered at startup.
///
/// The engine freezes one [`FrozenPrefix`] per step (via
/// `prompt_zones::PinnedPrefix`) and calls [`check_and_update`](Self::check_and_update);
/// drift emits [`crate::events::Event::PrefixCacheChange`] for the TUI.
#[derive(Debug, Clone)]
pub struct PrefixStabilityManager {
    /// The pinned fingerprint from session start or last stabilization.
    pinned: Option<FrozenPrefix>,
    /// The most recent fingerprint (computed during last check).
    current: Option<FrozenPrefix>,
    /// The last detected change, if any.
    last_change: Option<PrefixChange>,
    /// Total number of prefix changes detected this session.
    change_count: u64,
    /// Total number of stability checks performed.
    check_count: u64,
}

impl PrefixStabilityManager {
    /// Create a manager in "unpinned" state — no initial fingerprint.
    /// The first `check_and_update` establishes the baseline.
    #[must_use]
    pub fn new_unpinned() -> Self {
        Self {
            pinned: None,
            current: None,
            last_change: None,
            change_count: 0,
            check_count: 0,
        }
    }

    /// Check whether the current prefix matches the pinned fingerprint.
    /// Updates internal state and returns:
    /// - `Ok(true)` if the prefix is stable (fingerprint matches pinned).
    /// - `Err(change)` if the prefix changed; caller should surface this.
    ///
    /// After calling this, `last_change()` returns the detected change.
    /// On drift the manager re-pins to the new prefix, so the *next* drift
    /// is measured against the latest baseline.
    pub fn check_and_update(&mut self, frozen: &FrozenPrefix) -> Result<bool, Box<PrefixChange>> {
        self.current = Some(frozen.clone());
        self.check_count += 1;

        let Some(pinned) = self.pinned.clone() else {
            // First check: pin now.
            self.pinned = Some(frozen.clone());
            self.last_change = None;
            return Ok(true);
        };

        if frozen.combined_sha256 == pinned.combined_sha256 {
            // Stable — no change.
            Ok(true)
        } else {
            // Change detected. Compare components for the diagnosis.
            let system_changed = frozen.system_text != pinned.system_text;
            let tools_changed = frozen.tool_catalog != pinned.tool_catalog;

            let change = PrefixChange {
                old_sha256: pinned.combined_sha256,
                new_sha256: frozen.combined_sha256.clone(),
                system_changed,
                tools_changed,
            };

            self.last_change = Some(change.clone());
            self.change_count += 1;

            // Re-pin to the new prefix so subsequent checks are
            // against the latest baseline.
            self.pinned = Some(frozen.clone());

            Err(Box::new(change))
        }
    }

    /// Returns the most recent prefix change, if any.
    #[must_use]
    pub fn last_change(&self) -> Option<&PrefixChange> {
        self.last_change.as_ref()
    }

    /// Returns the pinned fingerprint.
    #[must_use]
    pub fn pinned_fingerprint(&self) -> Option<&FrozenPrefix> {
        self.pinned.as_ref()
    }

    /// Returns the current (most recently computed) fingerprint.
    #[must_use]
    pub fn current_fingerprint(&self) -> Option<&FrozenPrefix> {
        self.current.as_ref()
    }

    /// Returns the total number of prefix changes detected.
    #[must_use]
    pub fn change_count(&self) -> u64 {
        self.change_count
    }

    /// Returns the total number of stability checks performed.
    #[must_use]
    pub fn check_count(&self) -> u64 {
        self.check_count
    }

    /// Returns the prefix stability rate as a fraction (0.0 – 1.0).
    /// 1.0 means the prefix has never changed. Returns 1.0 when no
    /// checks have been performed (to avoid division by zero).
    #[must_use]
    pub fn stability_ratio(&self) -> f64 {
        if self.check_count == 0 {
            1.0
        } else {
            let stable_checks = self.check_count - self.change_count;
            stable_checks as f64 / self.check_count as f64
        }
    }

    /// Returns a human-readable stability summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let pct = self.stability_ratio() * 100.0;
        let pinned_short = self
            .pinned
            .as_ref()
            .map(FrozenPrefix::short_id)
            .unwrap_or("none");

        format!(
            "Prefix stability: {pct:.1}% ({stable}/{total} checks stable) | fingerprint: {pinned_short} | changes: {changes}",
            pct = pct,
            stable = self.check_count.saturating_sub(self.change_count),
            total = self.check_count,
            pinned_short = pinned_short,
            changes = self.change_count,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{SystemPrompt, Tool};
    use crate::prompt_zones::PinnedPrefix;

    fn make_tool(name: &str) -> Tool {
        Tool {
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::Value::Null,
            output_schema: None,
            tool_type: None,
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }

    fn freeze(system: &str, tools: &[Tool]) -> FrozenPrefix {
        let sys = SystemPrompt::Text(system.to_string());
        PinnedPrefix::new(Some(&sys), tools.to_vec()).freeze()
    }

    #[test]
    fn same_prefix_produces_same_fingerprint() {
        let a = freeze("hello world", &[]);
        let b = freeze("hello world", &[]);
        assert_eq!(a.combined_sha256, b.combined_sha256);
    }

    #[test]
    fn different_system_produces_different_fingerprint() {
        let a = freeze("hello", &[]);
        let b = freeze("world", &[]);
        assert_ne!(a.combined_sha256, b.combined_sha256);
    }

    #[test]
    fn tool_order_does_not_affect_fingerprint() {
        let tools_a = vec![make_tool("read_file"), make_tool("write_file")];
        let tools_b = vec![make_tool("write_file"), make_tool("read_file")];
        let a = freeze("system", &tools_a);
        let b = freeze("system", &tools_b);
        assert_eq!(a.combined_sha256, b.combined_sha256);
    }

    #[test]
    fn different_tools_produce_different_fingerprint() {
        let tools_a = vec![make_tool("read_file")];
        let tools_b = vec![make_tool("write_file")];
        let a = freeze("system", &tools_a);
        let b = freeze("system", &tools_b);
        assert_ne!(a.combined_sha256, b.combined_sha256);
    }

    #[test]
    fn tool_description_change_is_detected() {
        // Full-JSON tool fingerprinting: same name, different description.
        // The retired names-only hash missed this class of drift (#2264).
        let mut tool_v2 = make_tool("read_file");
        tool_v2.description = "updated description".to_string();
        let a = freeze("system", &[make_tool("read_file")]);
        let b = freeze("system", &[tool_v2]);
        assert_ne!(a.combined_sha256, b.combined_sha256);

        let mut mgr = PrefixStabilityManager::new_unpinned();
        assert!(mgr.check_and_update(&a).unwrap());
        let change = mgr.check_and_update(&b).unwrap_err();
        assert!(change.tools_changed);
        assert!(!change.system_changed);
    }

    #[test]
    fn manager_starts_stable() {
        let frozen = freeze("system prompt", &[]);
        let mut mgr = PrefixStabilityManager::new_unpinned();
        assert!(mgr.check_and_update(&frozen).unwrap());
        assert_eq!(mgr.change_count(), 0);
        assert_eq!(mgr.check_count(), 1);
    }

    #[test]
    fn manager_detects_change() {
        let mut mgr = PrefixStabilityManager::new_unpinned();
        assert!(mgr.check_and_update(&freeze("system prompt", &[])).unwrap());
        let result = mgr.check_and_update(&freeze("different prompt", &[]));
        assert!(result.is_err());
        assert_eq!(mgr.change_count(), 1);
        let change = mgr.last_change().unwrap();
        assert!(change.system_changed);
        assert!(!change.tools_changed);
    }

    #[test]
    fn manager_detects_tool_change() {
        let tools_a = vec![make_tool("read_file")];
        let tools_b = vec![make_tool("write_file")];
        let mut mgr = PrefixStabilityManager::new_unpinned();
        assert!(mgr.check_and_update(&freeze("system", &tools_a)).unwrap());
        let result = mgr.check_and_update(&freeze("system", &tools_b));
        assert!(result.is_err());
        let change = mgr.last_change().unwrap();
        assert!(!change.system_changed);
        assert!(change.tools_changed);
    }

    #[test]
    fn manager_re_pins_after_change() {
        let mut mgr = PrefixStabilityManager::new_unpinned();
        let _ = mgr.check_and_update(&freeze("old", &[])); // pin
        let _ = mgr.check_and_update(&freeze("new", &[])); // drift, re-pin
        // After re-pin, the new "new" should be stable.
        assert!(mgr.check_and_update(&freeze("new", &[])).unwrap());
        assert_eq!(mgr.change_count(), 1);
    }

    #[test]
    fn stability_ratio_is_one_for_no_changes() {
        let frozen = freeze("hello", &[]);
        let mut mgr = PrefixStabilityManager::new_unpinned();
        mgr.check_and_update(&frozen).unwrap();
        mgr.check_and_update(&frozen).unwrap();
        assert!((mgr.stability_ratio() - 1.0).abs() < f64::EPSILON);
        assert_eq!(mgr.check_count(), 2);
        assert_eq!(mgr.change_count(), 0);
    }

    #[test]
    fn stability_ratio_reflects_change_rate() {
        let mut mgr = PrefixStabilityManager::new_unpinned();
        mgr.check_and_update(&freeze("hello", &[])).unwrap(); // check 1: pin, stable
        let _ = mgr.check_and_update(&freeze("world", &[])); // check 2: changed
        mgr.check_and_update(&freeze("world", &[])).unwrap(); // check 3: stable
        // 2 stable out of 3 checks = 0.666...
        assert!((mgr.stability_ratio() - 2.0 / 3.0).abs() < 0.01);
        assert_eq!(mgr.check_count(), 3);
        assert_eq!(mgr.change_count(), 1);
    }

    #[test]
    fn empty_tools_and_no_tools_produce_same_hash() {
        let empty = freeze("system", &[]);
        assert_eq!(empty.tool_catalog, "");
    }

    #[test]
    fn prefix_change_description_is_informative() {
        let change = PrefixChange {
            old_sha256: "a".repeat(64),
            new_sha256: "b".repeat(64),
            system_changed: true,
            tools_changed: false,
        };
        assert_eq!(
            change.description(),
            "prefix cache invalidated: system prompt changed"
        );
        assert_eq!(change.label(), "sys");
    }

    #[test]
    fn new_unpinned_has_no_change_history() {
        let mut mgr = PrefixStabilityManager::new_unpinned();
        assert!(mgr.pinned_fingerprint().is_none());
        assert!(mgr.current_fingerprint().is_none());
        assert!(mgr.last_change().is_none());
        assert_eq!(mgr.change_count(), 0);
        assert_eq!(mgr.check_count(), 0);
        // First check should pin automatically and count as a check.
        assert!(mgr.check_and_update(&freeze("hello", &[])).unwrap());
        assert!(mgr.pinned_fingerprint().is_some());
        assert_eq!(mgr.check_count(), 1);
    }
}
