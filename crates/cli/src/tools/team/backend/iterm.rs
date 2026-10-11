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
        let prompt_file = write_prompt_file("iterm", &spec.agent_id, &spec.prompt)?;
        let script = build_applescript(spec, &prompt_file);

        let output = Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .await
            .map_err(|e| BackendError::Unavailable {
                backend: "iterm",
                message: format!("failed to invoke osascript: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BackendError::SpawnFailed {
                backend: "iterm",
                message: format!("osascript exited {}: {stderr}", output.status),
            });
        }

        let session_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if session_id.is_empty() {
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
    // The shell command typed into the new tab: cd into the teammate cwd,
    // then run the codesmith team-teammate subcommand. `write text` sends
    // this to the interactive shell, so the user can watch / interrupt it.
    let shell_cmd = format!(
        "cd {} && {}",
        shell_quote(&spec.cwd.to_string_lossy()),
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
         \twrite theSession text \"{escaped}\"\n\
         \treturn id of theSession\n\
         end tell"
    )
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
        spec.permission_mode = "ask".to_string();
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
        assert!(script.contains("write theSession text"));
        assert!(script.contains("return id of theSession"));
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
