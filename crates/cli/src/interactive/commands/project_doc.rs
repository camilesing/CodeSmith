//! AGENTS.md bootstrap (moved from the retired `commands/init.rs`).

use std::fmt::Write as _;
use std::path::Path;

/// If `workspace` is inside a git repository, ensure `.codesmith/` is
/// listed in the repo root's `.gitignore` so that snapshots, instructions,
/// and other workspace-local state are not accidentally committed.
pub(crate) fn ensure_codesmith_gitignored(workspace: &Path) {
    // Find the nearest ENCLOSING repository: a workspace opened as a
    // subdirectory of a larger repo (a common layout) has no `.git` of its
    // own, and the old `workspace/.git` check silently skipped the ignore
    // entry entirely there.
    let Some(repo_root) = workspace.ancestors().find(|dir| dir.join(".git").exists()) else {
        return;
    };

    let gitignore = repo_root.join(".gitignore");
    let entries = [".codesmith/"];

    // Read existing contents once. Missing = empty, but a real read
    // failure (permissions, non-UTF-8) must not masquerade as empty —
    // the dedup check below would then re-append `.codesmith/` on every
    // invocation, accumulating duplicate lines.
    let existing = match std::fs::read_to_string(&gitignore) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            crate::logging::warn(format!(
                "cannot read {}: {err} — .codesmith/ may not be git-ignored",
                gitignore.display()
            ));
            return;
        }
    };
    let mut missing: Vec<&str> = Vec::new();
    for entry in entries {
        let entry_no_slash = entry.trim_end_matches('/');
        let already_ignored = existing.lines().any(|line| {
            let trimmed = line.trim();
            trimmed == entry || trimmed == entry_no_slash
        });
        if !already_ignored {
            missing.push(entry);
        }
    }

    if missing.is_empty() {
        return;
    }

    // Append missing entries. If .gitignore doesn't exist yet, create it.
    // Failures are loud: on permission errors, disk-full, or read-only
    // mounts the user must not believe `.codesmith/` is ignored while
    // snapshots/instructions get committed.
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gitignore)
    {
        Ok(mut file) => {
            // If the file is non-empty and doesn't end with a newline, add one first.
            if !existing.is_empty() && !existing.ends_with('\n') {
                let _ = writeln!(file);
            }
            for entry in &missing {
                if let Err(err) = writeln!(file, "{entry}") {
                    crate::logging::warn(format!(
                        "cannot append {entry} to {}: {err} — .codesmith/ may not be git-ignored",
                        gitignore.display()
                    ));
                    break;
                }
            }
        }
        Err(err) => crate::logging::warn(format!(
            "cannot open {} for append: {err} — .codesmith/ may not be git-ignored",
            gitignore.display()
        )),
    }
}

/// Generate project documentation based on detected project type
pub(crate) fn generate_project_doc(workspace: &Path) -> String {
    let mut doc = String::new();

    // Header
    doc.push_str("# Project Instructions\n\n");
    doc.push_str("This file provides context for AI assistants working on this project.\n\n");

    // Detect project type
    let project_info = detect_project_type(workspace);
    doc.push_str(&project_info);

    // Agent behavior — conventions, gotchas, testing
    doc.push_str("## Agent Guidance\n\n");
    doc.push_str("<!-- How should an AI agent approach this project? Fill in tool gotchas, -->\n");
    doc.push_str("<!-- file patterns to avoid, and anything that helps a model navigate -->\n");
    doc.push_str("<!-- the codebase without reading every file. -->\n");
    doc.push('\n');
    doc.push_str(
        "- **CodeSmith reads this file as:** <!-- AGENTS.md (compatible with other agents) -->\n",
    );
    doc.push_str(
        "- **Read-only surface:** <!-- Which directories can the agent read but not write? -->\n",
    );
    doc.push_str(
        "- **Never edit:** <!-- Files that are generated, vendored, or owned by another tool -->\n",
    );
    doc.push_str("- **Always test with:** <!-- The single command that validates a change (e.g. `cargo test -p foo`) -->\n");
    doc.push('\n');

    // Architecture — the "big picture" that requires reading multiple files
    doc.push_str("## Architecture\n\n");
    doc.push_str("<!-- Describe the high-level structure. What are the key modules and how -->\n");
    doc.push_str("<!-- do they connect? Focus on the context a new contributor would need. -->\n");
    doc.push('\n');
    doc.push_str("### Entry Points\n");
    doc.push_str(
        "<!-- Where does execution start? Binary entry, request handler, main loop? -->\n",
    );
    doc.push('\n');
    doc.push_str("### Key Modules\n");
    doc.push_str("<!-- List the 3-6 most important directories/files and their role -->\n");
    doc.push('\n');
    doc.push_str("### Data Flow\n");
    doc.push_str("<!-- How does a request / event / input travel through the system? -->\n");
    doc.push('\n');

    // Cache-aware editing — helps maintain prefix-cache hit rates
    doc.push_str("## Cache Stability\n\n");
    doc.push_str("<!-- DeepSeek V4 uses a byte-stable prefix cache (128-token granularity). -->\n");
    doc.push_str(
        "<!-- Keeping these things stable turn-over-turn saves ~90% on input tokens. -->\n",
    );
    doc.push('\n');
    doc.push_str("- **Frequently-rebuilt files:** <!-- Generated code, lockfiles, build artifacts → mark as cache-churn -->\n");
    doc.push_str("- **Stable scaffolding:** <!-- Config files, project instructions, model cards → keep byte-stable -->\n");
    doc.push_str("- **Append, don't reorder:** <!-- New context goes at the end of the request; reordering invalidates cache -->\n");
    doc.push('\n');

    // Guidelines
    doc.push_str("## Guidelines\n\n");
    doc.push_str("- Follow existing code style and patterns\n");
    doc.push_str("- Write tests for new functionality\n");
    doc.push_str("- Keep changes focused and atomic\n");
    doc.push_str("- Document public APIs\n");
    doc.push_str("- Update this file when project conventions change\n");

    doc
}

/// Detect project type and return relevant information
fn detect_project_type(workspace: &Path) -> String {
    let mut info = String::new();

    // Check for Rust project
    if workspace.join("Cargo.toml").exists() {
        info.push_str("## Project Type: Rust\n\n");
        info.push_str("### Commands\n");
        info.push_str("- Build: `cargo build`\n");
        info.push_str("- Test: `cargo test`\n");
        info.push_str("- Run: `cargo run`\n");
        info.push_str("- Check: `cargo check`\n");
        info.push_str("- Format: `cargo fmt`\n");
        info.push_str("- Lint: `cargo clippy`\n\n");

        // Try to extract project name from Cargo.toml
        if let Some(name) = std::fs::read_to_string(workspace.join("Cargo.toml"))
            .ok()
            .and_then(|content| extract_cargo_name(&content))
        {
            let _ = write!(info, "### Project: {name}\n\n");
        }
    }
    // Check for Node.js project
    else if workspace.join("package.json").exists() {
        info.push_str("## Project Type: Node.js\n\n");
        info.push_str("### Commands\n");
        info.push_str("- Install: `npm install`\n");
        info.push_str("- Test: `npm test`\n");
        info.push_str("- Build: `npm run build`\n");
        info.push_str("- Start: `npm start`\n\n");

        // Check for common frameworks
        if workspace.join("next.config.js").exists() || workspace.join("next.config.ts").exists() {
            info.push_str("### Framework: Next.js\n\n");
        } else if workspace.join("vite.config.js").exists()
            || workspace.join("vite.config.ts").exists()
        {
            info.push_str("### Framework: Vite\n\n");
        }
    }
    // Check for Python project
    else if workspace.join("pyproject.toml").exists() || workspace.join("setup.py").exists() {
        info.push_str("## Project Type: Python\n\n");
        info.push_str("### Commands\n");
        if workspace.join("pyproject.toml").exists() {
            info.push_str("- Install: `pip install -e .`\n");
        }
        info.push_str("- Test: `pytest`\n");
        info.push_str("- Format: `black .`\n");
        info.push_str("- Lint: `ruff check .`\n\n");
    }
    // Check for Go project
    else if workspace.join("go.mod").exists() {
        info.push_str("## Project Type: Go\n\n");
        info.push_str("### Commands\n");
        info.push_str("- Build: `go build`\n");
        info.push_str("- Test: `go test ./...`\n");
        info.push_str("- Run: `go run .`\n");
        info.push_str("- Format: `go fmt ./...`\n\n");
    }
    // Unknown project type
    else {
        info.push_str("## Project Type: Unknown\n\n");
        info.push_str("<!-- Add build/test commands here -->\n\n");
    }

    // Check for README
    if workspace.join("README.md").exists() {
        info.push_str("### Documentation\n");
        info.push_str("See README.md for project overview.\n\n");
    }

    // Check for .gitignore
    if workspace.join(".gitignore").exists() {
        info.push_str("### Version Control\n");
        info.push_str("This project uses Git. See .gitignore for excluded files.\n\n");
    }

    info
}

/// Extract project name from Cargo.toml
fn extract_cargo_name(content: &str) -> Option<String> {
    // Anchor to the `[package]` table and the exact `name` key: a plain
    // `starts_with("name")` also matched `namespace = "…"` and took the
    // first hit anywhere (`[[bin]]`, `[workspace.package]`, …), so the
    // generated doc could carry a wrong project name.
    let mut in_package = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "name"
        {
            return Some(
                value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string(),
            );
        }
    }
    None
}
