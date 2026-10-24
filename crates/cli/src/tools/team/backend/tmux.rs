//! `tmux` pane backend — spawn teammates in real tmux panes.
//!
//! When the leader runs inside a tmux session (`$TMUX` set), teammates are
//! launched via `tmux split-window` so each agent gets its own visible,
//! interactive pane. The pane runs `codesmith team-teammate …`, which
//! re-registers with the team from disk and enters the teammate loop.
//!
//! Command construction is split out as pure functions so it can be unit
//! tested without invoking `tmux`.

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use async_trait::async_trait;
use tokio::process::Command;

use super::{
    BackendError, BackendKind, SpawnedTeammate, TeammateBackend, TeammateHandle, TeammateSpawnSpec,
    build_team_teammate_command, write_env_file, write_prompt_file,
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
        spec.validate()?;
        // The prompt is written to a temp file so we never have to shell-quote
        // an arbitrarily long / arbitrarily nasty prompt string onto the pane
        // command line. The pane process reads and unlinks it on startup.
        // Env vars get the same treatment: `-e KEY=VALUE` would put API keys
        // on the tmux client's argv (visible to every process via `ps`).
        // Both writes are sync, so they are offloaded off the executor.
        let agent_id = spec.agent_id.clone();
        let prompt = spec.prompt.clone();
        let agent_env = spec.env.clone();
        let (prompt_file, env_file) = tokio::task::spawn_blocking(move || {
            let prompt_file = write_prompt_file("tmux", &agent_id, &prompt)?;
            let env_file = write_env_file("tmux", &agent_id, &agent_env)?;
            Ok::<_, BackendError>((prompt_file, env_file))
        })
        .await
        .map_err(|e| BackendError::SpawnFailed {
            backend: "tmux",
            message: format!("prompt-file task failed: {e}"),
        })??;

        let argv = build_tmux_argv(spec, &prompt_file, env_file.as_deref());
        // argv[0] == "tmux".
        let output = match Command::new(&argv[0]).args(&argv[1..]).output().await {
            Ok(output) => output,
            Err(e) => {
                // tmux never ran, so nothing will unlink the prompt/env
                // files — remove them here rather than leaking content.
                let _ = tokio::fs::remove_file(&prompt_file).await;
                if let Some(env_file) = &env_file {
                    let _ = tokio::fs::remove_file(env_file).await;
                }
                return Err(BackendError::Unavailable {
                    backend: "tmux",
                    message: format!("failed to invoke tmux: {e}"),
                });
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            // The pane never started, so nothing will unlink the prompt/env
            // files — remove them here rather than leaking content.
            let _ = tokio::fs::remove_file(&prompt_file).await;
            if let Some(env_file) = &env_file {
                let _ = tokio::fs::remove_file(env_file).await;
            }
            return Err(BackendError::SpawnFailed {
                backend: "tmux",
                message: format!(
                    "tmux exited {}: stderr={stderr} stdout={stdout}",
                    output.status
                ),
            });
        }

        // `-P -F "#{pane_id}"` prints the new pane id to stdout.
        let pane_id = match parse_tmux_pane_id(&output.stdout) {
            Ok(id) => id,
            Err(err) => {
                // tmux exited 0, so the pane exists and the teammate process
                // is running in it. Kill it best-effort (any `%`-prefixed
                // line may be the pane id) so a parse failure does not leak
                // a live pane that a caller retry would duplicate.
                if let Some(candidate) = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .find(|line| line.starts_with('%'))
                    .map(str::to_owned)
                {
                    let _ = Command::new("tmux")
                        .args(["kill-pane", "-t", &candidate])
                        .output()
                        .await;
                }
                return Err(err);
            }
        };

        Ok(SpawnedTeammate {
            agent_id: spec.agent_id.clone(),
            agent_name: spec.agent_name.clone(),
            team_name: spec.team_name.clone(),
            backend: BackendKind::Tmux,
            handle: TeammateHandle::TmuxPane(pane_id),
        })
    }
}

/// tmux prints exactly one `%N` line. Any extra diagnostic or multi-line
/// output baked into the handle would make every later pane operation
/// (`kill-pane`, `send-keys`) fail with a confusing error, so the shape is
/// validated before it is accepted.
fn parse_tmux_pane_id(stdout: &[u8]) -> Result<String, BackendError> {
    let pane_id = String::from_utf8_lossy(stdout).trim().to_string();
    if !pane_id.is_empty() && pane_id.lines().count() == 1 && pane_id.starts_with('%') {
        Ok(pane_id)
    } else {
        Err(BackendError::SpawnFailed {
            backend: "tmux",
            message: format!("tmux returned an unexpected pane id: {pane_id:?}"),
        })
    }
}

/// Build the full `tmux split-window …` argv for `spec`, including the
/// shell-quoted `codesmith team-teammate …` command as the final element.
///
/// Env vars are **not** passed as `-e KEY=VALUE` — tmux client argv is
/// world-visible (`ps`), and the propagated set includes API keys. They ride
/// the 0600 `env_file` instead, sourced by the pane command wrapper.
///
/// Exposed for unit testing the command shape without invoking tmux.
#[must_use]
pub fn build_tmux_argv(
    spec: &TeammateSpawnSpec,
    prompt_file: &Path,
    env_file: Option<&Path>,
) -> Vec<String> {
    let mut argv = vec![
        "tmux".to_string(),
        "split-window".to_string(),
        // Create the pane without selecting it, so spawning a teammate does
        // not steal keyboard focus from the leader's pane (panes stay
        // visible and interactive).
        "-d".to_string(),
        // Run the pane in the teammate's working directory.
        "-c".to_string(),
        spec.cwd.to_string_lossy().into_owned(),
    ];
    argv.push("-P".to_string());
    argv.push("-F".to_string());
    argv.push("#{pane_id}".to_string());
    argv.push(build_team_teammate_command(spec, prompt_file, env_file));
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> TeammateSpawnSpec {
        let mut spec = TeammateSpawnSpec::new(
            "team_agent_abcd1234".to_string(),
            "worker-1".to_string(),
            "demo".to_string(),
        );
        spec.agent_type = "team".to_string();
        spec.permission_mode = super::super::PermissionMode::Auto;
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
        let cmd = build_team_teammate_command(&spec, prompt_file, None);
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
        let cmd = build_team_teammate_command(&spec, Path::new("/tmp/p.txt"), None);
        assert!(!cmd.contains("--model"));
        assert!(!cmd.contains("--allowed-tools"));
    }

    #[test]
    fn argv_starts_with_tmux_split_window_detached_and_cwd() {
        let spec = sample_spec();
        let argv = build_tmux_argv(&spec, Path::new("/tmp/p.txt"), None);
        assert_eq!(argv[0], "tmux");
        assert_eq!(argv[1], "split-window");
        // `-d` keeps the new pane from stealing focus from the leader.
        assert_eq!(argv[2], "-d");
        assert_eq!(argv[3], "-c");
        assert_eq!(argv[4], "/repo/worktrees/worker-1");
    }

    #[test]
    fn argv_never_carries_env_values() {
        // API keys ride the 0600 env file (sourced by the pane wrapper), not
        // `-e KEY=VALUE` on the world-visible tmux client argv.
        let mut spec = sample_spec();
        spec.env = std::collections::HashMap::from([
            ("DEEPSEEK_API_KEY".to_string(), "sk-test".to_string()),
            ("A=B".to_string(), "splitter".to_string()),
            ("".to_string(), "empty".to_string()),
            ("\0key".to_string(), "nul".to_string()),
        ]);
        let argv = build_tmux_argv(&spec, Path::new("/tmp/p.txt"), None);
        assert!(
            !argv.iter().any(|a| a == "-e" || a.contains("sk-test")),
            "env values leaked onto argv: {argv:?}"
        );
    }

    #[test]
    fn env_file_wrapper_sources_and_unlinks() {
        let spec = sample_spec();
        let cmd = build_team_teammate_command(
            &spec,
            Path::new("/tmp/p.txt"),
            Some(Path::new("/tmp/team.env")),
        );
        assert!(
            cmd.starts_with("set -a; . '/tmp/team.env'; set +a; rm -f '/tmp/team.env'; "),
            "env wrapper missing: {cmd}"
        );
        assert!(cmd.contains("team-teammate"));
    }

    #[test]
    fn env_file_skips_non_identifier_keys_and_multiline_values() {
        let env = std::collections::HashMap::from([
            ("GOOD_KEY".to_string(), "ok".to_string()),
            ("1BAD".to_string(), "x".to_string()),
            ("BAD=KEY".to_string(), "x".to_string()),
            ("MULTI".to_string(), "a\nb".to_string()),
        ]);
        let path = write_env_file("tmux", "agent_x", &env)
            .expect("write ok")
            .expect("Some");
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        assert!(
            body.contains("GOOD_KEY='ok'"),
            "valid entry missing: {body}"
        );
        assert!(
            !body.contains("1BAD") && !body.contains("BAD=KEY") && !body.contains("MULTI"),
            "invalid entries written: {body}"
        );
    }

    #[test]
    fn parse_pane_id_accepts_single_percent_line_only() {
        assert_eq!(parse_tmux_pane_id(b"%5\n").expect("valid"), "%5");
        assert!(parse_tmux_pane_id(b"").is_err());
        assert!(parse_tmux_pane_id(b"5").is_err());
        assert!(parse_tmux_pane_id(b"%5\nextra\n").is_err());
    }

    #[test]
    fn argv_includes_print_format_and_command() {
        let spec = sample_spec();
        let argv = build_tmux_argv(&spec, Path::new("/tmp/p.txt"), None);
        assert!(argv.iter().any(|a| a == "-P"));
        assert!(argv.iter().any(|a| a == "-F"));
        assert!(argv.iter().any(|a| a == "#{pane_id}"));
        // The final element is the shell command string.
        let last = argv.last().expect("non-empty argv");
        assert!(last.contains("team-teammate"));
    }

    #[test]
    fn empty_allowed_tools_omits_flag() {
        let mut spec = sample_spec();
        spec.allowed_tools = Some(vec![]);
        let cmd = build_team_teammate_command(&spec, Path::new("/tmp/p.txt"), None);
        assert!(!cmd.contains("--allowed-tools"));
    }
}
