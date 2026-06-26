use std::path::Path;

use crate::agent_memory::AgentMemoryScope;
use crate::knowledge::entrypoint::load_entrypoint;

/// Compose the prompt overlay for an agent's scoped memory directory.
#[must_use]
pub fn compose_agent_memory_prompt(
    agent_type: &str,
    scope: AgentMemoryScope,
    memory_dir: &Path,
) -> String {
    let entrypoint = load_entrypoint(memory_dir);
    let mut block = format!(
        "<agent_memory agent_type=\"{}\" scope=\"{}\" source=\"{}\">\n",
        xml_escape_attr(agent_type),
        scope.as_str(),
        xml_escape_attr(&memory_dir.display().to_string())
    );

    block.push_str(
        "You have persistent memory for this sub-agent type. Use it only for durable, non-secret facts that help this agent do future work. MEMORY.md is the index; detailed memories should live in topic .md files in this directory. Use agent_memory_read, agent_memory_write, and agent_memory_edit to inspect or update only this memory directory.\n\n",
    );

    if let Some(entrypoint) = entrypoint {
        block.push_str("Current MEMORY.md:\n");
        block.push_str(&entrypoint.content);
        if entrypoint.was_line_truncated || entrypoint.was_byte_truncated {
            block.push_str("\n\n[Note: MEMORY.md was truncated to fit prompt budget. Use agent_memory_read on MEMORY.md if you need the full index.]");
        }
    } else {
        block.push_str(
            "Current MEMORY.md is empty or missing. Create durable entries only when useful; keep the index concise and link to topic files.",
        );
    }

    block.push_str("\n</agent_memory>");
    block
}

fn xml_escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn prompt_includes_guidance_when_empty() {
        let tmp = tempdir().unwrap();
        let prompt = compose_agent_memory_prompt("explore", AgentMemoryScope::Project, tmp.path());
        assert!(prompt.contains("<agent_memory"));
        assert!(prompt.contains("MEMORY.md is empty"));
        assert!(prompt.contains("agent_memory_read"));
    }

    #[test]
    fn prompt_includes_entrypoint_content() {
        let tmp = tempdir().unwrap();
        std::fs::write(
            tmp.path().join("MEMORY.md"),
            "- [Style](style.md) — prefer concise reports",
        )
        .unwrap();
        let prompt = compose_agent_memory_prompt("review", AgentMemoryScope::User, tmp.path());
        assert!(prompt.contains("prefer concise reports"));
        assert!(prompt.contains("scope=\"user\""));
    }
}
