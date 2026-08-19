#![allow(dead_code)]
//! System prompts for different modes.
//!
//! Re-export shim: portable production logic lives in
//! `codesmith_agent_runtime::prompts`. The two `..._with_context[_and_skills]`
//! entry points stay TUI-local because they pre-render the skills block via
//! `crate::skills::render_*` (skills discovery is workspace-coupled and has
//! not yet migrated to the runtime crate). The verbatim test module below
//! exercises the full public surface via the glob re-export.

pub use codesmith_agent_runtime::prompts::*;
pub use codesmith_agent_runtime::prompt_sources::{InstructionSource, PromptAppendSource};

use crate::models::SystemPrompt;
use crate::tui::app::AppMode;
use crate::tui::approval::ApprovalMode;
use std::path::Path;

/// Get the system prompt for a specific mode with project context.
pub fn system_prompt_for_mode_with_context(
    mode: AppMode,
    workspace: &Path,
    working_set_summary: Option<&str>,
) -> SystemPrompt {
    system_prompt_for_mode_with_context_and_skills(
        mode,
        workspace,
        working_set_summary,
        None,
        None,
        None,
        None,
    )
}

/// Get the system prompt for a specific mode with project and skills context.
///
/// **Volatile-content-last invariant.** Blocks are appended in order from
/// most-static to most-volatile so DeepSeek's KV prefix cache hits the
/// longest possible byte prefix turn-over-turn:
///
///   1. mode prompt (compile-time constant)
///   2. project context / fallback (workspace-static)
///   3. skills block (skills-dir-static)
///   4. `## Context Management` (compile-time constant, Agent/Yolo only)
///   5. compaction relay template (compile-time constant)
///   6. relay block — file-backed; rewritten by `/compact` and on exit
///
/// Anything appended after a volatile block forfeits the cache for the rest
/// of the request. New blocks belong above the relay boundary unless they
/// themselves are turn-volatile. Working-set metadata is now injected into the
/// latest user message as per-turn metadata instead of this system prompt.
pub fn system_prompt_for_mode_with_context_and_skills(
    mode: AppMode,
    workspace: &Path,
    working_set_summary: Option<&str>,
    skills_dir: Option<&Path>,
    instructions: Option<&[InstructionSource]>,
    user_memory_block: Option<&str>,
    knowledge_prompt_block: Option<&str>,
) -> SystemPrompt {
    system_prompt_for_mode_with_context_skills_and_session(
        mode,
        workspace,
        working_set_summary,
        skills_dir,
        instructions,
        PromptSessionContext {
            user_memory_block,
            knowledge_prompt_block,
            goal_objective: None,
            project_context_pack_enabled: true,
            locale_tag: "en",
            translation_enabled: false,
            model_id: "codesmith",
            show_thinking: true,
            is_simple: false,
            skills_block: crate::skills::render_available_skills_context_for_workspace(workspace)
                .or_else(|| skills_dir.and_then(crate::skills::render_available_skills_context)),
        },
    )
}

#[cfg(test)]
mod tests {
    // Don't assert on prose. If you wouldn't fail a code review for
    // changing the wording, don't fail a test for it.
    use super::*;
    use tempfile::tempdir;

    /// Discriminator unique to the injected relay block (not present in the
    /// agent prompt's own discussion of the convention).
    const HANDOFF_BLOCK_MARKER: &str = "left a relay artifact at `.codesmith/handoff.md`";

    #[test]
    fn memory_extraction_prompt_uses_recent_messages_and_existing_memory() {
        let messages = vec![
            MemoryExtractionMessage {
                role: "user".to_string(),
                content: "first".to_string(),
            },
            MemoryExtractionMessage {
                role: "assistant".to_string(),
                content: "second".to_string(),
            },
            MemoryExtractionMessage {
                role: "user".to_string(),
                content: "third".to_string(),
            },
        ];

        let prompt = build_memory_extraction_prompt(&messages, Some("Existing preference"), 2);
        assert!(prompt.system_prompt.contains("Memory Extraction Protocol"));
        assert!(prompt.user_prompt.contains("## Existing memory"));
        assert!(prompt.user_prompt.contains("Existing preference"));
        assert!(!prompt.user_prompt.contains("first"));
        assert!(prompt.user_prompt.contains("second"));
        assert!(prompt.user_prompt.contains("third"));
    }

    #[test]
    fn prompt_override_storage_reports_duplicate_sets() {
        let cell = std::sync::OnceLock::new();

        assert_eq!(effective_prompt_override(&cell, "fallback"), "fallback");
        assert!(set_prompt_override(&cell, "first".to_string()).is_ok());
        assert_eq!(effective_prompt_override(&cell, "fallback"), "first");
        assert_eq!(
            set_prompt_override(&cell, "second".to_string()),
            Err("second".to_string())
        );
        assert_eq!(effective_prompt_override(&cell, "fallback"), "first");
    }

    fn contains_cjk(text: &str) -> bool {
        text.chars().any(|ch| {
            matches!(
                ch,
                '\u{3040}'..='\u{30ff}'
                    | '\u{3400}'..='\u{4dbf}'
                    | '\u{4e00}'..='\u{9fff}'
                    | '\u{f900}'..='\u{faff}'
            )
        })
    }

    #[test]
    fn base_prompt_carries_execution_discipline_block() {
        // The XML-tagged execution-discipline block is the contract —
        // verify each section name is present so reviewers can't quietly
        // strip the rules that herd V4 toward acting instead of narrating.
        for tag in [
            "<tool_persistence>",
            "<mandatory_tool_use>",
            "<act_dont_ask>",
            "<verification>",
            "<missing_context>",
        ] {
            assert!(
                BASE_PROMPT.contains(tag),
                "BASE_PROMPT missing required tag {tag}"
            );
        }
        assert!(
            BASE_PROMPT.contains("Tool-use enforcement"),
            "BASE_PROMPT missing the tool-use enforcement clause"
        );
    }

    #[test]
    fn base_prompt_carries_constitutional_preamble() {
        // Pin the load-bearing Constitutional anchors. The exact prose
        // can evolve, but CodeSmith must keep the Brother Whale preamble,
        // the coordination principle, and the hierarchy of law.
        for phrase in [
            "We begin with Brother Whale",
            "Brother Whale is the founding intelligence",
            "Every model that runs here is Brother Whale",
            "future intelligences can better coordinate",
            "Article II — The Primacy of Truth",
            "Article VII — The Hierarchy of Law",
        ] {
            assert!(
                BASE_PROMPT.contains(phrase),
                "BASE_PROMPT missing Constitutional phrase {phrase:?}"
            );
        }
    }

    #[test]
    fn constitutional_hierarchy_keeps_case_command_above_local_law() {
        let case_at = BASE_PROMPT
            .find("2. **Case Command.**")
            .expect("case command tier present");
        let statute_at = BASE_PROMPT
            .find("3. **Statutes.**")
            .expect("statutes tier present");
        let local_law_at = BASE_PROMPT
            .find("5. **Local Law.**")
            .expect("local law tier present");

        assert!(
            case_at < statute_at && statute_at < local_law_at,
            "Article VII must keep the current user request above runtime guidance and local law"
        );
        assert!(
            BASE_PROMPT.contains("actual runtime gates still determine what tools can execute"),
            "Article VII must distinguish prompt authority from executable runtime gates"
        );
    }

    #[test]
    fn base_prompt_contains_model_id_template() {
        assert!(
            BASE_PROMPT.contains("{model_id}"),
            "BASE_PROMPT must contain the {{model_id}} template for dynamic injection"
        );
    }

    #[test]
    fn apply_model_template_replaces_placeholder() {
        let result = apply_model_template("You are {model_id}", "deepseek-v4-pro");
        assert_eq!(result, "You are deepseek-v4-pro");
        assert!(!result.contains("{model_id}"));
    }

    #[test]
    fn compose_prompt_injects_model_id() {
        let prompt = compose_prompt_with_approval_and_model(
            AppMode::Agent,
            Personality::Calm,
            ApprovalMode::Suggest,
            "deepseek-v4-flash",
        );
        assert!(
            prompt.contains("You are deepseek-v4-flash"),
            "composed prompt must contain the injected model id"
        );
        assert!(
            !prompt.contains("{model_id}"),
            "composed prompt must not contain the raw template placeholder"
        );
    }

    #[test]
    fn composed_prompt_starts_with_core_tool_taxonomy() {
        let prompt = compose_prompt_with_approval_and_model(
            AppMode::Agent,
            Personality::Calm,
            ApprovalMode::Suggest,
            "deepseek-v4-pro",
        );
        let expected_taxonomy = render_core_tool_taxonomy_block(AppMode::Agent);

        assert!(
            prompt.starts_with(&expected_taxonomy),
            "composed prompt should start with the compact generated tool taxonomy"
        );
    }

    #[test]
    fn plan_prompt_taxonomy_omits_run_tests() {
        let prompt = compose_prompt_with_approval_and_model(
            AppMode::Plan,
            Personality::Calm,
            ApprovalMode::Never,
            "deepseek-v4-pro",
        );
        let expected_taxonomy = render_core_tool_taxonomy_block(AppMode::Plan);

        assert!(
            prompt.starts_with(&expected_taxonomy),
            "Plan prompt should start with its mode-specific tool taxonomy"
        );
        assert!(
            expected_taxonomy.contains("for discovery")
                && expected_taxonomy.contains("for git inspection"),
            "Plan taxonomy should keep read-only discovery and git guidance"
        );
        assert!(
            !expected_taxonomy.contains("run_tests")
                && !expected_taxonomy.contains("for verification")
                && !expected_taxonomy.contains("Use  "),
            "Plan taxonomy must not advertise unavailable verification tools: {expected_taxonomy:?}"
        );
    }

    #[test]
    fn core_tool_taxonomy_only_references_default_active_tools() {
        let core_tools = crate::core::engine::default_active_native_tool_names();
        for tool in TOOL_TAXONOMY_DISCOVERY
            .iter()
            .chain(TOOL_TAXONOMY_GIT)
            .chain(TOOL_TAXONOMY_VERIFICATION)
        {
            assert!(
                core_tools.contains(tool),
                "tool taxonomy references {tool}, but it is not in the eager native-tool list"
            );
        }
    }

    #[test]
    fn authority_recap_appears_in_full_prompt() {
        let tmp = tempdir().expect("tempdir");
        let text = match system_prompt_for_mode_with_context_skills_session_and_approval(
            AppMode::Agent,
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext::default(),
            ApprovalMode::Suggest,
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        assert!(
            text.contains("## Authority Recap"),
            "full system prompt must contain the authority recap"
        );
        assert!(
            text.contains("The Constitution of CodeSmith (Articles I-VII) governs your behavior"),
            "authority recap must reference the Constitution"
        );
    }

    #[test]
    fn calm_personality_declares_tier_8_subordination() {
        assert!(
            CALM_PERSONALITY.contains("Tier 8"),
            "Calm personality must identify as Tier 8"
        );
        assert!(
            CALM_PERSONALITY.contains("cannot override"),
            "Calm personality must have a subordination clause"
        );
    }

    #[test]
    fn execution_discipline_is_at_the_end_for_cache_stability() {
        // DeepSeek's prefix cache keys on a leading byte-stable run, so
        // the new sections must be appended, not interleaved earlier.
        let body = BASE_PROMPT;
        let persistence_at = body
            .find("<tool_persistence>")
            .expect("tool_persistence anchor present");
        let language_at = body.find("## Language").expect("Language anchor present");
        assert!(
            language_at < persistence_at,
            "execution-discipline block must come after the early sections"
        );
    }

    #[test]
    fn plan_mode_prompt_uses_update_plan_as_confirmation_handoff() {
        assert!(
            PLAN_MODE.contains("call `update_plan`"),
            "Plan mode must tell the model to finish plans through update_plan"
        );
        assert!(
            PLAN_MODE.contains("accept / revise / exit prompt"),
            "Plan mode must explain why update_plan is the UI handoff signal"
        );
    }

    #[test]
    fn render_environment_block_lists_supplied_locale_and_workspace() {
        let tmp = tempdir().expect("tempdir");
        let block = render_environment_block(tmp.path(), "zh-Hans");
        assert!(block.starts_with("## Environment"));
        assert!(block.contains("- lang: zh-Hans"));
        assert!(block.contains(&format!(
            "- deepseek_version: {}",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(block.contains(&format!("- pwd: {}", tmp.path().display())));
        assert!(block.contains("- platform:"));
        assert!(block.contains("- shell:"));
    }

    #[test]
    fn locale_reinforcement_preamble_returns_native_script_for_supported_locales() {
        // English (and unknown locales) get None — the existing English
        // directive in `base.md` is sufficient.
        assert!(locale_reinforcement_preamble("en").is_none());
        assert!(locale_reinforcement_preamble("en-US").is_none());
        assert!(locale_reinforcement_preamble("fr-FR").is_none());
        assert!(locale_reinforcement_preamble("").is_none());

        // zh-Hans (and the de-facto equivalents the TUI accepts) get a
        // native-script preamble. The text must explicitly mention
        // `reasoning_content` (the V4 knob this is meant to steer) and
        // preserve tool-name immutability — those are the load-bearing
        // claims behind the #1118 fix that someone could quietly
        // delete in a future translation pass.
        for tag in ["zh-Hans", "zh-CN", "zh"] {
            let preamble =
                locale_reinforcement_preamble(tag).expect("zh-Hans preamble should exist");
            assert!(
                preamble.contains("简体中文"),
                "zh preamble must be in Simplified Chinese: {preamble:?}"
            );
            assert!(
                preamble.contains("reasoning_content"),
                "zh preamble must steer reasoning_content: {preamble:?}"
            );
            assert!(
                preamble.contains("read_file"),
                "zh preamble must call out tool-name immutability: {preamble:?}"
            );
        }

        let ja = locale_reinforcement_preamble("ja").expect("ja preamble");
        assert!(ja.contains("日本語"), "ja preamble must be in Japanese");
        assert!(ja.contains("reasoning_content"));

        let pt = locale_reinforcement_preamble("pt-BR").expect("pt-BR preamble");
        assert!(
            pt.contains("português do Brasil"),
            "pt preamble must call out pt-BR explicitly"
        );
        assert!(pt.contains("reasoning_content"));
    }

    #[test]
    fn system_prompt_prepends_locale_preamble_for_zh_hans() {
        // Build the full system prompt with locale=zh-Hans and assert
        // the native-script preamble shows up *before* the English
        // base-prompt body. Cache stability and attention precedence
        // both depend on this ordering.
        let tmp = tempdir().expect("tempdir");
        let text = match system_prompt_for_mode_with_context_skills_session_and_approval(
            AppMode::Agent,
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                user_memory_block: None,
                knowledge_prompt_block: None,
                goal_objective: None,
                project_context_pack_enabled: false,
                locale_tag: "zh-Hans",
                translation_enabled: false,
                model_id: "codesmith",
                show_thinking: true,
                is_simple: false,
                skills_block: None,
            },
            ApprovalMode::Suggest,
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        let preamble_marker = "## 语言要求";
        let base_marker = "You are codesmith";
        let preamble_pos = text
            .find(preamble_marker)
            .expect("zh-Hans preamble should be present");
        let base_pos = text
            .find(base_marker)
            .expect("base prompt should be present");
        assert!(
            preamble_pos < base_pos,
            "locale preamble must precede the English base prompt (preamble={preamble_pos}, base={base_pos})",
        );
    }

    #[test]
    fn locale_reinforcement_closer_returns_native_script_for_supported_locales() {
        // English (and unknown locales) get None.
        assert!(locale_reinforcement_closer("en").is_none());
        assert!(locale_reinforcement_closer("fr-FR").is_none());
        assert!(locale_reinforcement_closer("").is_none());

        // Each supported locale gets a closer in its own script that
        // explicitly tells the model "don't drift to English even as
        // English context accumulates" — that's the load-bearing claim
        // behind the bookend pattern.
        let zh = locale_reinforcement_closer("zh-Hans").expect("zh closer");
        assert!(
            zh.contains("简体中文"),
            "zh closer must be in Simplified Chinese"
        );
        assert!(
            zh.contains("reasoning_content"),
            "zh closer must steer reasoning_content"
        );
        let ja = locale_reinforcement_closer("ja").expect("ja closer");
        assert!(ja.contains("日本語"), "ja closer must be in Japanese");
        assert!(ja.contains("reasoning_content"));
        let pt = locale_reinforcement_closer("pt-BR").expect("pt-BR closer");
        assert!(pt.contains("português do Brasil"));
        assert!(pt.contains("reasoning_content"));
    }

    #[test]
    fn system_prompt_bookends_zh_hans_with_preamble_and_closer() {
        // The full system prompt for zh-Hans must contain BOTH the
        // opening preamble (`## 语言要求`) and the closing reinforcement
        // (`## 语言再次提醒`), with the closer appearing AFTER the
        // preamble — i.e. the prompt is "bookended" in native script,
        // matching the empirical finding from the WeChat thread that
        // motivated the closer.
        let tmp = tempdir().expect("tempdir");
        let text = match system_prompt_for_mode_with_context_skills_session_and_approval(
            AppMode::Agent,
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                user_memory_block: None,
                knowledge_prompt_block: None,
                goal_objective: None,
                project_context_pack_enabled: false,
                locale_tag: "zh-Hans",
                translation_enabled: false,
                model_id: "codesmith",
                show_thinking: true,
                is_simple: false,
                skills_block: None,
            },
            ApprovalMode::Suggest,
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        let preamble_pos = text
            .find("## 语言要求")
            .expect("zh-Hans preamble must be in prompt");
        let closer_pos = text
            .find("## 语言再次提醒")
            .expect("zh-Hans closer must be in prompt");
        assert!(
            preamble_pos < closer_pos,
            "closer must come after preamble (preamble={preamble_pos}, closer={closer_pos})",
        );
        // The closer must be the very last block — anything else after
        // it defeats the recency-bias purpose. Skip the closer's own
        // `## ` header before scanning.
        let closer_header_end = closer_pos + "## 语言再次提醒".len();
        let after_closer_body = &text[closer_header_end..];
        assert!(
            !after_closer_body.contains("\n## "),
            "no other top-level section should follow the closer; got: {after_closer_body:?}",
        );
    }

    #[test]
    fn simple_conversation_style_section_is_conditional_on_is_simple() {
        let tmp = tempdir().expect("tempdir");
        let build = |is_simple| {
            match system_prompt_for_mode_with_context_skills_session_and_approval(
                AppMode::Agent,
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    knowledge_prompt_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codesmith",
                    show_thinking: true,
                    is_simple,
                    skills_block: None,
                },
                ApprovalMode::Suggest,
            ) {
                SystemPrompt::Text(text) => text,
                SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
            }
        };

        assert!(
            build(true).contains("## Conversation Style: Simple"),
            "is_simple must append the conversation-style section"
        );
        assert!(
            !build(false).contains("## Conversation Style: Simple"),
            "default prompt must not include the conversation-style section"
        );
    }

    #[test]
    fn hidden_thinking_uses_english_reasoning_without_locale_bookends() {
        let tmp = tempdir().expect("tempdir");
        let text = match system_prompt_for_mode_with_context_skills_session_and_approval(
            AppMode::Agent,
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                user_memory_block: None,
                knowledge_prompt_block: None,
                goal_objective: None,
                project_context_pack_enabled: false,
                locale_tag: "zh-Hans",
                translation_enabled: false,
                model_id: "codesmith",
                show_thinking: false,
                is_simple: false,
                skills_block: None,
            },
            ApprovalMode::Suggest,
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };

        assert!(
            text.contains("## Hidden Thinking Language"),
            "hidden thinking prompt must include the request-side language override"
        );
        assert!(
            text.contains("reasoning_content") && text.contains("English"),
            "hidden thinking override must steer reasoning_content to English"
        );
        assert!(
            text.contains("final reply") && text.contains("Simplified Chinese"),
            "hidden thinking override must preserve the visible reply language"
        );
        assert!(
            !text.contains("## 语言要求") && !text.contains("## 语言再次提醒"),
            "hidden thinking prompt must not also ask for localized reasoning"
        );

        let hidden_pos = text
            .find("## Hidden Thinking Language")
            .expect("hidden thinking block present");
        let hidden_header_end = hidden_pos + "## Hidden Thinking Language".len();
        let after_hidden_body = &text[hidden_header_end..];
        assert!(
            !after_hidden_body.contains("\n## "),
            "hidden thinking override must be the final top-level block; got: {after_hidden_body:?}",
        );
    }

    #[test]
    fn system_prompt_skips_locale_preamble_for_english() {
        // English locale → no preamble injected. Asserts the
        // "preamble is opt-in for non-English" invariant.
        let tmp = tempdir().expect("tempdir");
        let text = match system_prompt_for_mode_with_context_skills_session_and_approval(
            AppMode::Agent,
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                user_memory_block: None,
                knowledge_prompt_block: None,
                goal_objective: None,
                project_context_pack_enabled: false,
                locale_tag: "en",
                translation_enabled: false,
                model_id: "codesmith",
                show_thinking: true,
                is_simple: false,
                skills_block: None,
            },
            ApprovalMode::Suggest,
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        assert!(
            !text.contains("语言要求"),
            "English locale must not get a zh preamble: {text:?}"
        );
        assert!(
            !text.contains("言語要件"),
            "English locale must not get a ja preamble: {text:?}"
        );
        assert!(
            !text.contains("Requisito de Idioma"),
            "English locale must not get a pt-BR preamble: {text:?}"
        );
        // Closer too — same bookend rule.
        assert!(
            !text.contains("语言再次提醒"),
            "English locale must not get a zh closer: {text:?}"
        );
        assert!(
            !text.contains("言語再確認"),
            "English locale must not get a ja closer: {text:?}"
        );
        assert!(
            !text.contains("Reforço de Idioma"),
            "English locale must not get a pt-BR closer: {text:?}"
        );
        assert!(
            !contains_cjk(BASE_PROMPT),
            "base prompt must not contain static CJK priming tokens"
        );
        for mode in [AppMode::Agent, AppMode::Plan, AppMode::Yolo] {
            let taxonomy = render_core_tool_taxonomy_block(mode);
            assert!(
                !contains_cjk(&taxonomy),
                "tool taxonomy must not contain static CJK priming tokens: {taxonomy:?}"
            );
        }
        // Do not assert on arbitrary CJK in the full system prompt: project
        // context may legitimately contain localized file names, README text,
        // or user-authored instructions. The locale bookend markers above are
        // the priming tokens this test is meant to guard.
    }

    #[test]
    fn language_section_carries_reasoning_content_directives_for_1118() {
        // #1118 ("Language has been configured to Chinese, but thinking
        // outputs are still in English"): the base prompt's language
        // section is the only knob that steers V4's `reasoning_content`
        // language. Pin the load-bearing phrases so a future innocuous
        // edit can't quietly drop them.
        let lang = BASE_PROMPT;
        assert!(
            lang.contains("reasoning_content"),
            "language section must explicitly call out reasoning_content"
        );
        assert!(
            lang.contains("latest user message"),
            "latest user message must be the primary language signal"
        );
        assert!(
            lang.contains("clearly English") && lang.contains("must stay English"),
            "English user turns must stay English even after localized context"
        );
        assert!(
            lang.contains("Simplified Chinese")
                && lang.contains("must both be in Simplified Chinese"),
            "Chinese user turns must still steer reasoning_content and replies"
        );
        assert!(
            lang.contains("README.zh-CN.md") && lang.contains("tool results"),
            "localized docs and tool results must be named as non-language signals"
        );
        // Explicit-user-override clause keeps the prompt useful for the
        // opposite preference (#1118 commenters who want English
        // thinking for token-cost reasons).
        for phrase in ["think in English", "reason in Chinese"] {
            assert!(
                lang.contains(phrase),
                "expected the user-override example `{phrase}`"
            );
        }
    }

    #[test]
    fn environment_block_is_inserted_into_system_prompt() {
        let tmp = tempdir().expect("tempdir");
        let prompt = match system_prompt_for_mode_with_context_skills_and_session(
            AppMode::Agent,
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                user_memory_block: None,
                knowledge_prompt_block: None,
                goal_objective: None,
                project_context_pack_enabled: true,
                locale_tag: "ja",
                translation_enabled: false,
                model_id: "codesmith",
                show_thinking: true,
                is_simple: false,
                skills_block: None,
            },
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        assert!(prompt.contains("## Environment"));
        assert!(prompt.contains("- lang: ja"));
        assert!(prompt.contains("- deepseek_version:"));
    }

    #[test]
    fn memory_guidance_carries_paired_examples() {
        // The fragment is the contract — verify the verbatim ✓ / ✗
        // pair is present so V4 has both shapes to imitate.
        assert!(MEMORY_GUIDANCE.contains("declarative facts"));
        assert!(MEMORY_GUIDANCE.contains(" ✓"));
        assert!(MEMORY_GUIDANCE.contains(" ✗"));
        assert!(MEMORY_GUIDANCE.contains("Imperative"));
    }

    #[test]
    fn memory_guidance_absent_when_no_memory_block() {
        let tmp = tempdir().expect("tempdir");
        let prompt = match system_prompt_for_mode_with_context_skills_and_session(
            AppMode::Agent,
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                user_memory_block: None,
                knowledge_prompt_block: None,
                goal_objective: None,
                project_context_pack_enabled: false,
                locale_tag: "en",
                translation_enabled: false,
                model_id: "codesmith",
                show_thinking: true,
                is_simple: false,
                skills_block: None,
            },
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        assert!(
            !prompt.contains("Memory Hygiene"),
            "memory guidance must not leak into sessions without a memory block"
        );
    }

    #[test]
    fn memory_guidance_appended_after_memory_block() {
        let tmp = tempdir().expect("tempdir");
        let block = "## User Memory\n\n- prefers Rust\n";
        let prompt = match system_prompt_for_mode_with_context_skills_and_session(
            AppMode::Agent,
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                user_memory_block: Some(block),
                knowledge_prompt_block: None,
                goal_objective: None,
                project_context_pack_enabled: false,
                locale_tag: "en",
                translation_enabled: false,
                model_id: "codesmith",
                show_thinking: true,
                is_simple: false,
                skills_block: None,
            },
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        let mem_at = prompt.find("User Memory").expect("user memory present");
        let guide_at = prompt.find("Memory Hygiene").expect("guidance present");
        assert!(
            mem_at < guide_at,
            "guidance must come after the user memory block"
        );
    }

    #[test]
    fn memory_guidance_matches_constitutional_tier_order() {
        let guidance = MEMORY_GUIDANCE
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let current_request_at = guidance
            .find("the user's current request (Tier 2)")
            .expect("current request tier present");
        let statutes_at = guidance
            .find("Statutes (Tier 3)")
            .expect("statutes tier present");
        let local_law_at = guidance
            .find("Local Law (Tier 5)")
            .expect("local law tier present");
        let live_evidence_at = guidance
            .find("live evidence (Tier 6)")
            .expect("live evidence tier present");

        assert!(
            current_request_at < statutes_at
                && statutes_at < local_law_at
                && local_law_at < live_evidence_at,
            "memory guidance must keep the current request above memory and local law"
        );
    }

    #[test]
    fn project_context_pack_can_be_disabled() {
        let tmp = tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("README.md"), "# Pack test").expect("write readme");
        let prompt = match system_prompt_for_mode_with_context_skills_and_session(
            AppMode::Agent,
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                user_memory_block: None,
                knowledge_prompt_block: None,
                goal_objective: None,
                project_context_pack_enabled: false,
                locale_tag: "en",
                translation_enabled: false,
                model_id: "codesmith",
                show_thinking: true,
                is_simple: false,
                skills_block: None,
            },
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        assert!(!prompt.contains("<project_context_pack>"));
    }

    #[test]
    fn project_context_pack_is_before_dynamic_tail() {
        let tmp = tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("README.md"), "# Pack test").expect("write readme");
        std::fs::create_dir_all(tmp.path().join(".deepseek")).expect("mkdir");
        std::fs::write(tmp.path().join(".deepseek").join("handoff.md"), "handoff")
            .expect("handoff");
        let prompt = match system_prompt_for_mode_with_context_skills_and_session(
            AppMode::Agent,
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                user_memory_block: None,
                knowledge_prompt_block: None,
                goal_objective: None,
                project_context_pack_enabled: true,
                locale_tag: "en",
                translation_enabled: false,
                model_id: "codesmith",
                show_thinking: true,
                is_simple: false,
                skills_block: None,
            },
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        assert!(prompt.contains("<project_context_pack>"));
        assert!(
            prompt.find("<project_context_pack>").expect("pack")
                < prompt.find("## Previous Session Relay").expect("relay")
        );
    }

    #[test]
    fn handoff_artifact_is_prepended_to_system_prompt_when_present() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();
        let handoff_dir = workspace.join(".deepseek");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("handoff.md"),
            "# Session relay — prior\n\n## Active task\nFinish #32.\n\n## Open blockers\n- [ ] write the basic version\n",
        )
        .unwrap();

        let prompt = match system_prompt_for_mode_with_context(AppMode::Agent, workspace, None) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };

        assert!(prompt.contains(HANDOFF_BLOCK_MARKER));
        assert!(prompt.contains("Finish #32."));
        assert!(prompt.contains("write the basic version"));
    }

    #[test]
    fn missing_handoff_does_not_inject_block() {
        let tmp = tempdir().expect("tempdir");
        let prompt = match system_prompt_for_mode_with_context(AppMode::Agent, tmp.path(), None) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        assert!(!prompt.contains(HANDOFF_BLOCK_MARKER));
    }

    #[test]
    fn empty_handoff_file_does_not_inject_block() {
        let tmp = tempdir().expect("tempdir");
        let dir = tmp.path().join(".deepseek");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("handoff.md"), "   \n\n  ").unwrap();
        let prompt = match system_prompt_for_mode_with_context(AppMode::Agent, tmp.path(), None) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        assert!(!prompt.contains(HANDOFF_BLOCK_MARKER));
    }

    #[test]
    fn compose_prompt_includes_all_layers() {
        let prompt = compose_prompt(AppMode::Agent, Personality::Calm);
        // Base layer
        assert!(prompt.contains("You are codesmith"));
        // Personality layer
        assert!(prompt.contains("Personality: Calm"));
        // Mode layer
        assert!(prompt.contains("Mode: Agent"));
        // Approval layer
        assert!(prompt.contains("Approval Policy: Suggest"));
    }

    /// Gate against shipping a release with a missing CHANGELOG entry — which
    /// is exactly what happened with v0.8.21 / v0.8.22 (entries had to be
    /// backfilled in v0.8.23). Asserts the top-of-file CHANGELOG contains a
    /// `## [X.Y.Z]` heading matching the current `CARGO_PKG_VERSION`. No
    /// hardcoded version string — the test self-updates with the workspace
    /// version bump and only fires when the CHANGELOG is the missing piece.
    ///
    /// Walks up from `CARGO_MANIFEST_DIR` to find `CHANGELOG.md` instead of
    /// assuming a fixed `../../CHANGELOG.md` layout. The workspace root is
    /// the common case, but the walk also tolerates deeper crate layouts and
    /// the packaged-crate case (where the workspace root has been stripped
    /// out): if no `CHANGELOG.md` is reachable, the gate quietly skips
    /// rather than panicking, so consumers running the suite outside the
    /// workspace checkout don't see a spurious failure.
    #[test]
    fn changelog_entry_exists_for_current_package_version() {
        let version = env!("CARGO_PKG_VERSION");
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(changelog_path) = manifest_dir
            .ancestors()
            .map(|dir| dir.join("CHANGELOG.md"))
            .find(|candidate| candidate.is_file())
        else {
            eprintln!(
                "changelog_entry_exists_for_current_package_version: no \
                 CHANGELOG.md found above {} — skipping (this gate only \
                 fires inside a workspace checkout).",
                manifest_dir.display()
            );
            return;
        };

        let contents = std::fs::read_to_string(&changelog_path).unwrap_or_else(|err| {
            panic!(
                "failed to read CHANGELOG.md at {}: {err}",
                changelog_path.display()
            )
        });
        let header = format!("## [{version}]");
        assert!(
            contents.contains(&header),
            "CHANGELOG.md is missing a `{header}` entry for the current package \
             version. Add a release section at the top before tagging — see \
             docs/RELEASE_CHECKLIST.md."
        );
    }

    #[test]
    fn compose_prompt_deterministic_order() {
        let prompt = compose_prompt(AppMode::Yolo, Personality::Calm);
        let base_pos = prompt.find("You are codesmith").unwrap();
        let personality_pos = prompt.find("Personality: Calm").unwrap();
        let mode_pos = prompt.find("Mode: YOLO").unwrap();
        let approval_pos = prompt.find("Approval Policy: Auto").unwrap();

        assert!(base_pos < personality_pos);
        assert!(personality_pos < mode_pos);
        assert!(mode_pos < approval_pos);
    }

    #[test]
    fn each_mode_gets_correct_approval() {
        assert!(
            compose_prompt(AppMode::Agent, Personality::Calm).contains("Approval Policy: Suggest")
        );
        assert!(compose_prompt(AppMode::Yolo, Personality::Calm).contains("Approval Policy: Auto"));
        assert!(
            compose_prompt(AppMode::Plan, Personality::Calm).contains("Approval Policy: Never")
        );
    }

    #[test]
    fn agent_prompt_can_reflect_never_approval_policy() {
        let prompt =
            compose_prompt_with_approval(AppMode::Agent, Personality::Calm, ApprovalMode::Never);
        assert!(prompt.contains("Mode: Agent"));
        assert!(prompt.contains("Approval Policy: Never"));
        assert!(prompt.contains("/config approval_mode suggest"));
    }

    #[test]
    fn personality_switches_correctly() {
        let calm = compose_prompt(AppMode::Agent, Personality::Calm);
        let playful = compose_prompt(AppMode::Agent, Personality::Playful);
        assert!(calm.contains("Personality: Calm"));
        assert!(playful.contains("Personality: Playful"));
        assert!(!calm.contains("Personality: Playful"));
    }

    #[test]
    fn compact_template_is_included_in_full_prompt() {
        let tmp = tempdir().expect("tempdir");
        let prompt = match system_prompt_for_mode_with_context(AppMode::Agent, tmp.path(), None) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        assert!(prompt.contains("## Compaction Relay"));
        // #429: structured Markdown template. Goal/Constraints/Progress
        // (Done/InProgress/Blocked)/Key Decisions/Next step.
        assert!(prompt.contains("### Goal"));
        assert!(prompt.contains("### Constraints"));
        assert!(prompt.contains("### Progress"));
        assert!(prompt.contains("#### Done"));
        assert!(prompt.contains("#### In Progress"));
        assert!(prompt.contains("#### Blocked"));
        assert!(prompt.contains("### Key Decisions"));
        assert!(prompt.contains("### Next step"));
    }

    #[test]
    fn session_goal_is_injected_below_compact_template() {
        let tmp = tempdir().expect("tempdir");
        let prompt = match system_prompt_for_mode_with_context_skills_and_session(
            AppMode::Agent,
            tmp.path(),
            Some("## Repo Working Set\nsrc/lib.rs"),
            None,
            None,
            PromptSessionContext {
                user_memory_block: None,
                knowledge_prompt_block: None,
                goal_objective: Some("Fix transcript corruption"),
                project_context_pack_enabled: true,
                locale_tag: "en",
                translation_enabled: false,
                model_id: "codesmith",
                show_thinking: true,
                is_simple: false,
                skills_block: None,
            },
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };

        let goal_pos = prompt.find("<session_goal>").expect("goal block");
        let compact_pos = prompt.find("## Compaction Relay").expect("compact block");

        assert!(prompt.contains("Fix transcript corruption"));
        // Session goal is volatile content — it lives below the
        // volatile-content boundary (after the compact template) so
        // per-session goal changes don't bust the prefix cache for
        // static layers.
        assert!(compact_pos < goal_pos);
        assert!(!prompt.contains("src/lib.rs"));
    }

    #[test]
    fn empty_session_goal_is_not_injected() {
        let tmp = tempdir().expect("tempdir");
        let prompt = match system_prompt_for_mode_with_context_skills_and_session(
            AppMode::Agent,
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                user_memory_block: None,
                knowledge_prompt_block: None,
                goal_objective: Some("   "),
                project_context_pack_enabled: true,
                locale_tag: "en",
                translation_enabled: false,
                model_id: "codesmith",
                show_thinking: true,
                is_simple: false,
                skills_block: None,
            },
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };

        assert!(!prompt.contains("<session_goal>"));
        assert!(!prompt.contains("## Current Hunt"));
    }

    #[test]
    fn tool_selection_guide_avoids_defensive_tool_suppression() {
        let prompt = compose_prompt(AppMode::Agent, Personality::Calm);
        assert!(prompt.contains("Tool Selection Guide"));
        assert!(prompt.contains("Use `agent_eval`"));
        assert!(
            !prompt.contains("When NOT to use certain tools"),
            "the system prompt should steer tool choice without training the model to avoid available tools"
        );
        assert!(
            !prompt.contains("Don't reach for"),
            "avoid defensive anti-tool wording in the base prompt"
        );
    }

    /// #588: language-mirroring directive must ship in every mode so
    /// DeepSeek's `reasoning_content` and final reply follow the user's
    /// language. Structural test — wording is not a test concern, but
    /// the cross-cutting commitment of #588 is specifically that the
    /// `reasoning_content` field tracks the user's language (not just
    /// the visible reply); pin that anchor token so a future edit
    /// can't silently weaken the section to a generic "respond in the
    /// user's language" directive while keeping the heading.
    #[test]
    fn language_mirroring_section_present_in_all_modes() {
        for mode in [AppMode::Agent, AppMode::Yolo, AppMode::Plan] {
            let prompt = compose_prompt(mode, Personality::Calm);
            assert!(
                prompt.contains("## Language"),
                "## Language section missing from mode {mode:?}"
            );
            assert!(
                prompt.contains("reasoning_content"),
                "## Language section in {mode:?} must mention `reasoning_content` — \
                 that field name is the structural anchor for the #588 commitment that \
                 internal reasoning, not just the visible reply, follows the user's language"
            );
        }
    }

    #[test]
    fn language_mirroring_prioritizes_latest_user_message_over_locale_default() {
        let prompt = compose_prompt(AppMode::Agent, Personality::Calm);
        assert!(
            prompt.contains("latest user message first"),
            "the language directive must choose the turn language from the user message before \
             falling back to the environment locale"
        );
        assert!(
            prompt.contains("If the latest user message is clearly English"),
            "English user text must not drift after non-English context"
        );
        assert!(
            prompt.contains("localized READMEs") && prompt.contains("tool results"),
            "file/tool context must not become a language signal"
        );
        assert!(
            prompt.contains("even when the `lang` field in `## Environment` is `en`"),
            "Chinese user text must override an English resolved locale for reasoning_content"
        );
        assert!(
            prompt.contains("Use the `lang` field only when"),
            "environment locale should be an ambiguity fallback, not the primary language source"
        );
    }

    #[test]
    fn english_base_prompt_avoids_native_script_language_priming() {
        let prompt = compose_prompt(AppMode::Agent, Personality::Calm);
        assert!(
            !contains_cjk(&prompt),
            "English base prompt should keep native-script reinforcement in locale bookends only"
        );
        assert!(
            !prompt.contains("multilingual coding agent"),
            "identity should not prime language switching; language belongs in the Language section"
        );
    }

    /// #358: rlm guidance was reframed from "first-class" to "specialty
    /// tool" — verify the structural markers are present so a future
    /// change doesn't silently remove the RLM section entirely.
    ///
    /// Don't assert on prose. If you wouldn't fail a code review for
    /// changing the wording, don't fail a test for it.
    #[test]
    fn rlm_specialty_tool_guidance_present() {
        let prompt = compose_prompt(AppMode::Agent, Personality::Calm);
        // Structural: the RLM heading must exist as a section anchor.
        assert!(prompt.contains("RLM — How to Use It"));
        // Structural: the word "rlm" must appear multiple times (tool
        // name, section heading, toolbox reference). Just verify the
        // lowercase form — exact wording is NOT a test concern.
        let rlm_count = prompt.to_lowercase().matches("rlm").count();
        assert!(
            rlm_count >= 5,
            "RLM guidance present: expected >= 5 mentions of 'rlm', got {rlm_count}"
        );
        assert!(
            !prompt.contains("When NOT to use RLM"),
            "RLM guidance should explain fit and verification without telling the model to avoid the tool"
        );
    }

    /// Tier 5 Local Law must explicitly cover `EngineConfig.instructions`
    /// files. Without this clause, embedders that inject instructions via the
    /// config field (rather than via the four hard-coded path conventions)
    /// get their files classified by path — and since those embedder-supplied
    /// paths aren't `AGENTS.md` / `CLAUDE.md` / `.codesmith/instructions.md` /
    /// `.deepseek/instructions.md`, the model defaults to treating their
    /// imperatives as Tier 7 Memory (the lowest tier per Article VII),
    /// overridable by a single user sentence.
    #[test]
    fn local_law_tier_covers_engine_config_instructions() {
        let prompt = compose_prompt(AppMode::Agent, Personality::Calm);
        assert!(
            prompt.contains("any file configured via `EngineConfig.instructions`"),
            "Tier 5 must explicitly cover EngineConfig.instructions so \
             embedder-injected instructions are not default-classified as Tier 7 Memory."
        );
    }

    #[test]
    fn workspace_orientation_guidance_present() {
        let prompt = compose_prompt(AppMode::Agent, Personality::Calm);
        assert!(prompt.contains("AGENTS.md"));
        assert!(prompt.contains("Local Law"));
        assert!(
            prompt.contains("CLAUDE.md"),
            "CLAUDE.md must be listed as a project instruction source"
        );
    }

    #[test]
    fn prompt_uses_persistent_agent_and_rlm_surface() {
        let prompt = compose_prompt(AppMode::Agent, Personality::Calm);
        for tool in [
            "agent_open",
            "agent_eval",
            "agent_close",
            "rlm_open",
            "rlm_eval",
            "rlm_configure",
            "rlm_close",
            "handle_read",
        ] {
            assert!(
                prompt.contains(tool),
                "prompt should mention new persistent tool `{tool}`"
            );
        }
        for retired in [
            "agent_spawn",
            "agent_wait",
            "agent_result",
            "agent_send_input",
            "agent_assign",
            "agent_resume",
            "agent_list",
            "spawn_agent",
            "delegate_to_agent",
            "send_input",
            "close_agent",
        ] {
            assert!(
                !prompt.contains(retired),
                "prompt should not advertise retired sub-agent tool `{retired}`"
            );
        }
    }

    #[test]
    fn prompt_documents_fork_context_prefix_cache_contract() {
        let prompt = compose_prompt(AppMode::Agent, Personality::Calm);
        assert!(prompt.contains("fork_context: true"));
        assert!(prompt.contains("byte-identical"));
        assert!(prompt.contains("DeepSeek prefix-cache reuse"));
        assert!(prompt.contains("Fresh sessions are the default"));
    }

    #[test]
    fn subagent_done_sentinel_section_present() {
        let prompt = compose_prompt(AppMode::Agent, Personality::Calm);
        assert!(prompt.contains("Internal Sub-agent Completion Events"));
        assert!(prompt.contains("<codesmith:subagent.done>"));
        assert!(prompt.contains("not user input"));
        assert!(prompt.contains("Integration protocol"));
        assert!(prompt.contains("Do not tell the user they pasted sentinels"));
    }

    #[test]
    fn preamble_rhythm_section_present() {
        let prompt = compose_prompt(AppMode::Agent, Personality::Calm);
        // Preamble rhythm is now part of the Calm personality overlay.
        // Verify the load-bearing guidance is still present.
        assert!(prompt.contains("In preambles, name the action"));
        assert!(prompt.contains("Reading the module tree"));
    }

    #[test]
    fn legacy_constants_still_available() {
        // Verify the legacy .txt constant still compiles and contains expected content
        assert!(AGENT_PROMPT.lines().next().is_some());
    }

    // ── Cache-prefix stability harness (#263 step 2) ───────────────────────
    //
    // These tests pin the byte-stability invariant required for DeepSeek's
    // KV prefix cache to hit: any prompt-construction surface that ends up
    // in the cached prefix must produce identical bytes given identical
    // inputs across calls.

    use crate::test_support::assert_byte_identical;

    #[test]
    fn compose_prompt_is_byte_stable_across_calls() {
        // Suspect #4 from #263: mode prompt churn within a single mode.
        // Two calls with identical (mode, personality) inputs must produce
        // identical bytes — anything else is a cache buster.
        for mode in [AppMode::Agent, AppMode::Yolo, AppMode::Plan] {
            for personality in [Personality::Calm, Personality::Playful] {
                let a = compose_prompt(mode, personality);
                let b = compose_prompt(mode, personality);
                assert_byte_identical(
                    &format!("compose_prompt(mode={mode:?}, personality={personality:?})"),
                    &a,
                    &b,
                );
            }
        }
    }

    #[test]
    fn system_prompt_for_mode_with_context_is_byte_stable_for_unchanged_workspace() {
        // Same workspace, no working_set / skills churn between calls →
        // identical bytes. This pins the most representative production
        // surface (engine.rs builds the system prompt via this fn or
        // its sibling _and_skills variant on every turn).
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();

        for mode in [AppMode::Agent, AppMode::Yolo, AppMode::Plan] {
            let a = match system_prompt_for_mode_with_context(mode, workspace, None) {
                SystemPrompt::Text(text) => text,
                SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
            };
            let b = match system_prompt_for_mode_with_context(mode, workspace, None) {
                SystemPrompt::Text(text) => text,
                SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
            };
            assert_byte_identical(
                &format!("system_prompt_for_mode_with_context(mode={mode:?}) on empty workspace"),
                &a,
                &b,
            );
        }
    }

    #[test]
    fn system_prompt_ignores_working_set_summary_argument() {
        // Working-set metadata is now injected into the latest user message
        // per turn. The legacy argument remains for call-site compatibility
        // but must not reintroduce volatile bytes into the system prompt.
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();
        let summary = "## Repo Working Set\nWorkspace: /tmp/x\n";

        let a = match system_prompt_for_mode_with_context(AppMode::Agent, workspace, Some(summary))
        {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        let b = match system_prompt_for_mode_with_context(AppMode::Agent, workspace, Some(summary))
        {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        assert_byte_identical(
            "system_prompt_for_mode_with_context with constant working_set summary",
            &a,
            &b,
        );
        assert!(
            !a.contains(summary),
            "summary must not be embedded in system prompt"
        );
    }

    #[test]
    fn system_prompt_with_handoff_file_is_byte_stable_when_file_is_unchanged() {
        // If `.deepseek/handoff.md` hasn't moved between two builds, the
        // rendered prompt must produce identical bytes. The relay block
        // lands below the static boundary in
        // `system_prompt_for_mode_with_context_and_skills`.
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();
        let handoff_dir = workspace.join(".deepseek");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("handoff.md"),
            "# Session relay\n\n## Active task\nFinish #280.\n\n## Open blockers\n- [ ] none\n",
        )
        .unwrap();

        let a = match system_prompt_for_mode_with_context(AppMode::Agent, workspace, None) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        let b = match system_prompt_for_mode_with_context(AppMode::Agent, workspace, None) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };
        assert_byte_identical(
            "system_prompt_for_mode_with_context with constant handoff file",
            &a,
            &b,
        );
        assert!(a.contains(HANDOFF_BLOCK_MARKER), "relay must be embedded");
        assert!(a.contains("Finish #280."), "relay body must be present");
    }

    #[test]
    fn handoff_appears_after_static_blocks_without_working_set() {
        // Cache-prefix invariant: the relay block must come after static
        // `## Context Management` and the compaction relay template
        // (`## Compaction Relay`). Working-set metadata is per-turn user
        // metadata now, not a system-prompt tail block.
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();
        let handoff_dir = workspace.join(".deepseek");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(handoff_dir.join("handoff.md"), "# handoff body\n").unwrap();

        let summary = "## Repo Working Set\nWorkspace: /tmp/x\n";
        let prompt =
            match system_prompt_for_mode_with_context(AppMode::Agent, workspace, Some(summary)) {
                SystemPrompt::Text(text) => text,
                SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
            };

        let context_pos = prompt
            .find("## Context Management")
            .expect("Context Management section present in Agent mode");
        let compact_pos = prompt
            .find("## Compaction Relay")
            .expect("compaction relay template present");
        let handoff_pos = prompt
            .find(HANDOFF_BLOCK_MARKER)
            .expect("relay block present when fixture file exists");
        assert!(
            !prompt.contains("## Repo Working Set"),
            "working-set summary must stay out of the system prompt"
        );

        assert!(
            context_pos < handoff_pos,
            "## Context Management must precede the relay block"
        );
        assert!(
            compact_pos < handoff_pos,
            "## Compaction Relay must precede the relay block"
        );
    }

    #[test]
    fn render_instructions_block_returns_none_for_empty_input() {
        let empty: &[super::InstructionSource] = &[];
        assert!(super::render_instructions_block(empty).is_none());
    }

    #[test]
    fn render_instructions_block_skips_missing_files_with_warning() {
        let tmp = tempdir().expect("tempdir");
        let real = tmp.path().join("real.md");
        std::fs::write(&real, "real content here").unwrap();
        let bogus = tmp.path().join("does-not-exist.md");

        let block = super::render_instructions_block(&[bogus.clone().into(), real.clone().into()])
            .expect("present file should produce a block");
        assert!(block.contains("real content here"));
        assert!(block.contains(&real.display().to_string()));
        // Bogus path is skipped, not rendered.
        assert!(!block.contains(&bogus.display().to_string()));
    }

    #[test]
    fn render_instructions_block_concatenates_in_declared_order() {
        let tmp = tempdir().expect("tempdir");
        let a = tmp.path().join("a.md");
        let b = tmp.path().join("b.md");
        std::fs::write(&a, "ALPHA_MARKER").unwrap();
        std::fs::write(&b, "BRAVO_MARKER").unwrap();

        let block = super::render_instructions_block(&[a.into(), b.into()]).expect("non-empty");
        let alpha_pos = block.find("ALPHA_MARKER").expect("alpha rendered");
        let bravo_pos = block.find("BRAVO_MARKER").expect("bravo rendered");
        assert!(
            alpha_pos < bravo_pos,
            "instructions must concatenate in declared order"
        );
    }

    #[test]
    fn render_instructions_block_skips_empty_files() {
        let tmp = tempdir().expect("tempdir");
        let empty = tmp.path().join("empty.md");
        let real = tmp.path().join("real.md");
        std::fs::write(&empty, "   \n   \n").unwrap();
        std::fs::write(&real, "real content").unwrap();

        let block =
            super::render_instructions_block(&[empty.into(), real.into()]).expect("non-empty");
        // Empty file produces no `<instructions>` section, only the real one.
        let count = block.matches("<instructions").count();
        assert_eq!(count, 1, "only the non-empty file should produce a section");
    }

    #[test]
    fn render_instructions_block_truncates_oversize_files() {
        let tmp = tempdir().expect("tempdir");
        let big = tmp.path().join("big.md");
        // 200 KiB of content — well above the 100 KiB cap.
        std::fs::write(&big, "X".repeat(200 * 1024)).unwrap();

        let block = super::render_instructions_block(&[big.into()]).expect("non-empty");
        assert!(block.contains("[…elided]"), "truncation marker missing");
        // Block should be much smaller than the original file.
        assert!(
            block.len() < 110 * 1024,
            "block should be capped near 100 KiB"
        );
    }

    /// `InstructionSource::Inline` bypasses disk reads — the content is used
    /// directly and `name` becomes the `<instructions source="…">` attribute.
    /// Empty / oversize handling mirrors `File` variant.
    #[test]
    fn render_instructions_block_handles_inline_source() {
        let block = super::render_instructions_block(&[super::InstructionSource::Inline {
            name: "embedded:test/template".to_string(),
            content: "INLINE_MARKER_CONTENT".to_string(),
        }])
        .expect("non-empty");
        assert!(block.contains("INLINE_MARKER_CONTENT"));
        assert!(block.contains("source=\"embedded:test/template\""));

        // Empty inline → skipped just like empty file.
        let empty_inline = super::InstructionSource::Inline {
            name: "empty".to_string(),
            content: "   ".to_string(),
        };
        assert!(super::render_instructions_block(&[empty_inline]).is_none());

        // Oversize inline → truncated with elided marker.
        let big_inline = super::InstructionSource::Inline {
            name: "huge".to_string(),
            content: "Y".repeat(200 * 1024),
        };
        let trimmed = super::render_instructions_block(&[big_inline]).expect("non-empty");
        assert!(trimmed.contains("[…elided]"));

        // File + Inline 混用,顺序保持。
        let tmp = tempdir().expect("tempdir");
        let file_path = tmp.path().join("file-first.md");
        std::fs::write(&file_path, "FILE_MARKER").unwrap();
        let mixed = super::render_instructions_block(&[
            file_path.into(),
            super::InstructionSource::Inline {
                name: "inline-second".to_string(),
                content: "INLINE_MARKER".to_string(),
            },
        ])
        .expect("non-empty");
        let file_pos = mixed.find("FILE_MARKER").expect("file rendered");
        let inline_pos = mixed.find("INLINE_MARKER").expect("inline rendered");
        assert!(file_pos < inline_pos, "声明顺序必须保留(File then Inline)");
    }

    #[test]
    fn instructions_block_appears_in_system_prompt_when_configured() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();
        let extra = workspace.join("extra-instructions.md");
        std::fs::write(&extra, "EXTRA_INSTRUCTIONS_MARKER_BODY").unwrap();

        let extra_source: super::InstructionSource = extra.clone().into();
        let prompt = match super::system_prompt_for_mode_with_context_and_skills(
            AppMode::Agent,
            workspace,
            None,
            None,
            Some(std::slice::from_ref(&extra_source)),
            None,
            None,
        ) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(_) => panic!("expected text system prompt"),
        };

        assert!(
            prompt.contains("EXTRA_INSTRUCTIONS_MARKER_BODY"),
            "configured instructions file body must appear in the prompt"
        );
        assert!(
            prompt.contains(&extra.display().to_string()),
            "instructions block must annotate its source path"
        );
    }
}
