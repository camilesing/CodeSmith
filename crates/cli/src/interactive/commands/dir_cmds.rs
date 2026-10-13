//! `/cd <path>` — move the session to another working directory, and
//! `/add-dir <path>` — allow one more directory for file access.
//!
//! The two are the two halves of the same trust question, which is why they
//! live together:
//!
//! - `/cd` moves the session's working directory (tools resolve relative
//!   paths from the new root, and the engine's workspace is re-synced), so
//!   the directory must already be trusted: the current workspace's own
//!   subtree, or a path the workspace trust record covers (`/trust add`).
//!   Anything else is refused with the exact `/trust add` line to run —
//!   moving the session is not a way to acquire file access to an untrusted
//!   tree.
//! - `/add-dir` adds a directory to that same trust record, which is what
//!   makes it usable both as a `/cd` target and as an out-of-workspace path
//!   for file tools.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - Project configuration (`.codesmith/config.toml`, project `AGENTS.md`)
//!   is loaded at startup and trust-gated against the *starting* directory;
//!   `/cd` moves the session's working directory and the engine's workspace
//!   but does not re-run that load. Restart in the new directory for its
//!   project config to apply.
//! - `/cd` re-syncs the engine with the current conversation (the same op
//!   `/resume` uses). A turn in flight is not interrupted; its remaining
//!   tool calls still resolve against the old root.
//! - `/add-dir` persists to `~/.codesmith/workspace-trust.json` (the same
//!   record `/trust add` writes). A *session-only* directory needs the
//!   engine's live path list, which the REPL does not hold; `[y]` on a
//!   path-access prompt is the session-scoped answer.

use std::path::{Path, PathBuf};

use colored::Colorize;

use super::ReplState;
use crate::workspace_trust::WorkspaceTrust;

/// Expand `~`, resolve relative paths against the workspace, and require the
/// result to be an existing directory.
///
/// # Errors
/// Names what is wrong (missing path, not a directory) so the caller prints
/// a reason rather than a stack of path components.
pub fn resolve_target(workspace: &Path, argument: &str) -> Result<PathBuf, String> {
    let raw = argument.trim();
    if raw.is_empty() {
        return Err("usage: /cd <path>".to_string());
    }
    let expanded = if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()
            .ok_or_else(|| "cannot expand `~`: no home directory".to_string())?
            .join(rest)
    } else if raw == "~" {
        dirs::home_dir().ok_or_else(|| "cannot expand `~`: no home directory".to_string())?
    } else if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        workspace.join(raw)
    };
    let canonical = expanded
        .canonicalize()
        .map_err(|err| format!("{}: {err}", expanded.display()))?;
    if !canonical.is_dir() {
        return Err(format!("{} is not a directory", canonical.display()));
    }
    Ok(canonical)
}

/// Whether `target` may become the session's working directory.
///
/// The current workspace's own subtree always may; anything else must be
/// covered by the workspace trust record.
#[must_use]
pub fn cd_allowed(workspace: &Path, target: &Path) -> bool {
    if target.starts_with(workspace) {
        return true;
    }
    WorkspaceTrust::load_for(workspace).permits(target)
}

/// `/cd <path>`.
pub async fn cd(state: &mut ReplState, argument: &str) -> anyhow::Result<()> {
    let target = match resolve_target(&state.workspace, argument) {
        Ok(target) => target,
        Err(err) => {
            println!("{} {err}", "✗".red());
            return Ok(());
        }
    };
    let previous = state.workspace.clone();
    if target == previous {
        println!("{}", format!("already in {}", target.display()).dimmed());
        return Ok(());
    }
    if !cd_allowed(&previous, &target) {
        println!(
            "{} {} is not trusted for this session — run `/trust add {}` first (or /add-dir)",
            "✗".red(),
            target.display(),
            target.display()
        );
        return Ok(());
    }

    state.workspace = target.clone();
    // Re-root the engine with the conversation intact: the same sync
    // `/resume` uses, which is what makes relative tool paths resolve from
    // the new directory.
    let messages = std::mem::take(&mut state.api_messages);
    let synced = match state.engine() {
        Ok(engine) => engine
            .send(crate::core::ops::Op::SyncSession {
                session_id: state.current_session_id.clone(),
                messages: messages.clone(),
                system_prompt: state.system_prompt.clone(),
                system_prompt_override: false,
                model: state.model.clone(),
                workspace: target.clone(),
            })
            .await
            .is_ok(),
        Err(_) => false,
    };
    state.api_messages = messages;
    println!(
        "{}",
        format!(
            "moved to {}{}",
            target.display(),
            if synced {
                ""
            } else {
                " — the engine could not be re-synced; restart in the new directory"
            }
        )
        .green()
    );
    println!(
        "{}",
        "project config and AGENTS.md were loaded at startup: restart here for the new directory's to apply"
            .dimmed()
    );
    Ok(())
}

/// `/add-dir <path>` — trust a directory for this workspace.
pub fn add_dir(state: &ReplState, argument: &str) -> anyhow::Result<()> {
    // Unlike `/cd`, the directory need not exist as a cwd target — but a
    // path that does not exist cannot be canonicalized, so it is refused
    // with the same message the resolver produces.
    let target = match resolve_target(&state.workspace, argument) {
        Ok(target) => target,
        Err(err) => {
            println!("{} {err}", "✗".red());
            return Ok(());
        }
    };
    match crate::workspace_trust::add(&state.workspace, &target) {
        Ok(stored) => println!(
            "{}",
            format!(
                "added {} — file tools may now work there and `/cd {}` moves the session",
                stored.display(),
                stored.display()
            )
            .green()
        ),
        Err(err) => println!("{} could not save the trust entry: {err}", "✗".red()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_resolve_and_must_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("child");
        std::fs::create_dir(&nested).expect("mkdir");

        let resolved = resolve_target(dir.path(), "child").expect("relative resolves");
        assert_eq!(resolved, nested.canonicalize().expect("canonical"));
        let resolved =
            resolve_target(dir.path(), nested.to_str().expect("utf8")).expect("absolute");
        assert_eq!(resolved, nested.canonicalize().expect("canonical"));

        assert!(
            resolve_target(dir.path(), "").is_err(),
            "empty is a usage error"
        );
        assert!(resolve_target(dir.path(), "missing-dir").is_err());
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "x").expect("write");
        assert!(
            resolve_target(dir.path(), "file.txt")
                .expect_err("a file is not a cwd")
                .contains("not a directory")
        );
    }

    #[test]
    fn the_workspace_subtree_is_always_allowed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("child");
        std::fs::create_dir(&nested).expect("mkdir");
        assert!(cd_allowed(dir.path(), dir.path()));
        assert!(
            cd_allowed(dir.path(), &nested),
            "a subdirectory needs no trust"
        );
    }
}
