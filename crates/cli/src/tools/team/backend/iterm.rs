//! iTerm2 tab backend — spawn teammates in real iTerm2 tabs.
//!
//! When the leader runs inside iTerm2 (`$TERM_PROGRAM == iTerm.app`),
//! teammates are launched via AppleScript (`osascript`) that opens a new
//! tab in the current window and types the `codesmith team-teammate …`
//! command into it. The new tab's session id is captured and returned as
//! the teammate handle.
//!
//! AppleScript construction is split out as a pure function so it can be
//! unit tested without driving iTerm2.

use std::path::Path;

use async_trait::async_trait;
use tokio::process::Command;

use super::{
    BackendError, BackendKind, SpawnedTeammate, TeammateBackend, TeammateHandle, TeammateSpawnSpec,
    build_team_teammate_command, shell_quote, write_prompt_file,
};

/// Stateless iTerm2 tab backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct ITermBackend;

impl ITermBackend {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TeammateBackend for ITermBackend {
    fn name(&self) -> &'static str {
        "iterm"
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Iter
    }

    async fn spawn(&self, spec: &TeammateSpawnSpec) -> Result<SpawnedTeammate, BackendError> {
        spec.validate()?;
        // Sync temp-file write, offloaded off the async executor.
        let agent_id = spec.agent_id.clone();
        let prompt = spec.prompt.clone();
        let prompt_file =
            tokio::task::spawn_blocking(move || write_prompt_file("iterm", &agent_id, &prompt))
                .await
                .map_err(|e| BackendError::SpawnFailed {
                    backend: "iterm",
                    message: format!("prompt-file task failed: {e}"),
                })??;
        let script = build_applescript(spec, &prompt_file);

        // Bound the osascript call: a first-run Automation-permission dialog
        // (or a hung iTerm2) used to block indefinitely with no cancellation
        // path, stalling the leader's team-formation flow. kill_on_drop
        // reaps the abandoned child when the timeout drops the future.
        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .kill_on_drop(true)
                .output(),
        )
        .await
        {
            Ok(result) => match result {
                Ok(output) => output,
                Err(e) => {
                    // osascript never ran, so nothing will unlink the prompt
                    // file — remove it here rather than leaking content.
                    let _ = tokio::fs::remove_file(&prompt_file).await;
                    return Err(BackendError::Unavailable {
                        backend: "iterm",
                        message: format!("failed to invoke osascript: {e}"),
                    });
                }
            },
            Err(_) => {
                let _ = tokio::fs::remove_file(&prompt_file).await;
                return Err(BackendError::SpawnFailed {
                    backend: "iterm",
                    message: "osascript timed out after 30s (Automation permission prompt \
                              or iTerm2 unresponsive?)"
                        .to_string(),
                });
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // The pane never started, so nothing will unlink the prompt
            // file — remove it here rather than leaking prompt content.
            let _ = tokio::fs::remove_file(&prompt_file).await;
            return Err(BackendError::SpawnFailed {
                backend: "iterm",
                message: format!("osascript exited {}: {stderr}", output.status),
            });
        }

        let session_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if session_id.is_empty() {
            let _ = tokio::fs::remove_file(&prompt_file).await;
            return Err(BackendError::SpawnFailed {
                backend: "iterm",
                message: "osascript did not return a session id".to_string(),
            });
        }

        Ok(SpawnedTeammate {
            agent_id: spec.agent_id.clone(),
            agent_name: spec.agent_name.clone(),
            team_name: spec.team_name.clone(),
            backend: BackendKind::Iter,
            handle: TeammateHandle::ITermSession(session_id),
        })
    }
}

/// Build the AppleScript that opens a new iTerm2 tab (or window when none
/// exists), types the teammate command into it, and returns the new
/// session's id.
#[must_use]
pub fn build_applescript(spec: &TeammateSpawnSpec, prompt_file: &Path) -> String {
    // Propagate spec.env the way the tmux backend does (`split-window -e`):
    // the new tab's login shell otherwise inherits only the user's login
    // environment, not the leader's, so env-var configuration (e.g. proxy
    // or API-key settings) would silently never reach iTerm teammates.
    // Keys must be plain shell identifiers — quoting a key would produce an
    // invalid assignment word — so anything else is skipped.
    let mut env_prefix = String::new();
    for (k, v) in &spec.env {
        if !is_shell_identifier(k) {
            continue;
        }
        env_prefix.push_str(k);
        env_prefix.push('=');
        env_prefix.push_str(&shell_quote(v));
        env_prefix.push(' ');
    }
    // The shell command typed into the new tab: cd into the teammate cwd,
    // then run the codesmith team-teammate subcommand. `write text` sends
    // this to the interactive shell, so the user can watch / interrupt it.
    let shell_cmd = format!(
        "cd {} && {}{}",
        shell_quote(&spec.cwd.to_string_lossy()),
        env_prefix,
        build_team_teammate_command(spec, prompt_file)
    );
    let escaped = applescript_escape(&shell_cmd);
    format!(
        "tell application \"iTerm\"\n\
         \tactivate\n\
         \tif (count of windows) is 0 then\n\
         \t\tset theWindow to (create window with default profile)\n\
         \t\tset theSession to current session of theWindow\n\
         \telse\n\
         \t\tset theWindow to current window\n\
         \t\tset theSession to current session of (create tab with default profile)\n\
         \tend if\n\
         \trepeat until (length of (contents of theSession)) > 0\n\
         \t\tdelay 0.05\n\
         \tend repeat\n\
         \twrite theSession text \"{escaped}\"\n\
         \treturn id of theSession\n\
         end tell"
    )
}

/// A conservative shell-identifier check (`[A-Za-z_][A-Za-z0-9_]*`), used
/// to decide which env keys can safely become `KEY=value` assignment words
/// in the typed command.
fn is_shell_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Escape a string for safe embedding inside an AppleScript double-quoted
/// string literal: `\` → `\\` and `"` → `\"`.
fn applescript_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_spec() -> TeammateSpawnSpec {
        let mut spec = TeammateSpawnSpec::new(
            "team_agent_abcd1234".to_string(),
            "worker-1".to_string(),
            "demo".to_string(),
        );
        spec.agent_type = "team".to_string();
        spec.permission_mode = super::super::PermissionMode::Ask;
        spec.cwd = PathBuf::from("/repo");
        spec.codesmith_bin = PathBuf::from("codesmith");
        spec.model = Some("deepseek-v3".to_string());
        spec
    }

    #[test]
    fn applescript_escape_quotes_and_backslashes() {
        assert_eq!(applescript_escape("he said \"hi\""), "he said \\\"hi\\\"");
        assert_eq!(applescript_escape("a\\b"), "a\\\\b");
        assert_eq!(applescript_escape("plain"), "plain");
    }

    #[test]
    fn script_creates_tab_when_window_exists() {
        let spec = sample_spec();
        let script = build_applescript(&spec, Path::new("/tmp/p.txt"));
        assert!(script.contains("tell application \"iTerm\""));
        assert!(script.contains("if (count of windows) is 0 then"));
        assert!(script.contains("create window with default profile"));
        assert!(script.contains("create tab with default profile"));
        // The session must be ready before the command is typed: input
        // buffered while the login shell is still starting can be dropped.
        assert!(script.contains("repeat until (length of (contents of theSession)) > 0"));
        assert!(script.contains("write theSession text"));
        assert!(script.contains("return id of theSession"));
    }

    #[test]
    fn script_prefixes_env_assignments_for_valid_identifiers_only() {
        let mut spec = sample_spec();
        spec.env = std::collections::HashMap::from([
            ("DEEPSEEK_API_KEY".to_string(), "sk-test x".to_string()),
            ("9BAD".to_string(), "leading digit".to_string()),
            ("A=B".to_string(), "splitter".to_string()),
            ("with space".to_string(), "space".to_string()),
        ]);
        let script = build_applescript(&spec, Path::new("/tmp/p.txt"));
        assert!(
            script.contains("cd '/repo' && DEEPSEEK_API_KEY='sk-test x' "),
            "script = {script}"
        );
        assert!(!script.contains("9BAD"));
        assert!(!script.contains("A=B"));
        assert!(!script.contains("with space"));
    }

    #[test]
    fn script_embeds_cd_and_team_teammate_command() {
        let spec = sample_spec();
        let script = build_applescript(&spec, Path::new("/tmp/p.txt"));
        // The embedded shell command cd's into the cwd then runs the subcommand.
        assert!(script.contains("cd '/repo'"));
        assert!(script.contains("codesmith' team-teammate"));
        assert!(script.contains("--team"));
        assert!(script.contains("'demo'"));
        assert!(script.contains("--prompt-file"));
        assert!(script.contains("/tmp/p.txt"));
        assert!(script.contains("--model"));
        assert!(script.contains("'deepseek-v3'"));
    }

    #[test]
    fn script_escapes_embedded_quotes() {
        // A cwd with a double-quote would break the AppleScript string; it
        // must be escaped.
        let mut spec = sample_spec();
        spec.cwd = PathBuf::from("/repo/\"weird\"");
        let script = build_applescript(&spec, Path::new("/tmp/p.txt"));
        assert!(script.contains("\\\"weird\\\""));
        assert!(!script.contains("text \"/repo/\"weird\""));
    }
}
