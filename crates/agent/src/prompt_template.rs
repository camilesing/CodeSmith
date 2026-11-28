//! # Prompt templates
//!
//! The LangChain `ChatPromptTemplate` analog: role-tagged message templates
//! with `{var}` interpolation plus a history placeholder, rendered into the
//! wire [`Message`] / [`SystemPrompt`] types the executor and providers
//! already speak. A missing variable is a loud error naming the variable and
//! the known keys — a half-rendered prompt must never reach a model request.
//!
//! Known limitations (deliberate):
//! - String interpolation only (`{var}`, `{{`/`}}` escapes); no typed
//!   schema-checked variables, no image/multimodal templates, no few-shot
//!   example builders. Compose those at the call site.
//! - Exactly one history placeholder is supported; it splices the history at
//!   its position. Several placeholders are rejected at build time.

use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::memory::ChatHistory;
use crate::models::{ContentBlock, Message, SystemPrompt};

/// The role one template part renders as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptRole {
    System,
    User,
    Assistant,
}

impl PromptRole {
    fn as_str(self) -> &'static str {
        match self {
            PromptRole::System => "system",
            PromptRole::User => "user",
            PromptRole::Assistant => "assistant",
        }
    }
}

/// One part of a [`ChatPromptTemplate`]: a role-tagged template string, or
/// the position where conversation history splices in.
#[derive(Debug, Clone)]
pub enum PromptPart {
    /// A message rendered from `template` with `{var}` interpolation.
    Message { role: PromptRole, template: String },
    /// Splice the caller's [`ChatHistory`] messages here (the LangChain
    /// `MessagesPlaceholder` analog).
    HistoryPlaceholder,
}

/// A renderable conversation skeleton — role-tagged `{var}` templates plus
/// an optional history placeholder.
///
/// ```
/// use codesmith_agent::prompt_template::{
///     ChatPromptTemplate, PromptPart, PromptRole,
/// };
/// use std::collections::HashMap;
///
/// let template = ChatPromptTemplate::from_messages(vec![
///     (PromptRole::System, "You are {name}, a terse assistant.".into()),
///     (PromptRole::User, "{question}".into()),
/// ]);
/// let mut vars = HashMap::new();
/// vars.insert("name", "CodeSmith".to_string());
/// vars.insert("question", "hi".to_string());
/// let messages = template.format(&vars).unwrap();
/// assert_eq!(messages.len(), 2);
/// ```
#[derive(Debug, Clone)]
pub struct ChatPromptTemplate {
    parts: Vec<PromptPart>,
}

impl ChatPromptTemplate {
    /// Build from role/template pairs, no history placeholder.
    #[must_use]
    pub fn from_messages(messages: Vec<(PromptRole, String)>) -> Self {
        Self {
            parts: messages
                .into_iter()
                .map(|(role, template)| PromptPart::Message { role, template })
                .collect(),
        }
    }

    /// Build from explicit parts (the only way to place a history
    /// placeholder). Rejects more than one placeholder loudly — a second
    /// splice point is a template bug, not a rendering option.
    pub fn from_parts(parts: Vec<PromptPart>) -> Result<Self> {
        let placeholders = parts
            .iter()
            .filter(|p| matches!(p, PromptPart::HistoryPlaceholder))
            .count();
        if placeholders > 1 {
            bail!(
                "prompt template has {placeholders} history placeholders; at most one is supported"
            );
        }
        Ok(Self { parts })
    }

    /// Append a history placeholder at the end.
    pub fn with_history_placeholder(mut self) -> Self {
        self.parts.push(PromptPart::HistoryPlaceholder);
        self
    }

    /// Render every message part against `vars`. Errors loudly when the
    /// template contains a history placeholder (use
    /// [`format_with_history`](Self::format_with_history)) or an unknown
    /// variable.
    pub fn format(&self, vars: &HashMap<&str, String>) -> Result<Vec<Message>> {
        self.render(vars, None)
    }

    /// Render with the history placeholder spliced from `history`
    /// (`messages()` snapshot at render time).
    pub fn format_with_history(
        &self,
        vars: &HashMap<&str, String>,
        history: &dyn ChatHistory,
    ) -> Result<Vec<Message>> {
        let historic = history.messages();
        self.render(vars, Some(&historic))
    }

    /// Render the leading `System` part, if the template starts with one, as
    /// a [`SystemPrompt`] for [`crate::executor::AgentExecutorConfig::system`].
    /// `None` when the template has no leading system part.
    pub fn format_system(&self, vars: &HashMap<&str, String>) -> Result<Option<SystemPrompt>> {
        match self.parts.first() {
            Some(PromptPart::Message {
                role: PromptRole::System,
                template,
            }) => Ok(Some(SystemPrompt::Text(render_template(template, vars)?))),
            _ => Ok(None),
        }
    }

    fn render(
        &self,
        vars: &HashMap<&str, String>,
        history: Option<&[Message]>,
    ) -> Result<Vec<Message>> {
        let mut out = Vec::new();
        for (index, part) in self.parts.iter().enumerate() {
            match part {
                PromptPart::Message { role, template } => out.push(Message {
                    role: role.as_str().to_string(),
                    content: vec![ContentBlock::Text {
                        text: render_template(template, vars)?,
                        cache_control: None,
                    }],
                }),
                PromptPart::HistoryPlaceholder => {
                    let Some(history) = history else {
                        bail!(
                            "prompt template part {index} is a history placeholder; call \
                             format_with_history (or drop the placeholder)"
                        );
                    };
                    out.extend(history.iter().cloned());
                }
            }
        }
        Ok(out)
    }
}

/// Interpolate `{var}` from `vars`; `{{` and `}}` escape literal braces.
/// An unknown variable is a loud error naming it and the known keys.
fn render_template(template: &str, vars: &HashMap<&str, String>) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    out.push('{');
                    continue;
                }
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => bail!("template has an unclosed '{{' in: {template:?}"),
                    }
                }
                if name.is_empty() {
                    bail!("template has an empty '{{}}' placeholder in: {template:?}");
                }
                let Some(value) = vars.get(name.as_str()) else {
                    let mut known: Vec<&str> = vars.keys().copied().collect();
                    known.sort_unstable();
                    bail!(
                        "prompt template variable '{name}' is not provided; known variables: [{}]",
                        known.join(", ")
                    );
                };
                out.push_str(value);
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                }
                out.push('}');
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

// === Tests ==================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::VecChatHistory;
    use crate::models::ContentBlock;

    fn vars<'a>(pairs: &[(&'a str, &str)]) -> HashMap<&'a str, String> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    #[test]
    fn renders_roles_in_order() {
        let template = ChatPromptTemplate::from_messages(vec![
            (PromptRole::System, "sys {a}".into()),
            (PromptRole::User, "ask {b}".into()),
        ]);
        let messages = template
            .format(&vars(&[("a", "1"), ("b", "2")]))
            .expect("all vars provided");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        match &messages[1].content[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "ask 2"),
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    fn missing_variable_fails_loud_naming_it_and_known_keys() {
        // The known key is spelled to occur nowhere in the message's fixed
        // wording: `known` alone would match "known variables:" itself.
        let template =
            ChatPromptTemplate::from_messages(vec![(PromptRole::User, "{alpha} {missing}".into())]);
        let err = template
            .format(&vars(&[("alpha", "x")]))
            .expect_err("missing var must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("'missing'"), "{msg}");
        assert!(msg.contains("[alpha]"), "{msg}");
    }

    #[test]
    fn brace_escapes_and_literal_closing_brace() {
        let template =
            ChatPromptTemplate::from_messages(vec![(PromptRole::User, "{{json}} {v} }".into())]);
        let out = template
            .format(&vars(&[("v", "1")]))
            .expect("escapes render");
        match &out[0].content[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "{json} 1 }"),
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    fn history_placeholder_splices_and_format_rejects_it() {
        let template = ChatPromptTemplate::from_parts(vec![
            PromptPart::Message {
                role: PromptRole::User,
                template: "before".into(),
            },
            PromptPart::HistoryPlaceholder,
            PromptPart::Message {
                role: PromptRole::User,
                template: "after {v}".into(),
            },
        ])
        .expect("one placeholder is allowed");

        let err = template
            .format(&vars(&[("v", "x")]))
            .expect_err("format without history must fail");
        assert!(format!("{err:#}").contains("format_with_history"));

        let mut history = VecChatHistory::new();
        history.push(Message {
            role: "user".into(),
            content: vec![ContentBlock::Text {
                text: "old".into(),
                cache_control: None,
            }],
        });
        let messages = template
            .format_with_history(&vars(&[("v", "x")]), &history)
            .expect("renders with history");
        let texts: Vec<String> = messages
            .iter()
            .map(|m| match &m.content[0] {
                ContentBlock::Text { text, .. } => text.clone(),
                other => panic!("expected text block, got {other:?}"),
            })
            .collect();
        assert_eq!(texts, vec!["before", "old", "after x"]);
    }

    #[test]
    fn two_placeholders_are_rejected_at_build() {
        let err = ChatPromptTemplate::from_parts(vec![
            PromptPart::HistoryPlaceholder,
            PromptPart::HistoryPlaceholder,
        ])
        .expect_err("two placeholders must fail");
        assert!(format!("{err:#}").contains("at most one"));
    }

    #[test]
    fn leading_system_part_renders_as_system_prompt() {
        let template = ChatPromptTemplate::from_messages(vec![
            (PromptRole::System, "You are {name}.".into()),
            (PromptRole::User, "{q}".into()),
        ]);
        let system = template
            .format_system(&vars(&[("name", "X"), ("q", "y")]))
            .expect("renders");
        assert_eq!(system, Some(SystemPrompt::Text("You are X.".to_string())));

        let no_system = ChatPromptTemplate::from_messages(vec![(PromptRole::User, "{q}".into())]);
        assert_eq!(
            no_system
                .format_system(&vars(&[("q", "y")]))
                .expect("renders"),
            None
        );
    }
}
