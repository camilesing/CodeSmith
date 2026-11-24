//! Worktree isolation tools and utilities.
//!
//! Provides `enter_worktree` and `exit_worktree` tools for creating isolated
//! git worktrees during a session, plus shared session state and git helpers.

use std::path::{Path, PathBuf};

use codesmith_agent_runtime::dependencies::ExternalTool;
use codesmith_tools::ToolError;

mod enter;
mod exit;

pub use codesmith_agent_runtime::tool_state::worktree::*;
pub use enter::EnterWorktreeTool;
pub use exit::ExitWorktreeTool;

// ── Shared session state ──────────────────────────────────────────────────

// ── Slug validation ───────────────────────────────────────────────────────

#[allow(dead_code)]
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

// ── Git helpers ───────────────────────────────────────────────────────────

/// Run a git command with no-credential-prompt env vars.
fn run_git(working_dir: &Path, args: &[&str]) -> Result<std::process::Output, ToolError> {
    codesmith_agent_runtime::dependencies::Git::output(args, working_dir)
        .map_err(|e| ToolError::execution_failed(format!("git command failed: {e}")))
}

/// The directory session worktrees are created under and their git
/// commands run in: the workspace itself, canonicalized, when it is inside
/// a git repository. It only has to be inside the repository — `git worktree
/// add` resolves the repository from there whether the workspace is a main
/// checkout, a subdirectory of one, or a linked worktree — so the worktrees
/// land at `<workspace>/.codesmith/worktrees/`, inside the workspace the
/// file tools are contained to. Despite the name it is not the main
/// repository's root when the workspace is a linked worktree or a
/// subdirectory; nothing here needs that.
pub fn find_canonical_git_root(start_dir: &Path) -> Option<PathBuf> {
    let output = run_git(start_dir, &["rev-parse", "--git-dir"]).ok()?;
    if !output.status.success() {
        return None;
    }
    start_dir.canonicalize().ok()
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

/// Whether a worktree already exists at the expected path, and its HEAD.
/// Asks git: a worktree's `HEAD` names a branch (`ref: refs/heads/…`)
/// whose ref lives in the common dir or a packed-refs file, not under the
/// worktree's own git dir.
fn check_existing_worktree(worktree_path: &Path) -> Option<String> {
    if !worktree_path.join(".git").exists() {
        return None;
    }
    get_head_commit(worktree_path).ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── real-git fixtures ──────────────────────────────────────────────

    fn git_available() -> bool {
        codesmith_agent_runtime::dependencies::Git::available()
    }

    fn git_ok(root: &Path, args: &[&str]) {
        let output = run_git(root, args).expect("git should spawn");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repository on `main` (pinned: the default name is global config).
    fn init_git_repo(root: &Path) {
        git_ok(root, &["init", "-q", "-b", "main"]);
        git_ok(root, &["config", "user.email", "test@example.com"]);
        git_ok(root, &["config", "user.name", "Test User"]);
    }

    fn commit(root: &Path, message: &str) {
        // Nothing to add, and a global `commit.gpgsign` must not matter.
        git_ok(
            root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
            ],
        );
    }

    /// A session started inside a linked worktree creates its session
    /// worktrees under that workspace — where the file tools can reach them
    /// — not under the main repository's `.git` directory.
    #[test]
    fn a_session_in_a_linked_worktree_keeps_its_worktrees_inside_it() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).expect("mkdir");
        init_git_repo(&main);
        commit(&main, "init");
        let linked = tmp.path().join("linked");
        git_ok(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked",
                linked.to_str().unwrap(),
            ],
        );

        let root = find_canonical_git_root(&linked).expect("the workspace is a git repository");
        assert_eq!(
            root,
            linked.canonicalize().unwrap(),
            "the workspace itself, not a path under the main repo's .git"
        );
        let created = get_or_create_worktree(&root, "x").expect("session worktree");
        assert!(created.worktree_path.starts_with(&root));
        assert!(created.worktree_path.join(".git").is_file());
    }

    /// `exit_worktree(action="keep")` preserves a worktree so it can be
    /// entered again later. Re-entering resumes it and reports its current
    /// HEAD instead of failing to create it a second time.
    #[test]
    fn entering_an_existing_worktree_resumes_it() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        init_git_repo(tmp.path());
        commit(tmp.path(), "init");

        let created = get_or_create_worktree(tmp.path(), "x").expect("create");
        assert!(!created.existed);
        commit(&created.worktree_path, "work in the worktree");

        let resumed = get_or_create_worktree(tmp.path(), "x").expect("resume the kept worktree");
        assert!(resumed.existed);
        assert_eq!(
            resumed.head_commit,
            get_head_commit(&created.worktree_path).unwrap(),
            "the resumed HEAD, earlier work included"
        );
        assert_ne!(resumed.head_commit, created.head_commit);
    }

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
