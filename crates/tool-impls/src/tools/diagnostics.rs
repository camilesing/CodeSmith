//! Workspace diagnostics tool: `diagnostics`.
//!
//! This tool gathers lightweight, best-effort environment information without
//! failing hard when optional commands are unavailable.

use std::path::Path;
use std::process::Command;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

/// Tool for collecting workspace and toolchain diagnostics.
pub struct DiagnosticsTool;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiagnosticsOutput {
    workspace_root: String,
    current_dir: Option<String>,
    current_dir_error: Option<String>,
    git_repo: bool,
    git_branch: Option<String>,
    git_error: Option<String>,
    sandbox_available: bool,
    sandbox_type: Option<String>,
    bwrap_available: bool,
    cgroup_version: Option<u8>,
    rustc_version: Option<String>,
    cargo_version: Option<String>,
    /// User-trusted external paths the agent may access from this workspace
    /// (`/trust add <path>` from the slash command, persisted in
    /// `~/.deepseek/workspace-trust.json`). See issue #29.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    trusted_external_paths: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct GitProbe {
    detected: bool,
    branch: Option<String>,
    error: Option<String>,
}

#[async_trait]
impl ToolSpec for DiagnosticsTool {
    fn name(&self) -> &'static str {
        "diagnostics"
    }

    fn description(&self) -> &'static str {
        "Report workspace info, git detection, sandbox availability, and Rust toolchain versions."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, _input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let workspace_root = context.workspace.display().to_string();

        // The session's effective cwd (the per-turn ToolContext binding that
        // follows cd captures / /cd / worktree), not the process launch
        // directory — the model asked where the session is, not where the
        // binary was started.
        let (current_dir, current_dir_error) = (Some(context.cwd.display().to_string()), None);

        let git = probe_git(&context.workspace).await;
        let sandbox_type =
            codesmith_agent_runtime::sandbox::get_platform_sandbox().map(|s| s.to_string());
        let sandbox_available = sandbox_type.is_some();

        // Bubblewrap availability (#2184).
        let bwrap_available = probe_bwrap_available();

        // Cgroup version (Linux only).
        let cgroup_version = probe_cgroup_version();

        let trusted_external_paths = context
            .trusted_external_paths_snapshot()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let diagnostics = DiagnosticsOutput {
            workspace_root,
            current_dir,
            current_dir_error,
            git_repo: git.detected,
            git_branch: git.branch,
            git_error: git.error,
            sandbox_available,
            sandbox_type,
            bwrap_available,
            cgroup_version,
            rustc_version: probe_version("rustc", &["--version"], &context.workspace).await,
            cargo_version: probe_version("cargo", &["--version"], &context.workspace).await,
            trusted_external_paths,
        };

        ToolResult::json(&diagnostics).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

// === Helpers ===

async fn probe_git(workspace: &Path) -> GitProbe {
    let rev_parse = run_command("git", &["rev-parse", "--is-inside-work-tree"], workspace).await;
    match rev_parse {
        CommandProbe::Success(out) => {
            if out.trim() != "true" {
                return GitProbe {
                    detected: false,
                    branch: None,
                    error: Some(format!("unexpected git rev-parse output: {out}")),
                };
            }
            // `--abbrev-ref HEAD` prints the literal `HEAD` on a detached
            // HEAD — not a branch name. A failed lookup keeps its stderr so
            // `git_repo: true, git_branch: null` is never silently
            // indistinguishable from a real failure.
            match run_command("git", &["rev-parse", "--abbrev-ref", "HEAD"], workspace).await {
                CommandProbe::Success(name) if name == "HEAD" => GitProbe {
                    detected: true,
                    branch: None,
                    error: None,
                },
                CommandProbe::Success(name) => GitProbe {
                    detected: true,
                    branch: Some(name),
                    error: None,
                },
                CommandProbe::Failed { stderr } => GitProbe {
                    detected: true,
                    branch: None,
                    error: stderr,
                },
                CommandProbe::Missing => GitProbe {
                    detected: true,
                    branch: None,
                    error: Some("git is not installed or not in PATH".to_string()),
                },
            }
        }
        CommandProbe::Failed { stderr, .. } => GitProbe {
            detected: false,
            branch: None,
            error: stderr,
        },
        CommandProbe::Missing => GitProbe {
            detected: false,
            branch: None,
            error: Some("git is not installed or not in PATH".to_string()),
        },
    }
}

fn probe_bwrap_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        codesmith_agent_runtime::sandbox::bwrap::is_available()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn probe_cgroup_version() -> Option<u8> {
    #[cfg(target_os = "linux")]
    {
        let path = std::path::Path::new("/sys/fs/cgroup/cgroup.controllers");
        if path.exists() {
            return Some(2);
        }
        let path = std::path::Path::new("/sys/fs/cgroup");
        if path.exists() {
            return Some(1);
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

async fn probe_version(program: &str, args: &[&str], cwd: &Path) -> Option<String> {
    run_command(program, args, cwd).await.into_success()
}

enum CommandProbe {
    Success(String),
    Failed { stderr: Option<String> },
    Missing,
}

impl CommandProbe {
    fn into_success(self) -> Option<String> {
        match self {
            CommandProbe::Success(out) => Some(out),
            CommandProbe::Failed { .. } | CommandProbe::Missing => None,
        }
    }
}

/// Runs a probe subprocess on the blocking pool — `std::process::Command`
/// blocks the calling thread for the child's full lifetime.
async fn run_command(program: &str, args: &[&str], cwd: &Path) -> CommandProbe {
    let program = program.to_string();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || run_command_blocking(&program, &args, &cwd))
        .await
        .unwrap_or_else(|err| CommandProbe::Failed {
            stderr: Some(format!("probe task failed: {err}")),
        })
}

fn run_command_blocking(program: &str, args: &[String], cwd: &Path) -> CommandProbe {
    let output = Command::new(program).args(args).current_dir(cwd).output();
    let output = match output {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return CommandProbe::Missing,
        // Keep the spawn error (EACCES, …): a silent `stderr: None` made
        // "git refused to run" indistinguishable from "not a git repo".
        Err(err) => {
            return CommandProbe::Failed {
                stderr: Some(format!("failed to spawn {program}: {err}")),
            };
        }
    };

    if output.status.success() {
        CommandProbe::Success(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        CommandProbe::Failed {
            stderr: if stderr.is_empty() {
                None
            } else {
                Some(stderr)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent_runtime::dependencies::ExternalTool;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn git_available() -> bool {
        codesmith_agent_runtime::dependencies::Git::available()
    }

    fn init_git_repo(root: &Path) {
        let run = |args: &[&str]| {
            let status = codesmith_agent_runtime::dependencies::Git::status(args, root)
                .expect("git should spawn");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
        fs::write(root.join("README.md"), "init\n").expect("write");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
    }

    #[test]
    fn diagnostics_schema_has_empty_required_array() {
        let schema = DiagnosticsTool.input_schema();
        assert_eq!(schema["properties"], json!({}));
        assert_eq!(schema["required"], json!([]));
    }

    #[tokio::test]
    async fn diagnostics_runs_best_effort_outside_git_repo() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());
        let tool = DiagnosticsTool;
        let result = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(result.success);

        let parsed: DiagnosticsOutput =
            serde_json::from_str(&result.content).expect("tool result should be json");
        assert_eq!(parsed.workspace_root, tmp.path().display().to_string());
    }

    #[tokio::test]
    async fn diagnostics_detects_git_repo_when_available() {
        if !git_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        init_git_repo(tmp.path());

        let ctx = ToolContext::new(tmp.path());
        let tool = DiagnosticsTool;
        let result = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(result.success);

        let parsed: DiagnosticsOutput =
            serde_json::from_str(&result.content).expect("tool result should be json");
        assert!(parsed.git_repo);
        assert!(!parsed.git_branch.as_deref().unwrap_or("").is_empty());
    }
}
