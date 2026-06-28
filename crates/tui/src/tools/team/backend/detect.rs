//! Backend selection — choose where spawned teammates run.
//!
//! Detection order:
//! 1. `CODESMITH_TEAM_BACKEND` override (`in_process` | `tmux` | `iterm`).
//!    Lets users force a backend regardless of environment, and drives
//!    tests without touching the real terminal.
//! 2. `$TMUX` set → `tmux` (we are inside a tmux session, so `tmux
//!    split-window` targets the current session/window).
//! 3. `$TERM_PROGRAM == iTerm.app` → `iterm` (AppleScript tab creation).
//! 4. Otherwise → `in_process` (a supervised tokio task, the default).
//!
//! The result is cached for the process lifetime via a `OnceLock` — the
//! terminal environment does not change mid-run.

use std::sync::OnceLock;

use super::BackendKind;

static DETECTED: OnceLock<BackendKind> = OnceLock::new();

/// Resolve the backend kind for the current process, cached after the first
/// call.
///
/// Tests should call [`reset_cache_for_tests`] between scenarios.
#[must_use]
pub fn detect_backend_kind() -> BackendKind {
    *DETECTED.get_or_init(detect_backend_kind_uncached)
}

/// Bypass the cache — the raw detection logic. Exposed so tests can exercise
/// the decision table without polluting the global cache.
#[must_use]
pub fn detect_backend_kind_uncached() -> BackendKind {
    detect_backend_kind_from_env(&std::env::vars().collect())
}

/// Pure decision table over a snapshot of the process environment.
#[must_use]
pub fn detect_backend_kind_from_env(env: &std::collections::HashMap<String, String>) -> BackendKind {
    // 1. Explicit override.
    if let Some(value) = env.get("CODESMITH_TEAM_BACKEND") {
        return parse_backend_kind(value)
            .unwrap_or_else(|| fallback_from_env(env));
    }
    fallback_from_env(env)
}

fn fallback_from_env(env: &std::collections::HashMap<String, String>) -> BackendKind {
    // 2. Inside a tmux session — `tmux split-window` targets the active pane.
    if env.get("TMUX").is_some() {
        return BackendKind::Tmux;
    }
    // 3. Running inside iTerm2 — AppleScript can open a new tab.
    if env.get("TERM_PROGRAM").map(String::as_str) == Some("iTerm.app") {
        return BackendKind::Iter;
    }
    // 4. Default: run teammates in the leader's process.
    BackendKind::InProcess
}

fn parse_backend_kind(value: &str) -> Option<BackendKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "in_process" | "in-process" | "inprocess" => Some(BackendKind::InProcess),
        "tmux" => Some(BackendKind::Tmux),
        "iterm" | "iterm2" => Some(BackendKind::Iter),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn default_is_in_process() {
        assert_eq!(detect_backend_kind_from_env(&env(&[])), BackendKind::InProcess);
    }

    #[test]
    fn tmux_env_selects_tmux() {
        assert_eq!(
            detect_backend_kind_from_env(&env(&[("TMUX", "/tmp/tmux-1000/default,123,0")])),
            BackendKind::Tmux
        );
    }

    #[test]
    fn iterm_env_selects_iterm() {
        assert_eq!(
            detect_backend_kind_from_env(&env(&[("TERM_PROGRAM", "iTerm.app")])),
            BackendKind::Iter
        );
    }

    #[test]
    fn override_takes_priority() {
        let e = env(&[
            ("TMUX", "/tmp/tmux-1000/default,123,0"),
            ("TERM_PROGRAM", "iTerm.app"),
            ("CODESMITH_TEAM_BACKEND", "in_process"),
        ]);
        assert_eq!(detect_backend_kind_from_env(&e), BackendKind::InProcess);
    }

    #[test]
    fn override_accepts_hyphenated_and_case_variants() {
        for v in &["in-process", "InProcess", "INPROCESS", "in_process"] {
            let e = env(&[("CODESMITH_TEAM_BACKEND", v)]);
            assert_eq!(
                detect_backend_kind_from_env(&e),
                BackendKind::InProcess,
                "value = {v}"
            );
        }
        for v in &["iterm2", "ITERM", "iTerm2"] {
            let e = env(&[("CODESMITH_TEAM_BACKEND", v)]);
            assert_eq!(
                detect_backend_kind_from_env(&e),
                BackendKind::Iter,
                "value = {v}"
            );
        }
    }

    #[test]
    fn unknown_override_falls_back_to_env() {
        let e = env(&[
            ("CODESMITH_TEAM_BACKEND", "frobnicate"),
            ("TMUX", "/tmp/tmux-1000/default,123,0"),
        ]);
        assert_eq!(detect_backend_kind_from_env(&e), BackendKind::Tmux);
    }

    #[test]
    fn tmux_takes_priority_over_iterm_when_both_set() {
        // If both TMUX and TERM_PROGRAM=iTerm.app are present (running tmux
        // *inside* iTerm2), prefer tmux — split-window keeps teammates in
        // the same multiplexer session the user is managing.
        let e = env(&[
            ("TMUX", "/tmp/tmux-1000/default,123,0"),
            ("TERM_PROGRAM", "iTerm.app"),
        ]);
        assert_eq!(detect_backend_kind_from_env(&e), BackendKind::Tmux);
    }
}
