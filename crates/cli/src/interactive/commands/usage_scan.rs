//! Reading the saved sessions for the usage-inspection commands.
//!
//! `/fewer-permission-prompts` and `/skill-doctor` both answer "what does my
//! own history say": which shell commands were run, which skills were
//! activated. The sessions directory is the log that holds both — it is the
//! durable record of every turn (`docs/ARCHITECTURE.md`), so counting from it
//! counts what actually happened rather than what this process remembers.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - Only `~/.codesmith/sessions/*.json` is read: sessions saved elsewhere
//!   with `/save <path>`, and sessions a user deleted, are invisible.
//! - Reading is newest-first and capped by the caller, so a very long history
//!   is sampled rather than exhausted; the count printed by each command is
//!   the count it actually saw.
//! - A session file that does not parse is skipped, not reported: a broken
//!   file must not turn a usage report into an error wall. Callers say how
//!   many files they read.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Saved session files, newest first, at most `limit`.
pub fn session_files(limit: usize) -> Vec<PathBuf> {
    let Some(dir) = crate::session_manager::default_sessions_dir().ok() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.truncate(limit);
    files.into_iter().map(|(_, path)| path).collect()
}

fn messages(path: &Path) -> Option<Vec<Value>> {
    let content = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&content).ok()?;
    let messages = value.get("messages")?.as_array()?.clone();
    Some(messages)
}

/// Every shell command recorded in one session file, in order.
///
/// The shell tool's input is `{"command": "…"}`; a tool whose name is not one
/// of the shell family is skipped, so a `command` field on some other tool
/// cannot be mistaken for one.
pub fn shell_commands(path: &Path) -> Vec<String> {
    const SHELL_TOOLS: &[&str] = &["exec_shell", "shell", "bash", "run_shell"];

    let mut commands = Vec::new();
    for message in messages(path).unwrap_or_default() {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !SHELL_TOOLS.contains(&name) {
                continue;
            }
            if let Some(command) = block
                .pointer("/input/command")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                commands.push(command.to_string());
            }
        }
    }
    commands
}

/// Skill activations recorded in one session file.
///
/// Activation puts `# Skill: <name>` at the top of the instruction
/// (`skills_cmds::activate`), which is what makes a skill's use countable
/// from the record instead of from a counter that does not exist.
pub fn skill_activations(path: &Path) -> Vec<String> {
    const MARKER: &str = "# Skill: ";

    let mut names = Vec::new();
    for message in messages(path).unwrap_or_default() {
        // The marker counts activations of the instruction the engine
        // injects into USER messages — without the role/type gates, a
        // skill body that mentions the marker, an assistant reply echoing
        // it, or a tool result containing it all counted as extra
        // activations and inflated `/skill-doctor`'s numbers.
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("text") {
                continue;
            }
            let Some(text) = block.get("text").and_then(Value::as_str) else {
                continue;
            };
            // At most the FIRST marker per block: the activation
            // instruction embeds the whole skill body after the marker, so
            // later occurrences inside that body are content, not
            // activations.
            if let Some(index) = text.find(MARKER) {
                let rest = &text[index + MARKER.len()..];
                let name = rest.split_whitespace().next().unwrap_or("");
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(body: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.json");
        std::fs::write(&path, body).expect("write fixture");
        (dir, path)
    }

    #[test]
    fn shell_commands_come_from_the_shell_tool_only() {
        let (_dir, path) = fixture(
            r#"{"messages":[
              {"role":"user","content":[{"type":"text","text":"run it"}]},
              {"role":"assistant","content":[
                {"type":"tool_use","name":"exec_shell","input":{"command":"git status"}},
                {"type":"tool_use","name":"exec_shell","input":{"command":"  cargo test  "}},
                {"type":"tool_use","name":"read_file","input":{"command":"not a shell call"}}
              ]}
            ]}"#,
        );
        assert_eq!(shell_commands(&path), vec!["git status", "cargo test"]);
    }

    #[test]
    fn skill_activations_are_counted_from_the_instruction() {
        let (_dir, path) = fixture(
            r##"{"messages":[
              {"role":"user","content":[{"type":"text","text":"You are now using a skill.\n\n# Skill: code-review\n\nbody"}]},
              {"role":"assistant","content":[{"type":"text","text":"no marker here"}]},
              {"role":"user","content":[{"type":"text","text":"# Skill: batch\nmore"}]}
            ]}"##,
        );
        assert_eq!(skill_activations(&path), vec!["code-review", "batch"]);
    }

    #[test]
    fn a_broken_file_yields_nothing_instead_of_failing() {
        let (_dir, path) = fixture("{ not json");
        assert!(shell_commands(&path).is_empty());
        assert!(skill_activations(&path).is_empty());
        assert!(shell_commands(Path::new("/nonexistent/session.json")).is_empty());
    }
}
