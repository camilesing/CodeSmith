//! `remember` tool — model-callable memory write.
//!
//! Two modes:
//! - **Legacy** (KoD disabled): Appends a timestamped bullet to `memory.md`.
//! - **KoD** (KoD enabled): Writes a frontmatter-bearing `.md` file to the
//!   memory directory and appends a pointer line to `MEMORY.md`.
//!
//! Only registered when memory is enabled (`[memory] enabled = true`).
//! Auto-approved since it only writes to the user-owned memory location.

use async_trait::async_trait;
use serde_json::{Value, json};

use codesmith_agent_runtime::knowledge::paths::resolve_memory_entrypoint;
use codesmith_agent_runtime::knowledge::types::MemoryType;

use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};

/// Tool that writes durable memory entries.
pub struct RememberTool;

#[async_trait]
impl ToolSpec for RememberTool {
    fn name(&self) -> &'static str {
        "remember"
    }

    fn description(&self) -> &'static str {
        "Save a durable note to memory so it surfaces in future sessions. \
         When Knowledge On Demand is enabled, this creates a typed memory \
         file in the memory directory. Use 'memory_type' to categorize: \
         'user' (profile/preferences), 'feedback' (behavioral guidance), \
         'project' (ongoing work/goals), or 'reference' (external pointers). \
         Keep notes terse. Don't store secrets or transient tasks."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "note": {
                    "type": "string",
                    "description": "The durable note content to remember."
                },
                "name": {
                    "type": "string",
                    "description": "Short name for this memory (used as filename in KoD mode). Defaults to auto-generated slug."
                },
                "description": {
                    "type": "string",
                    "description": "One-line description of this memory (shown in KoD index for relevance ranking)."
                },
                "memory_type": {
                    "type": "string",
                    "enum": ["user", "feedback", "project", "reference"],
                    "description": "Category for this memory. Defaults to 'feedback'."
                }
            },
            "required": ["note"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let note = required_str(&input, "note")?;
        let memory_type_str = input
            .get("memory_type")
            .and_then(|v| v.as_str())
            .unwrap_or("feedback");
        let memory_type =
            MemoryType::from_str_loose(memory_type_str).unwrap_or(MemoryType::Feedback);
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());
        let description = input
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());

        // KoD mode: write frontmatter file to memory directory.
        if let Some(memory_dir) = &context.memory_dir {
            return write_kod_memory(memory_dir, note, name, description, memory_type);
        }

        // Legacy mode: append to single memory file.
        let path = context.memory_path.as_ref().ok_or_else(|| {
            ToolError::execution_failed(
                "user memory is disabled — set `[memory] enabled = true` in config.toml or \
                 `DEEPSEEK_MEMORY=on` in the environment to enable",
            )
        })?;

        codesmith_agent_runtime::memory::append_entry(path, note).map_err(|err| {
            ToolError::execution_failed(format!("failed to append to {}: {err}", path.display()))
        })?;

        Ok(ToolResult::success(format!(
            "remembered: {}",
            note.trim_start_matches('#').trim()
        )))
    }
}

/// Write a frontmatter-bearing memory file to the KoD directory.
fn write_kod_memory(
    memory_dir: &std::path::Path,
    note: &str,
    name: Option<String>,
    description: Option<String>,
    memory_type: MemoryType,
) -> Result<ToolResult, ToolError> {
    // Generate filename from name or slug.
    let filename = name.as_deref().map(slugify).unwrap_or_else(|| {
        slugify(
            &note
                .split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join("_"),
        )
    });
    let file_path = memory_dir.join(format!("{filename}.md"));

    // Build frontmatter. `name` / `description` are model-supplied and are
    // sanitized to a single-line YAML scalar so they cannot forge a
    // `\n---\n` document boundary, inject extra frontmatter keys, or break
    // out of the MEMORY.md pointer line below. `filename` (slug) and
    // `memory_type` (internally controlled enum) are already structurally
    // safe and pass through the sanitizer unchanged.
    let fm_name = sanitize_frontmatter_value(name.as_deref().unwrap_or(&filename));
    let fm_description = sanitize_frontmatter_value(description.as_deref().unwrap_or(note.trim()));

    let content = format!(
        "---\nname: {}\ndescription: {}\ntype: {}\n---\n{}",
        fm_name,
        fm_description,
        memory_type,
        note.trim()
    );

    // Ensure directory exists.
    std::fs::create_dir_all(memory_dir).map_err(|err| {
        ToolError::execution_failed(format!(
            "failed to create memory dir {}: {err}",
            memory_dir.display()
        ))
    })?;

    // Write file.
    codesmith_agent_runtime::utils::write_atomic(&file_path, content.as_bytes()).map_err(
        |err| {
            ToolError::execution_failed(format!("failed to write {}: {err}", file_path.display()))
        },
    )?;

    // Append pointer line to MEMORY.md entrypoint.
    let entrypoint_path = resolve_memory_entrypoint(memory_dir);
    let pointer_line = format!("- [{fm_name}]({filename}.md) — {fm_description}");
    if let Err(err) = append_to_entrypoint(&entrypoint_path, &pointer_line) {
        // Non-critical: entrypoint update failure shouldn't block the write.
        // Log but don't fail.
        eprintln!("warning: failed to update MEMORY.md entrypoint: {err}");
    }

    Ok(ToolResult::success(format!(
        "remembered as {memory_type} memory: {}",
        note.trim().chars().take(60).collect::<String>()
    )))
}

/// Sanitize a model-supplied scalar for use as a YAML frontmatter value.
///
/// Newlines / carriage returns are replaced with spaces so a value can
/// never spill onto its own frontmatter line — this defuses both extra-key
/// injection and the `\n---\n` document-boundary forgery, because the value
/// is guaranteed single-line afterwards. When the single-line result would
/// not parse safely as a plain scalar (it contains any of
/// ``:#{}[],&*?|-<>=!%@\``` or `'` — a leading quote would open a quoted
/// scalar — or it has leading/trailing whitespace), it is emitted in YAML
/// single-quoted style with embedded `'` doubled, which parses back to the
/// exact same string.
fn sanitize_frontmatter_value(value: &str) -> String {
    let single_line = value.replace(['\n', '\r'], " ");
    let needs_quoting = single_line.starts_with(' ')
        || single_line.ends_with(' ')
        || single_line.chars().any(|c| {
            matches!(
                c,
                ':' | '#'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | ','
                    | '&'
                    | '*'
                    | '?'
                    | '|'
                    | '-'
                    | '<'
                    | '>'
                    | '='
                    | '!'
                    | '%'
                    | '@'
                    | '`'
                    | '\''
            )
        });
    if needs_quoting {
        format!("'{}'", single_line.replace('\'', "''"))
    } else {
        single_line
    }
}

/// Append a pointer line to the MEMORY.md entrypoint.
fn append_to_entrypoint(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    // Check if the line already exists (dedup pointer lines).
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing.contains(line)
    {
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    use std::io::Write;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Convert a string to a filesystem-safe slug.
fn slugify(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn ctx_with_memory(path: PathBuf) -> ToolContext {
        let mut ctx = ToolContext::new(path.parent().unwrap_or_else(|| std::path::Path::new(".")));
        ctx.memory_path = Some(path);
        ctx
    }

    fn ctx_with_kod(dir: PathBuf) -> ToolContext {
        let mut ctx = ToolContext::new(&dir);
        ctx.memory_dir = Some(dir);
        ctx.memory_path = None; // KoD mode doesn't use legacy file
        ctx
    }

    #[tokio::test]
    async fn returns_error_when_memory_disabled() {
        let tmp = tempdir().unwrap();
        let mut ctx = ToolContext::new(tmp.path());
        ctx.memory_path = None;
        ctx.memory_dir = None;

        let tool = RememberTool;
        let err = tool
            .execute(json!({"note": "use 4 spaces for indentation"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("memory is disabled"), "{err}");
    }

    #[tokio::test]
    async fn appends_bullet_to_memory_file_legacy() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path.clone());

        let tool = RememberTool;
        let result = tool
            .execute(json!({"note": "use 4 spaces for indentation"}), &ctx)
            .await
            .expect("ok");
        assert!(result.success);
        assert!(result.content.contains("4 spaces"));

        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.contains("4 spaces"));
        assert!(body.starts_with("- ("), "{body}");
    }

    #[tokio::test]
    async fn writes_frontmatter_file_kod_mode() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("memory");
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ctx_with_kod(dir.clone());

        let tool = RememberTool;
        let result = tool
            .execute(
                json!({
                    "note": "User prefers concise responses",
                    "name": "concise preference",
                    "description": "User wants short answers",
                    "memory_type": "feedback"
                }),
                &ctx,
            )
            .await
            .expect("ok");

        assert!(result.success);
        assert!(result.content.contains("feedback memory"));

        // Check the written file.
        let file_path = dir.join("concise_preference.md");
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("name: concise preference"));
        assert!(content.contains("type: feedback"));
        assert!(content.contains("User prefers concise responses"));

        // Check MEMORY.md entrypoint.
        let entrypoint = dir.join("MEMORY.md");
        let entry_content = std::fs::read_to_string(&entrypoint).unwrap();
        assert!(entry_content.contains("concise_preference"));
    }

    #[tokio::test]
    async fn kod_mode_auto_slug_when_no_name() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("memory");
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ctx_with_kod(dir.clone());

        let tool = RememberTool;
        let result = tool
            .execute(json!({"note": "project uses pytest"}), &ctx)
            .await
            .expect("ok");

        assert!(result.success);
        // Should have created a file with auto-generated slug name.
        let md_files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .filter(|e| e.file_name() != "MEMORY.md")
            .collect();
        assert_eq!(md_files.len(), 1);
    }

    #[tokio::test]
    async fn rejects_missing_note_field() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path);

        let tool = RememberTool;
        let err = tool.execute(json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("note"), "{err}");
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("User Role"), "user_role");
        assert_eq!(slugify("build/config"), "build_config");
        assert_eq!(slugify("  spaced  out  "), "spaced_out");
    }

    // ── frontmatter sanitization ────────────────────────────────────────

    #[test]
    fn sanitize_keeps_plain_values_verbatim() {
        assert_eq!(
            sanitize_frontmatter_value("concise preference"),
            "concise preference"
        );
        assert_eq!(sanitize_frontmatter_value("user_role"), "user_role");
    }

    #[test]
    fn sanitize_strips_newlines_to_single_line() {
        assert_eq!(
            sanitize_frontmatter_value("evil\n---\ninjected: yes"),
            "'evil --- injected: yes'",
            "newline strip removes the --- boundary; `:`/`-` force quoting"
        );
        // CRLF collapses to two spaces (one per replaced line-break char) —
        // still a single line, which is the structural guarantee.
        assert_eq!(sanitize_frontmatter_value("a\r\nb"), "a  b");
    }

    #[test]
    fn sanitize_quotes_special_characters_and_doubles_quotes() {
        assert_eq!(sanitize_frontmatter_value("a: b"), "'a: b'");
        assert_eq!(sanitize_frontmatter_value("#comment"), "'#comment'");
        assert_eq!(sanitize_frontmatter_value("it's"), "'it''s'");
        assert_eq!(sanitize_frontmatter_value(" padded "), "' padded '");
    }

    #[test]
    fn frontmatter_injection_stays_single_document() {
        // Model-supplied name tries to forge a `\n---\n` document boundary
        // and an extra `injected: yes` key. The written file must still be
        // a single frontmatter document with exactly one `name` key whose
        // value round-trips to the sanitized expectation.
        let tmp = tempdir().unwrap();
        let result = write_kod_memory(
            tmp.path(),
            "note body",
            Some("evil\n---\ninjected: yes".to_string()),
            None,
            MemoryType::Feedback,
        )
        .expect("write");

        assert!(result.success);
        // Filename comes from the slugified name — newlines/dashes collapse.
        let file_path = tmp.path().join("evil_injected_yes.md");
        let content = std::fs::read_to_string(&file_path).expect("memory file");

        // Single document: exactly two `---` delimiter lines, nothing after
        // the closing delimiter that re-opens frontmatter.
        assert_eq!(
            content.lines().filter(|l| l.trim() == "---").count(),
            2,
            "no forged document boundary; got:\n{content}"
        );

        // Frontmatter body: exactly the three expected keys, one per line.
        let lines: Vec<&str> = content.lines().collect();
        let fm: Vec<&&str> = lines[1..].iter().take_while(|l| **l != "---").collect();
        assert_eq!(fm.len(), 3, "frontmatter keys: {fm:?}");
        assert_eq!(fm.iter().filter(|l| l.starts_with("name:")).count(), 1);
        assert_eq!(fm.iter().filter(|l| l.starts_with("injected:")).count(), 0);

        // The name value is single-quoted with the injected newline folded
        // to a space; unquoting (strip quotes, undouble `''`) yields the
        // expected single value.
        let name_line = *fm.iter().find(|l| l.starts_with("name:")).unwrap();
        let raw = name_line.strip_prefix("name: ").unwrap();
        let unquoted = raw
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .map(|s| s.replace("''", "'"))
            .unwrap_or_else(|| raw.to_string());
        assert_eq!(unquoted, "evil --- injected: yes");

        // The note body stays intact below the closing delimiter.
        assert!(content.ends_with("---\nnote body"));
    }

    #[test]
    fn kod_pointer_line_stays_single_line_for_hostile_description() {
        // The MEMORY.md pointer line interpolates the sanitized name /
        // description — a hostile description must not add lines there.
        let tmp = tempdir().unwrap();
        write_kod_memory(
            tmp.path(),
            "note",
            Some("ptr".to_string()),
            Some("desc\n---\nname: forged".to_string()),
            MemoryType::User,
        )
        .expect("write");

        let entry = std::fs::read_to_string(tmp.path().join("MEMORY.md")).expect("MEMORY.md");
        let lines: Vec<&str> = entry.lines().collect();
        assert_eq!(lines.len(), 1, "pointer stays a single line: {entry}");
        assert!(lines[0].starts_with("- [ptr](ptr.md) —"));
    }
}
