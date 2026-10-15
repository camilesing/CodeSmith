//! Policy readers for the output-presentation mechanisms (diff rendering,
//! shell-output truncation). Mechanisms stay here in Rust; the knobs come
//! from the scriptable policy seam ([`codesmith_agent::policy`]).
//!
//! Resolution order at read time: published policy (the runner flushed
//! builtin + user mods, last-wins) → compiled defaults (below). In a
//! runner-less context (embeds, unit tests) nothing has flushed, so the
//! first read seeds the process-global snapshot from the embedded builtin
//! tier — one implementation of the defaults, two materialization paths.

use std::collections::HashMap;
use std::sync::OnceLock;

use codesmith_agent::policy::{PolicyKind, any_policies_published, policy_usize, publish_policies};

/// Compiled fallback for the diff context radius (the value the builtin
/// `output-diff` mod also registers — `builtin/output_diff.rhai`).
pub const DEFAULT_DIFF_CONTEXT_RADIUS: usize = 3;
/// Upper bound for the radius: a huge radius floods the model's context
/// with unchanged lines, so a bad policy value falls back instead.
const MAX_DIFF_CONTEXT_RADIUS: usize = 64;

/// The shell-output knobs, with the pre-0.11.0 compiled values as
/// defaults (the builtin `shell-output` mod registers the same numbers —
/// `builtin/shell_output.rhai`).
#[derive(Debug, Clone, Copy)]
pub struct ShellOutputPolicy {
    pub max_output_size: usize,
    pub head_bytes: usize,
    pub tail_bytes: usize,
    pub summary_max_lines: usize,
    pub summary_max_chars: usize,
    pub preserved_summary_lines: usize,
    pub preserved_line_bytes: usize,
    pub preserved_total_bytes: usize,
}

impl Default for ShellOutputPolicy {
    fn default() -> Self {
        Self {
            max_output_size: 30_000,
            head_bytes: 22_000,
            tail_bytes: 8_000,
            summary_max_lines: 3,
            summary_max_chars: 240,
            preserved_summary_lines: 80,
            preserved_line_bytes: 512,
            preserved_total_bytes: 8_192,
        }
    }
}

/// Hard ceilings for each knob (a bad policy value falls back to the
/// compiled default rather than starving or flooding the context).
const SHELL_OUTPUT_CEILINGS: ShellOutputPolicy = ShellOutputPolicy {
    max_output_size: 1_000_000,
    head_bytes: 1_000_000,
    tail_bytes: 1_000_000,
    summary_max_lines: 64,
    summary_max_chars: 4_096,
    preserved_summary_lines: 1_000,
    preserved_line_bytes: 8_192,
    preserved_total_bytes: 131_072,
};

/// Seed the process-global policy snapshot from the embedded builtin tier
/// when nothing was flushed (runner-less contexts). Idempotent; same
/// process-wide data, so a benign race is harmless.
fn ensure_seeded() {
    static SEED: OnceLock<()> = OnceLock::new();
    SEED.get_or_init(|| {
        if any_policies_published() {
            return;
        }
        publish_policies(
            codesmith_extensions::builtin_policy_defaults()
                .into_iter()
                .collect::<HashMap<PolicyKind, serde_json::Value>>(),
        );
    });
}

/// The effective diff context radius.
pub fn diff_context_radius() -> usize {
    ensure_seeded();
    policy_usize(
        PolicyKind::OutputDiff,
        "context_radius",
        DEFAULT_DIFF_CONTEXT_RADIUS,
        MAX_DIFF_CONTEXT_RADIUS,
    )
}

/// The effective shell-output policy (validated: `head + tail ≤ max`, else
/// the compiled defaults).
pub fn shell_output() -> ShellOutputPolicy {
    ensure_seeded();
    let defaults = ShellOutputPolicy::default();
    let kind = PolicyKind::ShellOutput;
    let p = ShellOutputPolicy {
        max_output_size: policy_usize(
            kind,
            "max_output_size",
            defaults.max_output_size,
            SHELL_OUTPUT_CEILINGS.max_output_size,
        ),
        head_bytes: policy_usize(
            kind,
            "head_bytes",
            defaults.head_bytes,
            SHELL_OUTPUT_CEILINGS.head_bytes,
        ),
        tail_bytes: policy_usize(
            kind,
            "tail_bytes",
            defaults.tail_bytes,
            SHELL_OUTPUT_CEILINGS.tail_bytes,
        ),
        summary_max_lines: policy_usize(
            kind,
            "summary_max_lines",
            defaults.summary_max_lines,
            SHELL_OUTPUT_CEILINGS.summary_max_lines,
        ),
        summary_max_chars: policy_usize(
            kind,
            "summary_max_chars",
            defaults.summary_max_chars,
            SHELL_OUTPUT_CEILINGS.summary_max_chars,
        ),
        preserved_summary_lines: policy_usize(
            kind,
            "preserved_summary_lines",
            defaults.preserved_summary_lines,
            SHELL_OUTPUT_CEILINGS.preserved_summary_lines,
        ),
        preserved_line_bytes: policy_usize(
            kind,
            "preserved_line_bytes",
            defaults.preserved_line_bytes,
            SHELL_OUTPUT_CEILINGS.preserved_line_bytes,
        ),
        preserved_total_bytes: policy_usize(
            kind,
            "preserved_total_bytes",
            defaults.preserved_total_bytes,
            SHELL_OUTPUT_CEILINGS.preserved_total_bytes,
        ),
    };
    if p.head_bytes.saturating_add(p.tail_bytes) > p.max_output_size {
        tracing::warn!(
            target: "codesmith_policy",
            "shell-output policy has head {} + tail {} > max {}; using compiled defaults",
            p.head_bytes,
            p.tail_bytes,
            p.max_output_size
        );
        return defaults;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent::policy::clear_published_policies;
    use serde_json::json;

    /// The snapshot is process-global; tests in this module serialize
    /// through this guard (publish/read/clear as one critical section).
    fn policy_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        match LOCK.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn with_policy(kind: PolicyKind, value: serde_json::Value) {
        let mut set = HashMap::new();
        set.insert(kind, value);
        publish_policies(set);
    }

    #[test]
    fn defaults_match_builtin_registration() {
        // The compiled fallback and the builtin Rhai registration must
        // agree — this is the drift guard for the duplicated defaults.
        let _guard = policy_test_lock();
        clear_published_policies();
        let p = shell_output();
        let d = ShellOutputPolicy::default();
        assert_eq!(p.max_output_size, d.max_output_size);
        assert_eq!(p.head_bytes, d.head_bytes);
        assert_eq!(p.tail_bytes, d.tail_bytes);
        assert_eq!(diff_context_radius(), DEFAULT_DIFF_CONTEXT_RADIUS);
        clear_published_policies();
    }

    #[test]
    fn override_changes_and_bad_values_fall_back() {
        let _guard = policy_test_lock();
        with_policy(PolicyKind::OutputDiff, json!({"context_radius": 5}));
        assert_eq!(diff_context_radius(), 5);
        with_policy(PolicyKind::OutputDiff, json!({"context_radius": 999_999}));
        assert_eq!(diff_context_radius(), DEFAULT_DIFF_CONTEXT_RADIUS);

        with_policy(
            PolicyKind::ShellOutput,
            json!({"max_output_size": 5000, "head_bytes": 3000, "tail_bytes": 2000}),
        );
        let p = shell_output();
        assert_eq!(p.max_output_size, 5000);
        // head + tail > max → whole struct falls back.
        with_policy(
            PolicyKind::ShellOutput,
            json!({"max_output_size": 1000, "head_bytes": 3000, "tail_bytes": 2000}),
        );
        assert_eq!(
            shell_output().max_output_size,
            ShellOutputPolicy::default().max_output_size
        );
        clear_published_policies();
    }
}
