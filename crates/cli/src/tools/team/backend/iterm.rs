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
    build_team_teammate_command, shell_quote, write_env_file, write_prompt_file,
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
        // Sync temp-file writes, offloaded off the async executor. The env
        // file keeps API keys off the AppleScript text (visible in the
        // terminal and on osascript's argv).
        let agent_id = spec.agent_id.clone();
        let prompt = spec.prompt.clone();
        let agent_env = spec.env.clone();
        let (prompt_file, env_file) =
            tokio::task::spawn_blocking(move || {
                let prompt_file = write_prompt_file("iterm", &agent_id, &prompt)?;
                let env_file = write_env_file("iterm", &agent_id, &agent_env)?;
                Ok::<_, BackendError>((prompt_file, env_file))
            })
            .await
            .map_err(|e| BackendError::SpawnFailed {
                backend: "iterm",
                message: format!("prompt-file task failed: {e}"),
            })??;
        let script = build_applescript(spec, &prompt_file, env_file.as_deref());

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
                    // osascript never ran, so nothing will unlink the
                    // prompt/env files — remove them here rather than
                    // leaking content.
                    let _ = tokio::fs::remove_file(&prompt_file).await;
                    if let Some(env_file) = &env_file {
                        let _ = tokio::fs::remove_file(env_file).await;
                    }
                    return Err(BackendError::Unavailable {
                        backend: "iterm",
                        message: format!("failed to invoke osascript: {e}"),
                    });
                }
            },
            Err(_) => {
                let _ = tokio::fs::remove_file(&prompt_file).await;
                if let Some(env_file) = &env_file {
                    let _ = tokio::fs::remove_file(env_file).await;
                }
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
            // The pane never started, so nothing will unlink the
            // prompt/env files — remove them here rather than leaking
            // content.
            let _ = tokio::fs::remove_file(&prompt_file).await;
            if let Some(env_file) = &env_file {
                let _ = tokio::fs::remove_file(env_file).await;
            }
            return Err(BackendError::SpawnFailed {
                backend: "iterm",
                message: format!("osascript exited {}: {stderr}", output.status),
            });
        }

        let session_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if session_id.is_empty() {
            let _ = tokio::fs::remove_file(&prompt_file).await;
            if let Some(env_file) = &env_file {
                let _ = tokio::fs::remove_file(env_file).await;
            }
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
///
/// Env vars are propagated via the 0600 `env_file` (sourced by the typed
/// command's wrapper), not as `KEY='value'` prefix words: the typed command
/// is echoed to the terminal and the whole AppleScript rides `osascript`'s
/// argv, both readable by anyone looking.
#[must_use]
pub fn build_applescript(
    spec: &TeammateSpawnSpec,
    prompt_file: &Path,
    env_file: Option<&Path>,
) -> String {
    // The shell command typed into the new tab: cd into the teammate cwd,
    // then run the codesmith team-teammate subcommand. `write text` sends
    // this to the interactive shell, so the user can watch / interrupt it.
    let shell_cmd = format!(
        "cd {} && {}",
        shell_quote(&spec.cwd.to_string_lossy()),
        build_team_teammate_command(spec, prompt_file, env_file)
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
         \t\t\tdelay 0.05\n\
         \tend repeat\n\
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
        let script = build_applescript(&spec, Path::new("/tmp/p.txt"), None);
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
    fn script_never_embeds_env_values() {
        // Env values ride the 0600 env file (passed at spawn), never the
        // typed/echoed AppleScript text.
        let mut spec = sample_spec();
        spec.env = std::collections::HashMap::from([
            ("DEEPSEEK_API_KEY".to_string(), "sk-test x".to_string()),
            ("9BAD".to_string(), "leading digit".to_string()),
        ]);
        let script = build_applescript(&spec, Path::new("/tmp/p.txt"), None);
        assert!(
            !script.contains("sk-test") && !script.contains("DEEPSEEK_API_KEY"),
            "env leaked into script: {script}"
        );
        assert!(script.contains("cd '/repo' && 'codesmith' team-teammate"));
    }

    #[test]
    fn script_sources_env_file_when_provided() {
        let spec = sample_spec();
        let script = build_applescript(&spec, Path::new("/tmp/p.txt"), Some(Path::new("/tmp/t.env")));
        assert!(
            script.contains("set -a; . '/tmp/t.env'; set +a; rm -f '/tmp/t.env'"),
            "env wrapper missing: {script}"
        );
    }

    #[test]
    fn script_embeds_cd_and_team_teammate_command() {
        let spec = sample_spec();
        let script = build_applescript(&spec, Path::new("/tmp/p.txt"), None);
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
        let script = build_applescript(&spec, Path::new("/tmp/p.txt"), None);
        assert!(script.contains("\\\"weird\\\""));
        assert!(!script.contains("text \"/repo/\"weird\""));
    }
}
