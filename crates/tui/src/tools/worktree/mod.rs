//! Worktree isolation tools and utilities.
//!
//! Provides `enter_worktree` and `exit_worktree` tools for creating isolated
//! git worktrees during a session, plus shared session state and git helpers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::dependencies::ExternalTool;
use codesmith_tools::ToolError;

mod enter;
mod exit;

pub use enter::EnterWorktreeTool;
pub use exit::ExitWorktreeTool;
pub use codesmith_agent_runtime::tool_state::worktree::*;

// ── Shared session state ──────────────────────────────────────────────────


// ── Slug validation ───────────────────────────────────────────────────────

const VALID_SLUG_SEGMENT: &str = "[a-zA-Z0-9._-]";
const MAX_SLUG_LENGTH: usize = 64;

/// Validate a worktree slug. Prevents path traversal and enforces
/// the character allowlist per `/`-separated segment, max 64 chars total.
/// Forward slashes are allowed for nesting but each segment is validated
/// independently against `[a-zA-Z0-9._-]`.
pub fn validate_worktree_slug(slug: &str) -> Result<(), ToolError> {
    if slug.len() > MAX_SLUG_LENGTH {
        return Err(ToolError::execution_failed(format!(
            "Invalid worktree name: must be {MAX_SLUG_LENGTH} characters or fewer (got {})",
            slug.len()
        )));
    }
    for segment in slug.split('/') {
        if segment == "." || segment == ".." {
            return Err(ToolError::execution_failed(format!(
                "Invalid worktree name \"{slug}\": must not contain \".\" or \"..\" path segments"
            )));
        }
        if segment.is_empty() {
            return Err(ToolError::execution_failed(format!(
                "Invalid worktree name \"{slug}\": empty segment"
            )));
        }
        // Regex check: each segment must match [a-zA-Z0-9._-]+
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            return Err(ToolError::execution_failed(format!(
                "Invalid worktree name \"{slug}\": each segment must contain only letters, digits, dots, underscores, and dashes"
            )));
        }
    }
    Ok(())
}

/// Flatten nested slugs (`user/feature` → `user+feature`) for branch names
/// and directory paths. Avoids git D/F conflicts and nested worktree dirs.
fn flatten_slug(slug: &str) -> String {
    slug.replace('/', "+")
}

/// Derive the worktree branch name from a slug: `worktree-{flatten_slug}`.
pub fn worktree_branch_name(slug: &str) -> String {
    format!("worktree-{}", flatten_slug(slug))
}

/// Derive the worktree directory path: `{repo_root}/.codesmith/worktrees/{flatten_slug}`.
pub fn worktree_path_for(repo_root: &Path, slug: &str) -> PathBuf {
    repo_root
        .join(".codesmith")
        .join("worktrees")
        .join(flatten_slug(slug))
}

/// Directory holding all session worktrees: `{repo_root}/.codesmith/worktrees/`.
pub fn worktrees_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".codesmith").join("worktrees")
}

/// Generate a random slug for unnamed worktrees.
/// Pattern: `{adjective}-{noun}-{4hex}`
pub fn generate_random_slug() -> String {
    let adjectives = ["swift", "bright", "calm", "keen", "bold"];
    let nouns = ["fox", "owl", "elm", "oak", "ray"];
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let adj_idx = (ts % adjectives.len() as u64) as usize;
    let noun_idx = ((ts >> 8) % nouns.len() as u64) as usize;
    let suffix = format!("{:04x}", (ts >> 16) & 0xFFFF);
    format!("{}-{}-{}", adjectives[adj_idx], nouns[noun_idx], suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_worktree_slug_accepts_valid_names() {
        assert!(validate_worktree_slug("my-feature").is_ok());
        assert!(validate_worktree_slug("v2.0").is_ok());
        assert!(validate_worktree_slug("user/feature").is_ok());
        assert!(validate_worktree_slug("a_b_c").is_ok());
    }

    #[test]
    fn validate_worktree_slug_rejects_too_long() {
        let long = "x".repeat(65);
        assert!(validate_worktree_slug(&long).is_err());
    }

    #[test]
    fn validate_worktree_slug_rejects_dot_dot_segments() {
        assert!(validate_worktree_slug("../traversal").is_err());
        assert!(validate_worktree_slug("..").is_err());
        assert!(validate_worktree_slug("valid/..").is_err());
    }

    #[test]
    fn validate_worktree_slug_rejects_empty_segment() {
        assert!(validate_worktree_slug("a//b").is_err());
    }

    #[test]
    fn validate_worktree_slug_rejects_special_chars() {
        assert!(validate_worktree_slug("name with spaces").is_err());
        assert!(validate_worktree_slug("name!").is_err());
    }

    #[test]
    fn flatten_slug_replaces_slashes() {
        assert_eq!(flatten_slug("user/feature"), "user+feature");
        assert_eq!(flatten_slug("simple"), "simple");
    }

    #[test]
    fn worktree_branch_name_composes_correctly() {
        assert_eq!(worktree_branch_name("feat"), "worktree-feat");
        assert_eq!(worktree_branch_name("a/b"), "worktree-a+b");
    }

    #[test]
    fn worktree_path_for_constructs_path() {
        let path = worktree_path_for(Path::new("/repo"), "my-feature");
        assert_eq!(path, PathBuf::from("/repo/.codesmith/worktrees/my-feature"));
    }

    #[test]
    fn worktrees_dir_constructs_path() {
        let dir = worktrees_dir(Path::new("/repo"));
        assert_eq!(dir, PathBuf::from("/repo/.codesmith/worktrees"));
    }
}

// ── Git helpers ───────────────────────────────────────────────────────────

/// Run a git command with no-credential-prompt env vars.
fn run_git(working_dir: &Path, args: &[&str]) -> Result<std::process::Output, ToolError> {
    let output = crate::dependencies::Git::output(args, working_dir)
        .map_err(|e| ToolError::execution_failed(format!("git command failed: {e}")));
    output
}

/// Find the canonical git root, resolving through worktree links.
/// If inside a worktree, returns the main repository root.
pub fn find_canonical_git_root(start_dir: &Path) -> Option<PathBuf> {
    let output = run_git(start_dir, &["rev-parse", "--git-dir"]).ok()?;
    if !output.status.success() {
        return None;
    }
    let git_dir_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let git_dir = start_dir.join(&git_dir_str);

    // For worktrees, .git is a file containing "gitdir: <path>".
    // The commondir file inside the worktree git dir points to the main repo.
    if git_dir.is_dir() {
        // Not a worktree — git_dir IS the main .git directory
        let commondir_path = git_dir.join("commondir");
        if let Some(common_dir_str) = std::fs::read_to_string(&commondir_path).ok() {
            // commondir usually contains just "." for the main repo
            let common_dir_str = common_dir_str.trim();
            let common_dir = if PathBuf::from(common_dir_str).is_relative() {
                git_dir.join(common_dir_str)
            } else {
                PathBuf::from(common_dir_str)
            };
            common_dir.parent().map(|p| p.to_path_buf())
        } else {
            start_dir.canonicalize().ok()
        }
    } else {
        // git_dir is a file (worktree gitlink)
        let gitlink = std::fs::read_to_string(&git_dir).ok()?;
        let gitlink_trimmed = gitlink.trim();
        let real_git_dir = if gitlink_trimmed.starts_with("gitdir: ") {
            PathBuf::from(gitlink_trimmed.strip_prefix("gitdir: ").unwrap())
        } else {
            PathBuf::from(gitlink_trimmed)
        };
        let resolved_git_dir = if real_git_dir.is_relative() {
            start_dir.join(real_git_dir)
        } else {
            real_git_dir
        };
        // Read commondir from the resolved git dir
        let commondir_path = resolved_git_dir.join("commondir");
        if let Some(common_dir_str) = std::fs::read_to_string(&commondir_path).ok() {
            let common_dir_str = common_dir_str.trim();
            let common_dir = if PathBuf::from(common_dir_str).is_relative() {
                resolved_git_dir.join(common_dir_str)
            } else {
                PathBuf::from(common_dir_str)
            };
            // The canonical root is the parent of the common .git directory
            common_dir.parent().map(|p| p.to_path_buf())
        } else {
            start_dir.canonicalize().ok()
        }
    }
}

/// Get the current branch name at a given path.
pub fn get_current_branch(working_dir: &Path) -> Result<String, ToolError> {
    let output = run_git(working_dir, &["branch", "--show-current"])?;
    if !output.status.success() {
        return Err(ToolError::execution_failed(
            "git branch --show-current failed",
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get HEAD commit SHA at a given path.
pub fn get_head_commit(working_dir: &Path) -> Result<String, ToolError> {
    let output = run_git(working_dir, &["rev-parse", "HEAD"])?;
    if !output.status.success() {
        return Err(ToolError::execution_failed("git rev-parse HEAD failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get the default branch (origin/main or origin/master).
pub fn get_default_branch(working_dir: &Path) -> Result<String, ToolError> {
    // Try origin/main first, then origin/master
    for branch in ["origin/main", "origin/master"] {
        let output = run_git(working_dir, &["rev-parse", "--verify", branch])?;
        if output.status.success() {
            return Ok(branch.to_string());
        }
    }
    // Fall back to HEAD
    Ok("HEAD".to_string())
}

// ── Worktree creation/removal ─────────────────────────────────────────────

/// Result of worktree creation.
pub struct WorktreeCreateResult {
    pub worktree_path: PathBuf,
    pub worktree_branch: String,
    pub head_commit: String,
    /// Whether the worktree already existed (fast resume).
    pub existed: bool,
}

/// Check if a worktree already exists at the expected path by reading
/// the HEAD file directly (no subprocess).
fn check_existing_worktree(worktree_path: &Path) -> Option<String> {
    // Read .git/HEAD or the gitlink file
    let git_path = worktree_path.join(".git");
    let head_path = if git_path.is_file() {
        // Worktree gitlink — read it to find real git dir
        let gitlink = std::fs::read_to_string(&git_path).ok()?;
        let gitlink_trimmed = gitlink.trim();
        let real_git_dir = if gitlink_trimmed.starts_with("gitdir: ") {
            PathBuf::from(gitlink_trimmed.strip_prefix("gitdir: ").unwrap())
        } else {
            PathBuf::from(gitlink_trimmed)
        };
        let resolved = if real_git_dir.is_relative() {
            worktree_path.join(real_git_dir)
        } else {
            real_git_dir
        };
        resolved.join("HEAD")
    } else if git_path.is_dir() {
        git_path.join("HEAD")
    } else {
        return None;
    };

    let head_content = std::fs::read_to_string(&head_path).ok()?;
    let head_content = head_content.trim();

    // Direct SHA or ref like "ref: refs/heads/main"
    if head_content.starts_with("ref: ") {
        let ref_path = head_content.strip_prefix("ref: ").unwrap();
        let ref_file = if let Some(parent) = head_path.parent() {
            parent.join(ref_path)
        } else {
            return None;
        };
        std::fs::read_to_string(ref_file)
            .ok()
            .map(|s| s.trim().to_string())
    } else {
        Some(head_content.to_string())
    }
}

/// Create a git worktree for the given slug, or resume if it already exists.
pub fn get_or_create_worktree(
    repo_root: &Path,
    slug: &str,
) -> Result<WorktreeCreateResult, ToolError> {
    validate_worktree_slug(slug)?;

    let worktree_path = worktree_path_for(repo_root, slug);
    let worktree_branch = worktree_branch_name(slug);

    // Fast resume: worktree already exists
    if let Some(head_sha) = check_existing_worktree(&worktree_path) {
        return Ok(WorktreeCreateResult {
            worktree_path,
            worktree_branch,
            head_commit: head_sha,
            existed: true,
        });
    }

    // New worktree: create .codesmith/worktrees/ directory
    std::fs::create_dir_all(worktrees_dir(repo_root))
        .map_err(|e| ToolError::execution_failed(format!("Failed to create worktrees dir: {e}")))?;

    // Resolve base branch
    let base_branch = get_default_branch(repo_root)?;
    let base_sha = get_head_commit(repo_root)?;

    // git worktree add -B <branch> <path> <base>
    // -B resets any orphan branch left by a previous removed worktree
    let output = run_git(
        repo_root,
        &[
            "worktree",
            "add",
            "-B",
            &worktree_branch,
            worktree_path.to_str().unwrap_or(""),
            &base_branch,
        ],
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ToolError::execution_failed(format!(
            "Failed to create worktree: {stderr}"
        )));
    }

    Ok(WorktreeCreateResult {
        worktree_path,
        worktree_branch,
        head_commit: base_sha,
        existed: false,
    })
}

/// Summary of changes in a worktree (for safety checks).
pub struct ChangeSummary {
    pub changed_files: usize,
    pub commits: usize,
}

/// Count uncommitted files and new commits in the worktree.
/// Returns `None` on any git failure (fail-closed: assume unsafe).
pub fn count_worktree_changes(
    worktree_path: &Path,
    original_head_commit: &Option<String>,
) -> Option<ChangeSummary> {
    // git status --porcelain
    let status_output = run_git(worktree_path, &["status", "--porcelain"]).ok()?;
    if !status_output.status.success() {
        return None;
    }
    let changed_files = String::from_utf8_lossy(&status_output.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();

    // Without a baseline commit, we can't count new commits — fail-closed.
    let original_head = original_head_commit.as_ref()?;
    let rev_output = run_git(
        worktree_path,
        &["rev-list", "--count", &format!("{original_head}..HEAD")],
    )
    .ok()?;
    if !rev_output.status.success() {
        return None;
    }
    let commits = String::from_utf8_lossy(&rev_output.stdout)
        .trim()
        .parse::<usize>()
        .unwrap_or(0);

    Some(ChangeSummary {
        changed_files,
        commits,
    })
}

/// Remove the git worktree directory and delete the branch.
/// Runs `git worktree remove --force` then `git branch -D`.
pub fn cleanup_worktree(
    worktree_path: &Path,
    worktree_branch: &Option<String>,
    git_root: &Path,
) -> Result<(), ToolError> {
    // git worktree remove --force <path>
    let output = run_git(
        git_root,
        &[
            "worktree",
            "remove",
            "--force",
            worktree_path.to_str().unwrap_or(""),
        ],
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Log but don't fail — the worktree dir might already be gone
        tracing::warn!("Failed to remove worktree: {}", stderr.trim());
    }

    // Delete the temporary branch (git-based only)
    if let Some(branch) = worktree_branch {
        // Brief pause for git to release locks
        std::thread::sleep(std::time::Duration::from_millis(100));
        let branch_output = run_git(git_root, &["branch", "-D", branch])?;
        if !branch_output.status.success() {
            let stderr = String::from_utf8_lossy(&branch_output.stderr);
            tracing::warn!("Could not delete worktree branch: {}", stderr.trim());
        }
    }

    Ok(())
}
