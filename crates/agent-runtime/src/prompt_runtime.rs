use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::models::{SystemBlock, SystemPrompt};

pub const SECTION_SEPARATOR: &str = "\n\n";
pub const SECTION_HEADER_PREFIX: &str = "<!-- prompt-section:";
pub const SECTION_HEADER_SUFFIX: &str = " -->";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptSectionStability {
    Static,
    Workspace,
    Session,
    Dynamic,
}

impl PromptSectionStability {
    pub fn label(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Workspace => "workspace",
            Self::Session => "session",
            Self::Dynamic => "dynamic",
        }
    }

    pub fn is_reusable_prefix(self) -> bool {
        matches!(self, Self::Static | Self::Workspace)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptCachePolicy {
    Cacheable,
    Uncached,
    CacheBreaker,
}

impl PromptCachePolicy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cacheable => "cacheable",
            Self::Uncached => "uncached",
            Self::CacheBreaker => "cache_breaker",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptSectionSource {
    Builtin,
    ProjectContext,
    Skills,
    Config,
    Cli,
    RuntimeOverride,
    Memory,
    Handoff,
    Mcp,
    Debug,
}

impl PromptSectionSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::ProjectContext => "project_context",
            Self::Skills => "skills",
            Self::Config => "config",
            Self::Cli => "cli",
            Self::RuntimeOverride => "runtime_override",
            Self::Memory => "memory",
            Self::Handoff => "handoff",
            Self::Mcp => "mcp",
            Self::Debug => "debug",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSection {
    pub id: String,
    pub title: String,
    pub body: String,
    pub stability: PromptSectionStability,
    pub cache_policy: PromptCachePolicy,
    pub source: PromptSectionSource,
}

impl PromptSection {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        stability: PromptSectionStability,
        cache_policy: PromptCachePolicy,
        source: PromptSectionSource,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            body: body.into(),
            stability,
            cache_policy,
            source,
        }
    }

    pub fn cacheable(
        id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        stability: PromptSectionStability,
        source: PromptSectionSource,
    ) -> Self {
        Self::new(
            id,
            title,
            body,
            stability,
            PromptCachePolicy::Cacheable,
            source,
        )
    }

    pub fn render(&self) -> String {
        let body = self.body.trim();
        if body.is_empty() {
            return String::new();
        }
        format!(
            "{SECTION_HEADER_PREFIX} id=\"{}\" title=\"{}\" stability=\"{}\" cache=\"{}\" source=\"{}\"{SECTION_HEADER_SUFFIX}\n{}",
            escape_attr(&self.id),
            escape_attr(&self.title),
            self.stability.label(),
            self.cache_policy.label(),
            self.source.label(),
            body
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptBundle {
    pub sections: Vec<PromptSection>,
    pub dynamic_boundary_index: Option<usize>,
}

impl PromptBundle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_sections(sections: Vec<PromptSection>) -> Self {
        let dynamic_boundary_index = sections.iter().position(|section| {
            matches!(
                section.stability,
                PromptSectionStability::Session | PromptSectionStability::Dynamic
            ) || section.cache_policy != PromptCachePolicy::Cacheable
        });
        Self {
            sections,
            dynamic_boundary_index,
        }
    }

    pub fn push(&mut self, section: PromptSection) {
        if self.dynamic_boundary_index.is_none()
            && (matches!(
                section.stability,
                PromptSectionStability::Session | PromptSectionStability::Dynamic
            ) || section.cache_policy != PromptCachePolicy::Cacheable)
        {
            self.dynamic_boundary_index = Some(self.sections.len());
        }
        self.sections.push(section);
    }

    pub fn extend(&mut self, sections: impl IntoIterator<Item = PromptSection>) {
        for section in sections {
            self.push(section);
        }
    }

    pub fn render_text(&self) -> String {
        self.sections
            .iter()
            .map(PromptSection::render)
            .filter(|section| !section.trim().is_empty())
            .collect::<Vec<_>>()
            .join(SECTION_SEPARATOR)
    }

    pub fn render_system_prompt(&self) -> SystemPrompt {
        SystemPrompt::Text(self.render_text())
    }

    pub fn stable_prefix_text(&self) -> String {
        self.sections
            .iter()
            .filter(|section| {
                section.stability.is_reusable_prefix()
                    && section.cache_policy == PromptCachePolicy::Cacheable
            })
            .map(PromptSection::render)
            .filter(|section| !section.trim().is_empty())
            .collect::<Vec<_>>()
            .join(SECTION_SEPARATOR)
    }
}

#[derive(Debug, Clone, Default)]
pub struct EffectiveSystemPromptInput {
    pub default_bundle: PromptBundle,
    pub custom_system_prompt: Option<String>,
    pub agent_system_prompt: Option<String>,
    pub coordinator_system_prompt: Option<String>,
    pub override_system_prompt: Option<String>,
    pub append_sections: Vec<PromptSection>,
}

pub fn build_effective_system_prompt(input: EffectiveSystemPromptInput) -> PromptBundle {
    let mut bundle = if let Some(prompt) = non_empty(input.override_system_prompt) {
        PromptBundle::from_sections(vec![PromptSection::cacheable(
            "runtime_override",
            "Runtime override system prompt",
            prompt,
            PromptSectionStability::Session,
            PromptSectionSource::RuntimeOverride,
        )])
    } else if let Some(prompt) = non_empty(input.coordinator_system_prompt) {
        PromptBundle::from_sections(vec![PromptSection::cacheable(
            "coordinator_override",
            "Coordinator system prompt",
            prompt,
            PromptSectionStability::Session,
            PromptSectionSource::RuntimeOverride,
        )])
    } else if let Some(prompt) = non_empty(input.agent_system_prompt) {
        PromptBundle::from_sections(vec![PromptSection::cacheable(
            "agent_override",
            "Agent system prompt",
            prompt,
            PromptSectionStability::Session,
            PromptSectionSource::RuntimeOverride,
        )])
    } else if let Some(prompt) = non_empty(input.custom_system_prompt) {
        PromptBundle::from_sections(vec![PromptSection::cacheable(
            "custom_system_prompt",
            "Custom system prompt",
            prompt,
            PromptSectionStability::Session,
            PromptSectionSource::Config,
        )])
    } else {
        input.default_bundle
    };
    bundle.extend(input.append_sections);
    bundle
}

#[derive(Debug, Default)]
pub struct PromptRuntime {
    cache: HashMap<String, PromptSection>,
}

impl PromptRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear_cached_sections(&mut self) {
        self.cache.clear();
    }

    pub fn invalidate_section(&mut self, id: &str) {
        self.cache.remove(id);
    }

    pub fn cached_or_insert_with(
        &mut self,
        id: &str,
        build: impl FnOnce() -> PromptSection,
    ) -> PromptSection {
        if let Some(section) = self.cache.get(id) {
            return section.clone();
        }
        let section = build();
        if section.cache_policy == PromptCachePolicy::Cacheable {
            self.cache.insert(id.to_string(), section.clone());
        }
        section
    }

    pub fn render_effective_system_prompt(
        &mut self,
        input: EffectiveSystemPromptInput,
    ) -> SystemPrompt {
        build_effective_system_prompt(input).render_system_prompt()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPromptSection<'a> {
    pub id: Option<String>,
    pub title: Option<String>,
    pub stability: Option<PromptSectionStability>,
    pub cache_policy: Option<PromptCachePolicy>,
    pub source: Option<PromptSectionSource>,
    pub body: &'a str,
}

pub fn parse_rendered_sections(content: &str) -> Option<Vec<ParsedPromptSection<'_>>> {
    if !content.contains(SECTION_HEADER_PREFIX) {
        return None;
    }

    let mut sections = Vec::new();
    let mut cursor = 0usize;
    while let Some(relative_start) = content[cursor..].find(SECTION_HEADER_PREFIX) {
        let start = cursor + relative_start;
        let header_end_relative = content[start..].find(SECTION_HEADER_SUFFIX)?;
        let header_end = start + header_end_relative + SECTION_HEADER_SUFFIX.len();
        let header = &content[start + SECTION_HEADER_PREFIX.len()..start + header_end_relative];
        let body_start = if content[header_end..].starts_with('\n') {
            header_end + 1
        } else {
            header_end
        };
        let next_start = content[body_start..]
            .find(SECTION_HEADER_PREFIX)
            .map(|idx| body_start + idx)
            .unwrap_or(content.len());
        let body = content[body_start..next_start].trim();
        sections.push(ParsedPromptSection {
            id: attr(header, "id"),
            title: attr(header, "title"),
            stability: attr(header, "stability").and_then(|value| parse_stability(&value)),
            cache_policy: attr(header, "cache").and_then(|value| parse_cache_policy(&value)),
            source: attr(header, "source").and_then(|value| parse_source(&value)),
            body,
        });
        cursor = next_start;
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections)
    }
}

pub fn system_prompt_to_text(system: &SystemPrompt) -> Option<String> {
    match system {
        SystemPrompt::Text(text) => Some(text.clone()),
        SystemPrompt::Blocks(blocks) => blocks_to_text(blocks),
    }
}

fn blocks_to_text(blocks: &[SystemBlock]) -> Option<String> {
    let joined = blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

fn attr(header: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = header.find(&needle)? + needle.len();
    let rest = &header[start..];
    let end = rest.find('"')?;
    Some(unescape_attr(&rest[..end]))
}

fn parse_stability(value: &str) -> Option<PromptSectionStability> {
    match value {
        "static" => Some(PromptSectionStability::Static),
        "workspace" => Some(PromptSectionStability::Workspace),
        "session" => Some(PromptSectionStability::Session),
        "dynamic" => Some(PromptSectionStability::Dynamic),
        _ => None,
    }
}

fn parse_cache_policy(value: &str) -> Option<PromptCachePolicy> {
    match value {
        "cacheable" => Some(PromptCachePolicy::Cacheable),
        "uncached" => Some(PromptCachePolicy::Uncached),
        "cache_breaker" => Some(PromptCachePolicy::CacheBreaker),
        _ => None,
    }
}

fn parse_source(value: &str) -> Option<PromptSectionSource> {
    match value {
        "builtin" => Some(PromptSectionSource::Builtin),
        "project_context" => Some(PromptSectionSource::ProjectContext),
        "skills" => Some(PromptSectionSource::Skills),
        "config" => Some(PromptSectionSource::Config),
        "cli" => Some(PromptSectionSource::Cli),
        "runtime_override" => Some(PromptSectionSource::RuntimeOverride),
        "memory" => Some(PromptSectionSource::Memory),
        "handoff" => Some(PromptSectionSource::Handoff),
        "mcp" => Some(PromptSectionSource::Mcp),
        "debug" => Some(PromptSectionSource::Debug),
        _ => None,
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape_attr(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_prompt_uses_override_before_default_and_append_last() {
        let default_bundle = PromptBundle::from_sections(vec![PromptSection::cacheable(
            "default",
            "Default",
            "default body",
            PromptSectionStability::Static,
            PromptSectionSource::Builtin,
        )]);
        let bundle = build_effective_system_prompt(EffectiveSystemPromptInput {
            default_bundle,
            override_system_prompt: Some("override body".to_string()),
            custom_system_prompt: Some("custom body".to_string()),
            append_sections: vec![PromptSection::cacheable(
                "append",
                "Append",
                "append body",
                PromptSectionStability::Session,
                PromptSectionSource::Cli,
            )],
            ..Default::default()
        });
        let rendered = bundle.render_text();
        assert!(rendered.contains("override body"));
        assert!(!rendered.contains("default body"));
        assert!(!rendered.contains("custom body"));
        assert!(rendered.ends_with("append body"));
    }

    #[test]
    fn rendered_sections_round_trip_metadata() {
        let bundle = PromptBundle::from_sections(vec![PromptSection::cacheable(
            "a",
            "A",
            "body",
            PromptSectionStability::Workspace,
            PromptSectionSource::ProjectContext,
        )]);
        let rendered = bundle.render_text();
        let parsed = parse_rendered_sections(&rendered).expect("sections");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id.as_deref(), Some("a"));
        assert_eq!(parsed[0].title.as_deref(), Some("A"));
        assert_eq!(parsed[0].stability, Some(PromptSectionStability::Workspace));
        assert_eq!(parsed[0].source, Some(PromptSectionSource::ProjectContext));
        assert_eq!(parsed[0].body, "body");
    }
}
