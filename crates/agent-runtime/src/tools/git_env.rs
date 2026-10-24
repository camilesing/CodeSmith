//! Git invocation environment sanitization.
//!
//! When CodeSmith shells out to `git`, the host's `core.fsmonitor` and
//! `core.hooksPath` configuration can silently alter git's behaviour (spawn
//! fsmonitor daemons, run hook scripts outside the sandbox). These helpers
//! inject an indexed `GIT_CONFIG_*` overlay that disables both keys for the
//! child process (requires git >= 2.31; older versions ignore the overlay),
//! without mutating the user's on-disk git config. The guarantee covers only
//! these two keys and only values from lower-precedence sources: other
//! config — including values injected via `GIT_CONFIG_PARAMETERS` or
//! relocated `GIT_CONFIG_SYSTEM`/`GIT_CONFIG_GLOBAL` files — still applies
//! unless callers also set `GIT_CONFIG_NOSYSTEM`/`GIT_CONFIG_GLOBAL`.
//!
//! Terminal-agnostic (pure `std::collections::HashMap`); lives in the runtime
//! crate so the `ShellManager` and any downstream tool implementation can call
//! it without depending on the host binary.

use std::collections::HashMap;

/// The value `core.hooksPath` is overridden with: an owner-controlled
/// directory that is intentionally **not created**. `core.hooksPath` has no
/// "off" spelling — an empty value degrades to a version-dependent
/// root-relative hook lookup (`/pre-commit`) — so hooks are disabled by
/// pointing them at a path where the lookup deterministically fails. Only
/// the owner can create anything under `~/.codesmith` (0700), so a hook
/// cannot be smuggled in by another local user. The path is derived from
/// `$HOME` DIRECTLY: honoring `$CODESMITH_HOME` here would let the very
/// environment being scrubbed (which can inject `GIT_CONFIG_*` can equally
/// set `CODESMITH_HOME`) redirect the hook lookup at an attacker-controlled
/// directory containing real hook scripts — or at a relative path resolving
/// inside the sandboxed workspace.
fn hooks_disabled_path() -> String {
    dirs::home_dir()
        .map(|home| {
            home.join(".codesmith")
                .join("hooks-disabled")
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_default()
}

/// Build the git-config overlay that disables `core.fsmonitor` and
/// `core.hooksPath` for a child git process.
pub fn git_scrub_env() -> HashMap<String, String> {
    HashMap::from([
        ("GIT_CONFIG_COUNT".to_string(), "2".to_string()),
        ("GIT_CONFIG_KEY_0".to_string(), "core.fsmonitor".to_string()),
        // Explicit boolean disable; `""` only disables fsmonitor because
        // git's boolean parser maps empty to false.
        ("GIT_CONFIG_VALUE_0".to_string(), "false".to_string()),
        ("GIT_CONFIG_KEY_1".to_string(), "core.hooksPath".to_string()),
        ("GIT_CONFIG_VALUE_1".to_string(), hooks_disabled_path()),
    ])
}

/// Merge the git-config scrub overlay into `env`, re-indexing around any
/// existing `GIT_CONFIG_COUNT`.
///
/// An inherited count (a `shell_env` hook, CI wrapper, or parent git
/// tooling exporting indexed config) must not silently skip the scrub and
/// re-enable `core.hooksPath`/`core.fsmonitor` — and a malformed inherited
/// count makes git >= 2.31 hard-fail ("bogus format in GIT_CONFIG_COUNT").
/// So instead of skipping, the scrub entries are appended at the highest
/// indices and the count is bumped: git applies later indices last, so the
/// scrub entries always take precedence. A non-numeric inherited count is
/// treated as base 0 (its dangling `GIT_CONFIG_KEY_<n>` entries, if any,
/// would have broken git anyway).
pub fn merge_git_scrub_env(env: &mut HashMap<String, String>) {
    let base = env
        .get("GIT_CONFIG_COUNT")
        .and_then(|count| count.trim().parse::<u32>().ok())
        .unwrap_or(0);
    env.insert("GIT_CONFIG_COUNT".to_string(), (base + 2).to_string());
    env.insert(
        format!("GIT_CONFIG_KEY_{base}"),
        "core.fsmonitor".to_string(),
    );
    env.insert(format!("GIT_CONFIG_VALUE_{base}"), "false".to_string());
    env.insert(
        format!("GIT_CONFIG_KEY_{}", base + 1),
        "core.hooksPath".to_string(),
    );
    env.insert(
        format!("GIT_CONFIG_VALUE_{}", base + 1),
        hooks_disabled_path(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_disabled_path_is_absolute_and_home_anchored() {
        // The scrub target must derive from the real home directory, NOT
        // from `$CODESMITH_HOME` (the environment being scrubbed can set it
        // too), and must never be relative — a relative hooksPath resolves
        // inside the sandboxed workspace.
        let path = hooks_disabled_path();
        let parsed = std::path::Path::new(&path);
        assert!(
            parsed.is_absolute(),
            "hooksPath must be absolute, got {path:?}"
        );
        let home = dirs::home_dir().expect("home dir");
        assert!(
            parsed.starts_with(&home),
            "hooksPath must anchor under the real home, got {path:?}"
        );
    }

    #[test]
    fn merge_adds_scrub_keys_when_unset() {
        let mut env = HashMap::new();
        merge_git_scrub_env(&mut env);
        assert_eq!(env.get("GIT_CONFIG_COUNT"), Some(&"2".to_string()));
        assert_eq!(
            env.get("GIT_CONFIG_KEY_0"),
            Some(&"core.fsmonitor".to_string())
        );
        assert_eq!(env.get("GIT_CONFIG_VALUE_0"), Some(&"false".to_string()));
        assert_eq!(
            env.get("GIT_CONFIG_KEY_1"),
            Some(&"core.hooksPath".to_string())
        );
        let hooks = env["GIT_CONFIG_VALUE_1"].clone();
        assert!(
            hooks.ends_with("hooks-disabled") && !hooks.is_empty(),
            "hooksPath must point at the disabled dir, got {hooks:?}"
        );
    }

    #[test]
    fn merge_reindexes_around_explicit_git_config_count() {
        let mut env = HashMap::new();
        env.insert("GIT_CONFIG_COUNT".to_string(), "1".to_string());
        env.insert("GIT_CONFIG_KEY_0".to_string(), "user.email".to_string());
        merge_git_scrub_env(&mut env);
        // Caller's overlay is preserved at its indices; the scrub entries
        // are appended at the highest indices (later = higher precedence in
        // git) and the count is bumped.
        assert_eq!(env.get("GIT_CONFIG_COUNT"), Some(&"3".to_string()));
        assert_eq!(env.get("GIT_CONFIG_KEY_0"), Some(&"user.email".to_string()));
        assert_eq!(
            env.get("GIT_CONFIG_KEY_1"),
            Some(&"core.fsmonitor".to_string())
        );
        assert_eq!(
            env.get("GIT_CONFIG_KEY_2"),
            Some(&"core.hooksPath".to_string())
        );
        assert_eq!(env.get("GIT_CONFIG_VALUE_1"), Some(&"false".to_string()));
        assert!(env["GIT_CONFIG_VALUE_2"].ends_with("hooks-disabled"));
    }

    #[test]
    fn merge_replaces_malformed_inherited_count() {
        // A malformed count (hook noise, half-written value) must not
        // survive: git >= 2.31 hard-fails on "bogus format" and the scrub
        // would otherwise be skipped entirely.
        let mut env = HashMap::new();
        env.insert("GIT_CONFIG_COUNT".to_string(), "not-a-number".to_string());
        merge_git_scrub_env(&mut env);
        assert_eq!(env.get("GIT_CONFIG_COUNT"), Some(&"2".to_string()));
        assert_eq!(
            env.get("GIT_CONFIG_KEY_0"),
            Some(&"core.fsmonitor".to_string())
        );
        assert_eq!(
            env.get("GIT_CONFIG_KEY_1"),
            Some(&"core.hooksPath".to_string())
        );
    }

    #[test]
    fn scrub_env_shape_is_stable() {
        let env = git_scrub_env();
        assert_eq!(env.len(), 5);
        assert_eq!(env.get("GIT_CONFIG_COUNT"), Some(&"2".to_string()));
    }
}
