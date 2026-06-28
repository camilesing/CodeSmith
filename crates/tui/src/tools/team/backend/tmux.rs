//! `tmux` pane backend — spawn teammates in real tmux panes.
//!
//! When the leader runs inside a tmux session (`$TMUX` set), teammates are
//! launched via `tmux split-window` so each agent gets its own visible,
//! interactive pane. The pane runs `codesmith team-teammate …`, which
//! re-registers with the team from disk and enters the teammate loop.
//!
//! Command construction is split out as pure functions so it can be unit
//! tested without invoking `tmux`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::process::Command;

use super::{
    BackendError, BackendKind, SpawnedTeammate, TeammateBackend, TeammateHandle,
    TeammateSpawnSpec, build_team_teammate_command, write_prompt_file,
};

/// Stateless tmux pane backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct TmuxBackend;

impl TmuxBackend {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TeammateBackend for TmuxBackend {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    async fn spawn(&self, spec: &TeammateSpawnSpec) -> Result<SpawnedTeammate, BackendError> {
        // The prompt is written to a temp file so we never have to shell-quote
        // an arbitrarily long / arbitrarily nasty prompt string onto the pane
        // command line. The pane process reads and unlinks it on startup.
        let prompt_file = write_prompt_file("tmux", &spec.agent_id, &spec.prompt)?;

        let argv = build_tmux_argv(spec, &prompt_file);
        // argv[0] == "tmux".
        let output = Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .await
            .map_err(|e| BackendError::Unavailable {
                backend: "tmux",
                message: format!("failed to invoke tmux: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(BackendError::SpawnFailed {
                backend: "tmux",
                message: format!(
                    "tmux exited {}: stderr={stderr} stdout={stdout}",
                    output.status
                ),
            });
        }

        // `-P -F "#{pane_id}"` prints the new pane id to stdout.
        let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if pane_id.is_empty() {
            return Err(BackendError::SpawnFailed {
                backend: "tmux",
                message: "tmux did not return a pane id".to_string(),
            });
        }

        Ok(SpawnedTeammate {
            agent_id: spec.agent_id.clone(),
            agent_name: spec.agent_name.clone(),
            team_name: spec.team_name.clone(),
            backend: BackendKind::Tmux,
            handle: TeammateHandle::TmuxPane(pane_id),
        })
    }
}

/// Build the full `tmux split-window …` argv for `spec`, including the
/// shell-quoted `codesmith team-teammate …` command as the final element.
///
/// Exposed for unit testing the command shape without invoking tmux.
#[must_use]
pub fn build_tmux_argv(spec: &TeammateSpawnSpec, prompt_file: &Path) -> Vec<String> {
    let mut argv = vec![
        "tmux".to_string(),
        "split-window".to_string(),
        // Run the pane in the teammate's working directory.
        "-c".to_string(),
        spec.cwd.to_string_lossy().into_owned(),
    ];
    // Propagate any extra env vars into the pane (`-e KEY=VALUE`).
    for (k, v) in &spec.env {
        argv.push("-e".to_string());
        argv.push(format!("{k}={v}"));
    }
    argv.push("-P".to_string());
    argv.push("-F".to_string());
    argv.push("#{pane_id}".to_string());
    argv.push(build_team_teammate_command(spec, prompt_file));
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_spec() -> TeammateSpawnSpec {
        let mut spec = TeammateSpawnSpec::new(
            "team_agent_abcd1234".to_string(),
            "worker-1".to_string(),
            "demo".to_string(),
        );
        spec.agent_type = "team".to_string();
        spec.permission_mode = "auto".to_string();
        spec.cwd = PathBuf::from("/repo/worktrees/worker-1");
        spec.codesmith_bin = PathBuf::from("/usr/local/bin/codesmith");
        spec.model = Some("deepseek-v3".to_string());
        spec.allowed_tools = Some(vec!["read".to_string(), "bash".to_string()]);
        spec
    }

    #[test]
    fn command_includes_required_flags() {
        let spec = sample_spec();
        let prompt_file = Path::new("/tmp/prompt.txt");
        let cmd = build_team_teammate_command(&spec, prompt_file);
        assert!(cmd.starts_with("'/usr/local/bin/codesmith' team-teammate"));
        // Flags are emitted unquoted; only their values are shell-quoted.
        assert!(cmd.contains("--team 'demo'"));
        assert!(cmd.contains("--name 'worker-1'"));
        assert!(cmd.contains("--prompt-file '/tmp/prompt.txt'"));
        assert!(cmd.contains("--agent-type 'team'"));
        assert!(cmd.contains("--permission-mode 'auto'"));
        assert!(cmd.contains("--model 'deepseek-v3'"));
        assert!(cmd.contains("--cwd '/repo/worktrees/worker-1'"));
        assert!(cmd.contains("--allowed-tools 'read,bash'"));
    }

    #[test]
    fn command_omits_optional_flags_when_absent() {
        let mut spec = sample_spec();
        spec.model = None;
        spec.allowed_tools = None;
        let cmd = build_team_teammate_command(&spec, Path::new("/tmp/p.txt"));
        assert!(!cmd.contains("--model"));
        assert!(!cmd.contains("--allowed-tools"));
    }

    #[test]
    fn argv_starts_with_tmux_split_window_and_cwd() {
        let spec = sample_spec();
        let argv = build_tmux_argv(&spec, Path::new("/tmp/p.txt"));
        assert_eq!(argv[0], "tmux");
        assert_eq!(argv[1], "split-window");
        assert_eq!(argv[2], "-c");
        assert_eq!(argv[3], "/repo/worktrees/worker-1");
    }

    #[test]
    fn argv_includes_print_format_and_command() {
        let spec = sample_spec();
        let argv = build_tmux_argv(&spec, Path::new("/tmp/p.txt"));
        assert!(argv.iter().any(|a| a == "-P"));
        assert!(argv.iter().any(|a| a == "-F"));
        assert!(argv.iter().any(|a| a == "#{pane_id}"));
        // The final element is the shell command string.
        let last = argv.last().expect("non-empty argv");
        assert!(last.contains("team-teammate"));
    }

    #[test]
    fn argv_propagates_env_via_e_flag() {
        let mut spec = sample_spec();
        let mut env = HashMap::new();
        env.insert("DEEPSEEK_API_KEY".to_string(), "sk-test".to_string());
        spec.env = env;
        let argv = build_tmux_argv(&spec, Path::new("/tmp/p.txt"));
        let e_idx = argv.iter().position(|a| a == "-e").expect("-e present");
        assert_eq!(argv[e_idx + 1], "DEEPSEEK_API_KEY=sk-test");
    }

    #[test]
    fn empty_allowed_tools_omits_flag() {
        let mut spec = sample_spec();
        spec.allowed_tools = Some(vec![]);
        let cmd = build_team_teammate_command(&spec, Path::new("/tmp/p.txt"));
        assert!(!cmd.contains("--allowed-tools"));
    }
}
