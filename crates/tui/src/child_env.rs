//! Sanitized environment handling for child processes.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};

/// Convert a string env map into owned OS strings for child env helpers.
pub fn string_map_env(
    env: &HashMap<String, String>,
) -> impl Iterator<Item = (OsString, OsString)> + '_ {
    env.iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
}

/// Return the environment for a child process after dropping parent secrets.
///
/// `overrides` are trusted call-site values, such as sandbox markers, hook
/// variables, MCP server config, or RLM context path. They are applied after the
/// parent allowlist so explicit values win.
pub fn sanitized_child_env<I, K, V>(overrides: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut env = Vec::new();
    for (key, value) in std::env::vars_os() {
        if is_allowed_parent_env_key(&key) {
            upsert_env(&mut env, key, value);
        }
    }
    for (key, value) in overrides {
        upsert_env(
            &mut env,
            key.as_ref().to_os_string(),
            value.as_ref().to_os_string(),
        );
    }
    env
}

pub fn apply_to_command<I, K, V>(cmd: &mut std::process::Command, overrides: I)
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    cmd.env_clear();
    for (key, value) in sanitized_child_env(overrides) {
        cmd.env(key, value);
    }
}

pub fn apply_to_tokio_command<I, K, V>(cmd: &mut tokio::process::Command, overrides: I)
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    cmd.env_clear();
    for (key, value) in sanitized_child_env(overrides) {
        cmd.env(key, value);
    }
}

pub fn apply_to_pty_command<I, K, V>(cmd: &mut portable_pty::CommandBuilder, overrides: I)
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    cmd.env_clear();
    for (key, value) in sanitized_child_env(overrides) {
        cmd.env(key, value);
    }
}

fn is_allowed_parent_env_key(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    let normalized = key.to_ascii_uppercase();
    matches!(
        normalized.as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "USERNAME"
            | "LOGNAME"
            | "LANG"
            | "LANGUAGE"
            | "LC_ALL"
            | "LC_CTYPE"
            | "LC_MESSAGES"
            | "TERM"
            | "COLORTERM"
            | "NO_COLOR"
            | "FORCE_COLOR"
            | "SHELL"
            | "TMPDIR"
            | "TMP"
            | "TEMP"
            | "__CF_USER_TEXT_ENCODING"
            | "SYSTEMROOT"
            | "WINDIR"
            | "COMSPEC"
            | "PATHEXT"
            | "USERPROFILE"
            | "HOMEDRIVE"
            | "HOMEPATH"
    ) || normalized.starts_with("LC_")
}

fn upsert_env(env: &mut Vec<(OsString, OsString)>, key: OsString, value: OsString) {
    let normalized = normalize_key(&key);
    env.retain(|(existing, _)| normalize_key(existing) != normalized);
    env.push((key, value));
}

fn normalize_key(key: &OsStr) -> String {
    key.to_string_lossy().to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn sanitized_child_env_drops_parent_secret_like_values() {
        let _guard = env_lock().lock().expect("env lock");
        let previous = std::env::var_os("DEEPSEEK_CHILD_ENV_TEST_SECRET");
        unsafe {
            std::env::set_var("DEEPSEEK_CHILD_ENV_TEST_SECRET", "parent-secret");
        }

        let env = sanitized_child_env(std::iter::empty::<(OsString, OsString)>());

        match previous {
            Some(value) => unsafe {
                std::env::set_var("DEEPSEEK_CHILD_ENV_TEST_SECRET", value);
            },
            None => unsafe {
                std::env::remove_var("DEEPSEEK_CHILD_ENV_TEST_SECRET");
            },
        }

        assert!(
            env.iter()
                .all(|(key, _)| key != "DEEPSEEK_CHILD_ENV_TEST_SECRET")
        );
    }

    #[test]
    fn explicit_child_env_values_win_over_parent_allowlist() {
        let _guard = env_lock().lock().expect("env lock");
        let previous = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", "/parent/bin");
        }

        let env = sanitized_child_env([(OsString::from("PATH"), OsString::from("/explicit/bin"))]);

        match previous {
            Some(value) => unsafe {
                std::env::set_var("PATH", value);
            },
            None => unsafe {
                std::env::remove_var("PATH");
            },
        }

        let path = env
            .iter()
            .find(|(key, _)| normalize_key(key) == "PATH")
            .map(|(_, value)| value);
        assert_eq!(path, Some(&OsString::from("/explicit/bin")));
    }
}
