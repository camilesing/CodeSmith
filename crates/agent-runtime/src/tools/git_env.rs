//! Git invocation environment sanitization.
//!
//! When CodeSmith shells out to `git`, the host's `core.fsmonitor` and
//! `core.hooksPath` configuration can silently alter git's behaviour (spawn
//! fsmonitor daemons, run hook scripts outside the sandbox). These helpers
//! inject an indexed `GIT_CONFIG_*` overlay that blanks both keys for the
//! child process, so sandboxed/automated git invocations behave
//! deterministically without mutating the user's on-disk git config.
//!
//! Terminal-agnostic (pure `std::collections::HashMap`); lives in the runtime
//! crate so the `ShellManager` and any downstream tool implementation can call
//! it without depending on the `codesmith-tui` binary.

use std::collections::HashMap;

/// Build the git-config overlay that blanks `core.fsmonitor` and
/// `core.hooksPath` for a child git process.
pub fn git_scrub_env() -> HashMap<String, String> {
    HashMap::from([
        ("GIT_CONFIG_COUNT".to_string(), "2".to_string()),
        ("GIT_CONFIG_KEY_0".to_string(), "core.fsmonitor".to_string()),
        ("GIT_CONFIG_VALUE_0".to_string(), "".to_string()),
        ("GIT_CONFIG_KEY_1".to_string(), "core.hooksPath".to_string()),
        ("GIT_CONFIG_VALUE_1".to_string(), "".to_string()),
    ])
}

/// Merge the git-config scrub overlay into `env`, unless the caller has
/// already set `GIT_CONFIG_COUNT` (in which case their explicit configuration
/// is respected to avoid constructing a conflicting indexed overlay).
pub fn merge_git_scrub_env(env: &mut HashMap<String, String>) {
    if env.contains_key("GIT_CONFIG_COUNT") {
        // Respect explicit caller configuration rather than constructing a
        // potentially conflicting indexed git-config environment.
        return;
    }
    env.extend(git_scrub_env());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_adds_scrub_keys_when_unset() {
        let mut env = HashMap::new();
        merge_git_scrub_env(&mut env);
        assert_eq!(env.get("GIT_CONFIG_COUNT"), Some(&"2".to_string()));
        assert_eq!(
            env.get("GIT_CONFIG_KEY_0"),
            Some(&"core.fsmonitor".to_string())
        );
        assert_eq!(env.get("GIT_CONFIG_VALUE_0"), Some(&"".to_string()));
        assert_eq!(
            env.get("GIT_CONFIG_KEY_1"),
            Some(&"core.hooksPath".to_string())
        );
        assert_eq!(env.get("GIT_CONFIG_VALUE_1"), Some(&"".to_string()));
    }

    #[test]
    fn merge_respects_explicit_git_config_count() {
        let mut env = HashMap::new();
        env.insert("GIT_CONFIG_COUNT".to_string(), "1".to_string());
        env.insert("GIT_CONFIG_KEY_0".to_string(), "user.email".to_string());
        merge_git_scrub_env(&mut env);
        // Caller's overlay is preserved; scrub keys are NOT injected.
        assert_eq!(env.get("GIT_CONFIG_COUNT"), Some(&"1".to_string()));
        assert_eq!(env.get("GIT_CONFIG_KEY_0"), Some(&"user.email".to_string()));
        assert!(!env.contains_key("GIT_CONFIG_KEY_1"));
    }

    #[test]
    fn scrub_env_shape_is_stable() {
        let env = git_scrub_env();
        assert_eq!(env.len(), 5);
        assert_eq!(env.get("GIT_CONFIG_COUNT"), Some(&"2".to_string()));
    }
}
