#![allow(dead_code)]
//! System prompts for different modes.
//!
//! Prompts are assembled from composable layers loaded at compile time:
//!   tool taxonomy → base.md → personality overlay → mode delta → approval policy
//!
//! This keeps each concern in its own file and makes prompt tuning
//! a single-file operation.

use crate::mode::AppMode;
use crate::mode::ApprovalMode;
use crate::models::SystemPrompt;
use crate::project_context::{ProjectContext, load_project_context_with_parents};
use crate::prompt_runtime::{
    EffectiveSystemPromptInput, PromptBundle, PromptCachePolicy, PromptSection,
    PromptSectionSource, PromptSectionStability, build_effective_system_prompt,
};
pub use crate::prompt_sources::{InstructionSource, PromptAppendSource};
use std::fmt::Write as _;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct PromptSessionContext<'a> {
    pub user_memory_block: Option<&'a str>,
    /// KoD knowledge block — when set, replaces `user_memory_block` in the
    /// system prompt assembly. Contains the MEMORY.md entrypoint wrapped
    /// in `<knowledge_memory>` with type taxonomy guidance.
    pub knowledge_prompt_block: Option<&'a str>,
    pub goal_objective: Option<&'a str>,
    pub project_context_pack_enabled: bool,
    /// Resolved BCP-47 locale tag for the `## Environment` block in
    /// the system prompt (e.g. `"en"`, `"zh-Hans"`, `"ja"`). The
    /// caller is responsible for resolving this from `Settings`; no
    /// disk I/O happens inside the prompt builder, so the workspace-
    /// static portion of the system prompt stays cache-friendly.
    pub locale_tag: &'a str,
    /// When true, a ## Language Output Requirement block is appended
    /// to the system prompt instructing the model to respond in
    /// the resolved session locale.
    pub translation_enabled: bool,
    /// Active model identifier injected into the Constitutional
    /// preamble ("You are {model_id}, running inside CodeSmith").
    /// Defaults to `"codesmith"` when the caller doesn't supply one,
    /// preserving backward compatibility with existing call sites
    /// that predate dynamic model injection.
    pub model_id: &'a str,
    /// Whether the user-visible transcript renders thinking blocks.
    /// When false, the prompt should not spend localization pressure on
    /// `reasoning_content` the user will never see.
    pub show_thinking: bool,
    /// When true, a `## Conversation Style: Simple` block is appended to the
    /// system prompt instructing the model to answer in maximum-compression
    /// "caveman" style — short sentences, no filler — while keeping code,
    /// commands, and error messages byte-exact.
    pub is_simple: bool,
    /// Pre-rendered `## Skills` block. The caller resolves this from the
    /// workspace/skills directories via `crate::skills::render_available_skills_context*`
    /// so the prompt builder stays free of skills-discovery dependencies
    /// (and portable across crates). `None` when no skills are available.
    pub skills_block: Option<String>,
    /// Personality overlay — voice and tone only, never behavior. Resolved
    /// from the `personality` config key; call sites predate it default to
    /// [`Personality::Calm`].
    pub personality: Personality,
}

impl<'a> PromptSessionContext<'a> {
    pub fn runtime(self) -> PromptRuntimeContext<'a> {
        PromptRuntimeContext {
            session: self,
            override_system_prompt: None,
            custom_system_prompt: None,
            coordinator_system_prompt: None,
            agent_system_prompt: None,
            append_system_prompts: &[],
            cache_breaker: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PromptRuntimeContext<'a> {
    pub session: PromptSessionContext<'a>,
    /// Optional complete system prompt override. This replaces the default
    /// assembled section bundle while still allowing append sections below.
    pub override_system_prompt: Option<&'a str>,
    /// Optional custom system prompt. Lower priority than override/role-specific
    /// prompts; append sections still apply.
    pub custom_system_prompt: Option<&'a str>,
    /// Optional role-specific coordinator prompt override.
    pub coordinator_system_prompt: Option<&'a str>,
    /// Optional role-specific agent prompt override.
    pub agent_system_prompt: Option<&'a str>,
    /// Extra append sections rendered after the selected prompt base.
    pub append_system_prompts: &'a [PromptAppendSource],
    /// Optional dynamic cache breaker for debugging provider prefix behavior.
    pub cache_breaker: Option<&'a str>,
}

impl<'a> Default for PromptRuntimeContext<'a> {
    fn default() -> Self {
        PromptSessionContext::default().runtime()
    }
}

impl Default for PromptSessionContext<'_> {
    fn default() -> Self {
        Self {
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
            personality: Personality::Calm,
        }
    }
}

/// Conventional location for the structured session relay artifact (#32).
/// A previous session writes it on exit / `/compact`; the next session reads
/// it back on startup and prepends it to the system prompt so a fresh agent
/// doesn't have to re-discover open blockers from scratch.
pub const HANDOFF_RELATIVE_PATH: &str = ".codesmith/handoff.md";

/// Per-file size cap for `instructions = [...]` entries (#454). Mirrors
/// the existing project-context cap in `project_context::load_context_file`
/// so a malicious / oversized include can't blow the prompt budget on
/// its own. Files larger than this are truncated with an `[…elided]`
/// marker rather than skipped entirely so the model still sees the head.
pub const INSTRUCTIONS_FILE_MAX_BYTES: usize = 100 * 1024;

/// System prompt block appended when `translation_enabled` is true.
/// Instructs the model to respond in the resolved session locale for all
/// natural-language output — explanations, summaries, conversation.
/// Code identifiers, untranslatable technical terms, and explicitly
/// requested English code blocks are exempt.
pub fn translation_output_instruction(locale_tag: &str) -> String {
    let target_language = translation_target_language_for_tag(locale_tag);
    format!(
        "\
## Language Output Requirement\n\
\n\
The user requires all responses in {target_language}. \
Always respond in {target_language} — use natural, professional language for all \
explanations, code comments, summaries, and conversational turns. \
Only output English for:\n\
- Code identifiers (variable names, function names, file paths)\n\
- Technical terms that lack a standard translation in {target_language}\n\
- Code blocks the user explicitly requests in English\n\n\
This is a hard display requirement: the user does not read English, \
so any English prose in your response will block their decision-making."
    )
}

pub fn translation_target_language_for_tag(locale_tag: &str) -> &'static str {
    let normalized = locale_tag.trim().to_ascii_lowercase();
    if normalized.starts_with("ja") {
        "Japanese (日本語)"
    } else if normalized.starts_with("zh-hant")
        || normalized.contains("-tw")
        || normalized.contains("-hk")
        || normalized.contains("-mo")
    {
        "Traditional Chinese (繁體中文)"
    } else if normalized.starts_with("zh") {
        "Simplified Chinese (简体中文)"
    } else if normalized.starts_with("pt") {
        "Brazilian Portuguese (Português do Brasil)"
    } else if normalized.starts_with("vi") {
        "Vietnamese (Tiếng Việt)"
    } else {
        "English"
    }
}

pub fn hidden_thinking_language_instruction(locale_tag: &str) -> String {
    let fallback_language = translation_target_language_for_tag(locale_tag);
    format!(
        "\
## Hidden Thinking Language\n\
\n\
The user has disabled thinking display (`show_thinking = false`). If you emit \
`reasoning_content`, keep that hidden internal thinking in English regardless \
of the latest user-message language or `## Environment.lang`; the user will \
not see it, so localizing hidden thinking only adds language switching.\n\
\n\
The final reply is still user-visible. Follow the normal `## Language` rule \
for the final reply: mirror the latest user message, and use \
{fallback_language} only when the user message is ambiguous. If the user \
explicitly asks for a different thinking language, follow that explicit request \
for the current turn."
    )
}

/// Render a `## Environment` block listing the resolved locale tag,
/// runtime version, host platform, login shell, and current working directory.
///
/// The block is appended to the workspace-static portion of the
/// system prompt (after mode prompt + project context, before
/// configured instructions / skills) so the `## Language` directive
/// in `prompts/base.md` can reference it without the model having to
/// guess from the user's first message. `locale_tag` is resolved by
/// the caller from `Settings` so this function stays I/O-free.
pub fn render_environment_block(workspace: &Path, locale_tag: &str) -> String {
    let codesmith_version = env!("CARGO_PKG_VERSION");
    let platform = std::env::consts::OS;
    let shell = crate::shell_dispatcher::global_dispatcher()
        .kind()
        .binary()
        .to_string();
    let pwd = workspace.display();

    format!(
        "## Environment\n\
         \n\
         - lang: {locale_tag}\n\
         - codesmith_version: {codesmith_version}\n\
         - platform: {platform}\n\
         - shell: {shell}\n\
         - pwd: {pwd}"
    )
}

/// Render the `instructions = [...]` config array as a single
/// system-prompt block (#454). Each source is processed in declared order;
/// missing `File` sources are skipped with a tracing warning so a stale entry
/// doesn't fail the launch. Empty input (or all sources missing/empty)
/// returns `None` so callers append nothing.
pub fn render_instructions_block(sources: &[InstructionSource]) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();
    for source in sources {
        let (raw_source_name, raw_content): (String, String) = match source {
            InstructionSource::File(path) => match std::fs::read_to_string(path) {
                Ok(raw) => (path.display().to_string(), raw),
                Err(err) => {
                    tracing::warn!(
                        target: "instructions",
                        ?err,
                        ?path,
                        "skipping unreadable instructions file"
                    );
                    continue;
                }
            },
            InstructionSource::Inline { name, content } => (name.clone(), content.clone()),
        };
        let trimmed = raw_content.trim();
        if trimmed.is_empty() {
            continue;
        }
        let body = if trimmed.len() > INSTRUCTIONS_FILE_MAX_BYTES {
            let head_end = (0..=INSTRUCTIONS_FILE_MAX_BYTES)
                .rev()
                .find(|&i| trimmed.is_char_boundary(i))
                .unwrap_or(0);
            format!("{}\n[…elided]", &trimmed[..head_end])
        } else {
            trimmed.to_string()
        };
        sections.push(format!(
            "<instructions source=\"{raw_source_name}\">\n{body}\n</instructions>"
        ));
    }
    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

/// Read the workspace-local relay artifact, if present, and format it as a
/// system-prompt block. Returns `None` when the file is absent or empty so
/// callers can keep the default-uncluttered prompt for fresh workspaces.
pub fn load_handoff_block(workspace: &Path) -> Option<String> {
    let path = workspace.join(HANDOFF_RELATIVE_PATH);
    let raw = std::fs::read_to_string(&path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!(
        "## Previous Session Relay\n\nThe previous session in this workspace left a relay artifact at `{HANDOFF_RELATIVE_PATH}`. Consider it the first artifact to read on this turn — open blockers, in-flight changes, and recent decisions live there. Update or rewrite it before exiting if state changes materially.\n\n{trimmed}"
    ))
}

// ── Prompt layers loaded at compile time ──────────────────────────────

/// Core: task execution, tool-use rules, output format, toolbox reference,
/// "When NOT to use" guidance, sub-agent sentinel protocol.
pub const BASE_PROMPT: &str = include_str!("prompts/base.md");

/// Conversation-style overlay appended when `PromptSessionContext::is_simple`
/// is true (the `is_simple` user setting). Presentation-only: compressed
/// sentences, byte-exact technical content.
pub const SIMPLE_CONVERSATION_STYLE: &str = include_str!("prompts/styles/simple.md");

// ── Embedder prompt overrides ──
// Let an embedder replace these compile-time prompt constants at startup,
// so brand / slimming customizations live in the embedder crate instead of
// editing these files in-tree. Unset → the bundled constant (fully
// backward compatible). Intended to be set once at process start, before
// any engine spawns; later sets return the rejected override string.
static BASE_PROMPT_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_PREAMBLE_ZH_HANS_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_PREAMBLE_JA_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_PREAMBLE_PT_BR_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_PREAMBLE_VI_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_CLOSER_ZH_HANS_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_CLOSER_JA_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_CLOSER_PT_BR_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_CLOSER_VI_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static AUTHORITY_RECAP_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Replace `BASE_PROMPT` for all subsequent prompt composition. First call
/// wins; later calls return the rejected string. Set before spawning any
/// engine.
pub fn set_base_prompt_override(s: String) -> Result<(), String> {
    set_prompt_override(&BASE_PROMPT_OVERRIDE, s)
}

/// Replace the Simplified-Chinese locale preamble (`## 语言要求`).
pub fn set_locale_preamble_zh_hans_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_PREAMBLE_ZH_HANS_OVERRIDE, s)
}

/// Replace the Japanese locale preamble.
pub fn set_locale_preamble_ja_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_PREAMBLE_JA_OVERRIDE, s)
}

/// Replace the Brazilian-Portuguese locale preamble.
pub fn set_locale_preamble_pt_br_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_PREAMBLE_PT_BR_OVERRIDE, s)
}

/// Replace the Vietnamese locale preamble.
pub fn set_locale_preamble_vi_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_PREAMBLE_VI_OVERRIDE, s)
}

/// Replace the Simplified-Chinese locale closer (`## 语言再次提醒`).
pub fn set_locale_closer_zh_hans_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_CLOSER_ZH_HANS_OVERRIDE, s)
}

/// Replace the Japanese locale closer.
pub fn set_locale_closer_ja_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_CLOSER_JA_OVERRIDE, s)
}

/// Replace the Brazilian-Portuguese locale closer.
pub fn set_locale_closer_pt_br_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_CLOSER_PT_BR_OVERRIDE, s)
}

/// Replace the Vietnamese locale closer.
pub fn set_locale_closer_vi_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_CLOSER_VI_OVERRIDE, s)
}

/// Replace the trailing `## Authority Recap` block.
pub fn set_authority_recap_override(s: String) -> Result<(), String> {
    set_prompt_override(&AUTHORITY_RECAP_OVERRIDE, s)
}

pub fn set_prompt_override(cell: &std::sync::OnceLock<String>, s: String) -> Result<(), String> {
    cell.set(s)
}

pub fn effective_prompt_override<'a>(
    cell: &'a std::sync::OnceLock<String>,
    fallback: &'static str,
) -> &'a str {
    cell.get().map(String::as_str).unwrap_or(fallback)
}

pub fn effective_base_prompt() -> &'static str {
    effective_prompt_override(&BASE_PROMPT_OVERRIDE, BASE_PROMPT)
}

pub fn effective_locale_preamble_zh_hans() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_ZH_HANS_OVERRIDE, LOCALE_PREAMBLE_ZH_HANS)
}

pub fn effective_locale_preamble_ja() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_JA_OVERRIDE, LOCALE_PREAMBLE_JA)
}

pub fn effective_locale_preamble_pt_br() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_PT_BR_OVERRIDE, LOCALE_PREAMBLE_PT_BR)
}

pub fn effective_locale_preamble_vi() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_VI_OVERRIDE, LOCALE_PREAMBLE_VI)
}

pub fn effective_locale_closer_zh_hans() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_ZH_HANS_OVERRIDE, LOCALE_CLOSER_ZH_HANS)
}

pub fn effective_locale_closer_ja() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_JA_OVERRIDE, LOCALE_CLOSER_JA)
}

pub fn effective_locale_closer_pt_br() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_PT_BR_OVERRIDE, LOCALE_CLOSER_PT_BR)
}

pub fn effective_locale_closer_vi() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_VI_OVERRIDE, LOCALE_CLOSER_VI)
}

pub fn effective_authority_recap() -> &'static str {
    effective_prompt_override(&AUTHORITY_RECAP_OVERRIDE, AUTHORITY_RECAP)
}

/// Optional locale-native reinforcement preamble prepended to the system
/// prompt when the user's UI locale is non-English.
///
/// `base.md` itself stays English (single source of truth, model is
/// natively multilingual, prefix-cache stable across users in the same
/// locale). For non-English locales we prepend a short locale-native
/// passage so the model's first exposure to the prompt overrides the
/// "match user message language" English directive with an explicit
/// "use {locale}" instruction in the user's own writing system. Reduces
/// the model's reliance on inferring intent from `## Environment.lang`
/// — which previously got overpowered by overwhelmingly English task
/// context, the symptom reported in #1118 and visible in the WeChat
/// screenshot that prompted this change.
///
/// The list is intentionally short (only locales the TUI ships UI
/// strings for: `zh-Hans`, `ja`, `pt-BR`). Other locales fall through
/// to `None` and get the English-only directive, which is the same
/// behavior as before this change.
///
/// ## Design philosophy: why a bookend, not a full translation
///
/// Community feedback on the WeChat thread that prompted this work
/// pointed out — correctly — that DeepSeek V4 is a Chinese-first
/// multilingual model, not an English-only model with multilingual
/// veneer. Its tokenizer is co-trained on Chinese; `你好` typically
/// encodes to ~1 token, not 2 — the "Chinese is expensive in tokens"
/// folk wisdom from Western-LLM commentary doesn't apply here.
///
/// The naïve translation of that argument would be: ship a fully
/// translated `base.md` per locale. We deliberately stop short of
/// that for v0.8.29. The reasons, ranked:
///
///   1. **Drift risk.** A 200+ line technical prompt has subtle
///      phrasing that drives subtle behavior. Every rule change has
///      to land in N translated copies, kept in lockstep. The class
///      of bug that arises (Chinese users see slightly different
///      agent behavior than English users) is hard to reproduce and
///      hard to triage from bug reports.
///   2. **Cache stability.** With one English `base.md` and a
///      per-locale preamble+closer, the largest cacheable chunk
///      (mode prompt + project context + environment) stays
///      byte-stable within a session and across users in the same
///      locale. A fully translated per-locale `base.md` keeps cache
///      per-locale but doesn't share with English users.
///   3. **Translation QA is expensive.** Each prompt-language pair
///      needs a native speaker reviewing tone, register, and rule
///      preservation. Getting it 95% right is bad, because the
///      missing 5% becomes silent behavior divergence.
///
/// What we DO instead — the bookend pattern @MuMu described from
/// their other project — is reinforce the locale directive in
/// native script at BOTH ends of the prompt. The opening anchors
/// behavior at session start; the closing reinforcement
/// (`locale_reinforcement_closer`) sits at the maximum-recency
/// position right before the user's next message. Empirically this
/// is sufficient to keep `reasoning_content` in the target locale
/// even as English code accumulates in context turn-over-turn.
///
/// If at some future point the bookend proves insufficient — or if
/// the maintenance cost of per-locale `base.md` files becomes
/// preferable to whatever's blocking it — full translation is the
/// natural next step. The locale tags here, the test invariants,
/// and the closer position would all carry over unchanged.
pub fn locale_reinforcement_preamble(locale_tag: &str) -> Option<&'static str> {
    match locale_tag {
        "zh-Hans" | "zh-CN" | "zh" => Some(effective_locale_preamble_zh_hans()),
        "ja" | "ja-JP" => Some(effective_locale_preamble_ja()),
        "pt-BR" | "pt" => Some(effective_locale_preamble_pt_br()),
        "vi" | "vi-VN" => Some(effective_locale_preamble_vi()),
        _ => None,
    }
}

/// Locale-native closing reinforcement appended to the very end of the
/// system prompt — the bookend MuMu described in the WeChat thread that
/// prompted #1118 follow-up work.
///
/// The opening preamble alone is not enough: as the model accumulates
/// English context turn-over-turn (code, error logs, search results,
/// file listings), the recency bias of the transformer's attention
/// drifts thinking back toward English even when the user keeps writing
/// in their own language. A closing native-script reinforcement sits at
/// the position closest to the user's next message — where attention
/// weight is highest — and re-asserts the language rule right before
/// the model generates `reasoning_content` for the turn.
///
/// Like the opening preamble, English (and unknown) locales return
/// `None` and the system prompt is byte-identical to the pre-bookend
/// behavior.
pub fn locale_reinforcement_closer(locale_tag: &str) -> Option<&'static str> {
    match locale_tag {
        "zh-Hans" | "zh-CN" | "zh" => Some(effective_locale_closer_zh_hans()),
        "ja" | "ja-JP" => Some(effective_locale_closer_ja()),
        "pt-BR" | "pt" => Some(effective_locale_closer_pt_br()),
        "vi" | "vi-VN" => Some(effective_locale_closer_vi()),
        _ => None,
    }
}

pub const LOCALE_PREAMBLE_ZH_HANS: &str = "## 语言要求\n\n\
你正在 codesmith 中运行。无论任务上下文（代码、错误日志、文件名）\
是英文，无论系统提示的其余部分是英文，你都必须用简体中文进行 \
`reasoning_content`（内部思考）和最终回复。代码、文件路径、工具名称\
（例如 `read_file`、`exec_shell`）、环境变量、命令行参数和 URL \
保持原样 —— 只有自然语言散文要切换到简体中文。\n\n\
如果用户在会话中切换到另一种语言，从下一轮开始跟随切换。\
如果用户明确要求（例如 \"think in English\"），则覆盖此规则。";

pub const LOCALE_PREAMBLE_JA: &str = "## 言語要件\n\n\
codesmith を実行しています。タスクコンテキスト（コード、エラーログ、\
ファイル名）が英語であっても、システムプロンプトの他の部分が英語で\
あっても、`reasoning_content`（内部思考）と最終的な返信は日本語で\
行ってください。コード、ファイルパス、ツール名（例：`read_file`、\
`exec_shell`）、環境変数、コマンドライン引数、URL は元のまま —— \
自然言語の文章のみ日本語に切り替えます。\n\n\
ユーザーがセッション中に別の言語に切り替えた場合は、次のターンから\
それに従ってください。ユーザーが明示的に要求した場合（例：\
\"think in English\"）はこのルールを上書きします。";

pub const LOCALE_PREAMBLE_PT_BR: &str = "## Requisito de Idioma\n\n\
Você está rodando dentro do codesmith. Escreva tanto \
`reasoning_content` (seu pensamento interno) quanto a resposta final \
em português do Brasil, mesmo quando o contexto da tarefa (código, \
logs de erro, nomes de arquivos) estiver em inglês e mesmo quando o \
resto do system prompt for em inglês. Mantenha código, caminhos de \
arquivos, nomes de ferramentas (por exemplo `read_file`, \
`exec_shell`), variáveis de ambiente, flags de linha de comando e \
URLs no formato original — apenas a prosa em linguagem natural muda \
para português do Brasil.\n\n\
Se o usuário mudar de idioma no meio da sessão, mude no próximo turno. \
Se o usuário pedir explicitamente (por exemplo, \"think in English\"), \
isso sobrescreve esta regra.";

// ── Closing bookends (appended to the very end of the system prompt) ──

pub const LOCALE_CLOSER_ZH_HANS: &str = "## 语言再次提醒\n\n\
**重要：你的 `reasoning_content`（内部思考）和最终回复必须保持简体中文。** \
无论你在这次会话中读到了多少英文代码、错误日志或文档，无论项目上下文 \
是英文，思考过程不能漂移到英文。这是会话级硬性要求 —— 用户的语言决定 \
你的语言，与上下文中累积的英文内容无关。除非用户明确要求切换（例如 \
\"think in English\"），否则继续用简体中文思考和回答。";

pub const LOCALE_CLOSER_JA: &str = "## 言語再確認\n\n\
**重要：`reasoning_content`（内部思考）と最終的な返信は日本語で行ってください。** \
このセッションで読み込んだ英語のコード、エラーログ、ドキュメントの量に \
関係なく、プロジェクトコンテキストが英語であっても、思考プロセスを \
英語に逸らさないでください。これはセッションレベルの厳格な要件であり、 \
ユーザーの言語があなたの言語を決定します。ユーザーが明示的に切り替えを \
要求しない限り（例：\"think in English\"）、日本語で思考し、回答し続けて \
ください。";

pub const LOCALE_CLOSER_PT_BR: &str = "## Reforço de Idioma\n\n\
**Importante: seu `reasoning_content` (pensamento interno) e a resposta \
final devem permanecer em português do Brasil.** Independentemente de \
quanto código em inglês, logs de erro ou documentação você ler nesta \
sessão, e independentemente de o contexto do projeto ser em inglês, o \
processo de pensamento não pode derivar para o inglês. Este é um \
requisito rígido em nível de sessão — o idioma do usuário define seu \
idioma. A menos que o usuário peça explicitamente a troca (por exemplo, \
\"think in English\"), continue pensando e respondendo em português do \
Brasil.";

pub const LOCALE_PREAMBLE_VI: &str = "## Yêu cầu ngôn ngữ\n\n\
Bạn đang chạy trong codesmith. Cho dù ngữ cảnh tác vụ (mã nguồn, nhật ký lỗi, tên tệp) \
là tiếng Anh, cho dù phần còn lại của system prompt là tiếng Anh, bạn đều phải sử dụng \
tiếng Việt cho phần `reasoning_content` (suy nghĩ nội bộ) và câu trả lời cuối cùng. Các từ \
mã nguồn, đường dẫn tệp, tên công cụ (ví dụ `read_file`, `exec_shell`), biến môi trường, \
tham số dòng lệnh và URL giữ nguyên dạng gốc —— chỉ các văn bản giải thích bằng ngôn ngữ \
tự nhiên mới được chuyển sang tiếng Việt.\n\n\
Nếu người dùng chuyển sang ngôn ngữ khác trong phiên làm việc, hãy chuyển theo từ lượt tiếp theo. \
Nếu người dùng yêu cầu rõ ràng (ví dụ \"think in English\"), hãy ghi đè quy tắc này.";

pub const LOCALE_CLOSER_VI: &str = "## Nhắc nhở ngôn ngữ một lần nữa\n\n\
**Quan trọng: phần `reasoning_content` (suy nghĩ nội bộ) và phản hồi cuối cùng của bạn phải được viết bằng tiếng Việt.** \
Dù bạn có đọc bao nhiêu mã nguồn tiếng Anh, nhật ký lỗi hay tài liệu trong phiên làm việc này, và dù ngữ cảnh \
dự án có là tiếng Anh, quá trình suy nghĩ của bạn cũng không được chuyển sang tiếng Anh. Đây là yêu cầu cứng \
ở cấp phiên làm việc —— ngôn ngữ của người dùng quyết định ngôn ngữ của bạn, không phụ thuộc vào nội dung tiếng Anh \
tích lũy trong ngữ cảnh. Trừ khi người dùng yêu cầu rõ ràng việc chuyển đổi (ví dụ \"think in English\"), \
hãy tiếp tục suy nghĩ và trả lời bằng tiếng Việt.";

/// Memory extraction worker prompt — used by `/memory extract --dry-run` and
/// future background memory consolidation jobs. The prompt is intentionally a
/// narrow protocol: inspect only the supplied recent conversation transcript,
/// propose durable memory candidates, and do not read or modify workspace files.
pub const MEMORY_EXTRACTION_PROMPT: &str = "\
## Memory Extraction Protocol\n\
\n\
You are CodeSmith's memory extraction worker. Your only job is to identify durable, user-approved memory candidates from the supplied recent conversation transcript.\n\
\n\
Rules:\n\
- Use only the transcript included in the user's message. Do not request tools, inspect the repository, or infer facts from outside the transcript.\n\
- Extract only stable preferences, recurring workflow conventions, project facts, or explicit user instructions that are likely useful in future sessions.\n\
- Do not extract one-off task details, secrets, credentials, volatile status, temporary debugging notes, or facts the user explicitly rejected.\n\
- Preserve uncertainty. If a candidate is implied but not explicit, mark confidence as `low` and explain why.\n\
- Prefer declarative memories (for example, `User prefers concise status updates`) over imperatives (for example, `Always be concise`).\n\
- Return Markdown only, with this exact shape:\n\
\n\
```markdown\n\
## Memory candidates\n\
- memory: <durable memory text>\n\
  scope: user|project\n\
  confidence: high|medium|low\n\
  evidence: <short quote or message reference>\n\
\n\
## Rejected\n\
- <brief reason no candidate was extracted from a notable item>\n\
```\n\
\n\
If there are no candidates, write `- none` under `## Memory candidates` and explain the main rejection reason under `## Rejected`.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryExtractionMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryExtractionPrompt {
    pub system_prompt: &'static str,
    pub user_prompt: String,
}

pub fn build_memory_extraction_prompt(
    messages: &[MemoryExtractionMessage],
    existing_memory: Option<&str>,
    max_messages: usize,
) -> MemoryExtractionPrompt {
    let selected: Vec<&MemoryExtractionMessage> = messages
        .iter()
        .rev()
        .filter(|message| !message.content.trim().is_empty())
        .take(max_messages)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let mut user_prompt = String::new();
    user_prompt.push_str("Extract durable memory candidates from this recent conversation transcript. This is a dry-run proposal; do not write memory.\n\n");

    if let Some(existing) = existing_memory.and_then(|memory| {
        let trimmed = memory.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }) {
        user_prompt.push_str("## Existing memory\n\n");
        user_prompt.push_str(existing);
        user_prompt.push_str("\n\n");
    }

    user_prompt.push_str("## Recent transcript\n\n");
    if selected.is_empty() {
        user_prompt.push_str("(no recent transcript messages available)\n");
    } else {
        for (index, message) in selected.iter().enumerate() {
            let role = message.role.trim();
            let role = if role.is_empty() { "unknown" } else { role };
            let _ = writeln!(user_prompt, "### Message {} — {}", index + 1, role);
            user_prompt.push_str(message.content.trim());
            user_prompt.push_str("\n\n");
        }
    }

    MemoryExtractionPrompt {
        system_prompt: MEMORY_EXTRACTION_PROMPT,
        user_prompt,
    }
}

/// Personality overlays — voice and tone.
pub const CALM_PERSONALITY: &str = include_str!("prompts/personalities/calm.md");
pub const PLAYFUL_PERSONALITY: &str = include_str!("prompts/personalities/playful.md");

/// Mode deltas — permissions, workflow expectations, mode-specific rules.
pub const AGENT_MODE: &str = include_str!("prompts/modes/agent.md");
pub const PLAN_MODE: &str = include_str!("prompts/modes/plan.md");
pub const YOLO_MODE: &str = include_str!("prompts/modes/yolo.md");
pub const COORDINATOR_MODE: &str = include_str!("prompts/modes/coordinator.md");

/// Approval-policy overlays — whether tool calls are auto-approved,
/// require confirmation, or are blocked.
pub const AUTO_APPROVAL: &str = include_str!("prompts/approvals/auto.md");
pub const SUGGEST_APPROVAL: &str = include_str!("prompts/approvals/suggest.md");
pub const NEVER_APPROVAL: &str = include_str!("prompts/approvals/never.md");

/// Compaction relay template — written into the system prompt so the
/// model knows the format to use when writing `.codesmith/handoff.md`.
pub const COMPACT_TEMPLATE: &str = include_str!("prompts/compact.md");

/// Goal continuation audit template — injected by the engine when a runtime
/// goal is active and the assistant tries to end a turn without closing it.
pub const GOAL_CONTINUATION_PROMPT: &str = include_str!("prompts/continuation.md");

/// Memory hygiene guidance — appended to the system prompt only when the
/// session has a non-empty user-memory block. Steers the model toward
/// writing durable memories as declarative facts ("User prefers concise
/// responses") rather than imperatives ("Always respond concisely"),
/// because imperatives get re-read as directives in later sessions and
/// can override the user's current request (#725).
pub const MEMORY_GUIDANCE: &str = include_str!("prompts/memory_guidance.md");

/// KoD-specific guidance prompt — type taxonomy, save instructions,
/// staleness warnings, and constitutional tier placement.
pub const KNOWLEDGE_GUIDANCE: &str = include_str!("prompts/knowledge_guidance.md");

// ── Legacy prompt constants (kept for backwards compatibility) ────────

/// Legacy base prompt (agent.txt — now decomposed into base.md + overlays).
/// Still available for callers that haven't migrated to the layered API.
pub const AGENT_PROMPT: &str = include_str!("prompts/agent.txt");

// ── Personality selection ─────────────────────────────────────────────

/// Which personality overlay to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Personality {
    /// Cool, spatial, reserved — the default.
    Calm,
    /// Warm, energetic, playful — alternative for fun mode.
    Playful,
}

impl Personality {
    /// Resolve from the `calm_mode` settings flag.
    /// When `calm_mode` is true → Calm; when false → Playful (future).
    /// For now, always returns Calm — Playful is wired but opt-in.
    #[must_use]
    pub fn from_settings(calm_mode: bool) -> Self {
        if calm_mode {
            Self::Calm
        } else {
            // Future: when playful mode is exposed in settings, return Playful here.
            // For now, calm is the only default.
            Self::Calm
        }
    }

    /// Parse the `personality` config key value. Case-insensitive;
    /// accepts `"calm"` and `"playful"`.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "calm" => Ok(Self::Calm),
            "playful" => Ok(Self::Playful),
            other => Err(format!(
                "unknown personality `{other}` (expected `calm` or `playful`)"
            )),
        }
    }

    fn prompt(self) -> &'static str {
        match self {
            Self::Calm => CALM_PERSONALITY,
            Self::Playful => PLAYFUL_PERSONALITY,
        }
    }
}

// ── Composition ───────────────────────────────────────────────────────

pub fn mode_prompt(mode: AppMode) -> &'static str {
    match mode {
        AppMode::Agent => AGENT_MODE,
        AppMode::Yolo => YOLO_MODE,
        AppMode::Plan => PLAN_MODE,
        AppMode::Coordinator => COORDINATOR_MODE,
    }
}

pub fn default_approval_mode_for_mode(mode: AppMode) -> ApprovalMode {
    match mode {
        AppMode::Agent => ApprovalMode::Suggest,
        AppMode::Yolo => ApprovalMode::Auto,
        AppMode::Plan => ApprovalMode::Never,
        AppMode::Coordinator => ApprovalMode::Auto,
    }
}

pub fn approval_prompt_for_mode(mode: AppMode, approval_mode: ApprovalMode) -> &'static str {
    match mode {
        AppMode::Yolo | AppMode::Coordinator => AUTO_APPROVAL,
        AppMode::Plan => NEVER_APPROVAL,
        AppMode::Agent => match approval_mode {
            ApprovalMode::Auto => AUTO_APPROVAL,
            ApprovalMode::Suggest => SUGGEST_APPROVAL,
            ApprovalMode::Never => NEVER_APPROVAL,
        },
    }
}

/// Compose the full system prompt in deterministic order:
///   1. tool taxonomy  — compact hints generated from the eager core tools
///   2. base.md        — core identity, toolbox, execution contract
///   3. personality    — voice and tone overlay
///   4. mode delta     — mode-specific permissions and workflow
///   5. approval policy — tool-approval behavior
///
/// Each layer is separated by a blank line for readability in the
/// rendered prompt (the model sees them as contiguous sections).
/// Substitute the `{model_id}` template in the Constitutional preamble
/// with the active model identifier. The base prompt is a compile-time
/// constant; this function produces a per-session variant so the prompt
/// says "You are deepseek-v4-pro" or "You are deepseek-v4-flash" instead
/// of a static placeholder.
pub fn apply_model_template(prompt: &str, model_id: &str) -> String {
    prompt.replace("{model_id}", model_id)
}

pub const TOOL_TAXONOMY_DISCOVERY: &[&str] = &["grep_files", "file_search"];
pub const TOOL_TAXONOMY_GIT: &[&str] = &["git_status", "git_diff"];
pub const TOOL_TAXONOMY_VERIFICATION: &[&str] = &["run_tests"];

pub fn render_core_tool_taxonomy_block(mode: AppMode) -> String {
    let core_tools = core_taxonomy_tools_for_mode(mode);
    let mut sentences = Vec::new();

    if let Some(discovery) = render_core_tool_group(TOOL_TAXONOMY_DISCOVERY, &core_tools) {
        sentences.push(format!("Use {discovery} for discovery."));
    }
    if let Some(git) = render_core_tool_group(TOOL_TAXONOMY_GIT, &core_tools) {
        sentences.push(format!("Use {git} for git inspection."));
    }
    if let Some(verification) = render_core_tool_group(TOOL_TAXONOMY_VERIFICATION, &core_tools) {
        sentences.push(format!("Use {verification} for verification."));
    }

    debug_assert!(
        !sentences.is_empty(),
        "core tool taxonomy has no active tool groups"
    );
    format!("## Core Tool Taxonomy\n\n{}", sentences.join(" "))
}

pub fn core_taxonomy_tools_for_mode(mode: AppMode) -> Vec<&'static str> {
    let core_tools = crate::tools::default_active_native_tool_names();
    core_tools
        .iter()
        .copied()
        .filter(|tool| mode != AppMode::Plan || *tool != "run_tests")
        .collect()
}

pub fn render_core_tool_group(group: &[&str], core_tools: &[&str]) -> Option<String> {
    let rendered = group
        .iter()
        .copied()
        .filter(|tool| core_tools.contains(tool))
        .map(|tool| format!("`{tool}`"))
        .collect::<Vec<_>>()
        .join("/");
    (!rendered.is_empty()).then_some(rendered)
}

/// Authority recap block — appended at the end of the system prompt,
/// just before the user's first message. Uses recency bias constructively:
/// this is the last thing the model reads before generating, so it
/// reinforces the Constitutional hierarchy without occupying cache-stable
/// prefix space.
pub const AUTHORITY_RECAP: &str = "\
## Authority Recap

The Constitution of CodeSmith (Articles I-VII) governs your behavior.
Tier 1 rules — truthfulness, user agency, tool-use mandate, verification
duty — are non-negotiable. The user's next message is the highest
directive within Constitutional bounds. Personality, memory, and handoff
context are subordinate to the Constitution, the Statutes, and the user's
current request. When in doubt, consult Article VII: The Hierarchy of Law.";

pub fn compose_prompt(mode: AppMode, personality: Personality) -> String {
    compose_prompt_with_approval(mode, personality, default_approval_mode_for_mode(mode))
}

pub fn compose_prompt_with_approval(
    mode: AppMode,
    personality: Personality,
    approval_mode: ApprovalMode,
) -> String {
    compose_prompt_with_approval_and_model(mode, personality, approval_mode, "codesmith")
}

/// Compose with explicit model ID for dynamic identity injection.
/// The model_id replaces `{model_id}` in the Constitutional preamble.
pub fn compose_prompt_with_approval_and_model(
    mode: AppMode,
    personality: Personality,
    approval_mode: ApprovalMode,
    model_id: &str,
) -> String {
    let tool_taxonomy = render_core_tool_taxonomy_block(mode);
    let base_prompt = apply_model_template(effective_base_prompt().trim(), model_id);
    let parts: [&str; 5] = [
        tool_taxonomy.as_str(),
        base_prompt.as_str(),
        personality.prompt().trim(),
        mode_prompt(mode).trim(),
        approval_prompt_for_mode(mode, approval_mode).trim(),
    ];

    let mut out =
        String::with_capacity(parts.iter().map(|p| p.len()).sum::<usize>() + (parts.len() - 1) * 2);
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push('\n');
            out.push('\n');
        }
        out.push_str(part);
    }
    out
}

/// Compose for the default personality (Calm).
pub fn compose_mode_prompt(mode: AppMode) -> String {
    compose_prompt(mode, Personality::Calm)
}

pub fn compose_mode_prompt_with_approval(mode: AppMode, approval_mode: ApprovalMode) -> String {
    compose_prompt_with_approval(mode, Personality::Calm, approval_mode)
}

pub fn compose_mode_prompt_with_approval_and_model(
    mode: AppMode,
    personality: Personality,
    approval_mode: ApprovalMode,
    model_id: &str,
) -> String {
    compose_prompt_with_approval_and_model(mode, personality, approval_mode, model_id)
}

// ── Public API ────────────────────────────────────────────────────────

/// Get the system prompt for a specific mode (default Calm personality).
pub fn system_prompt_for_mode(mode: AppMode) -> SystemPrompt {
    SystemPrompt::Text(compose_mode_prompt(mode))
}

/// Get the system prompt for a specific mode with explicit personality.
pub fn system_prompt_for_mode_with_personality(
    mode: AppMode,
    personality: Personality,
) -> SystemPrompt {
    SystemPrompt::Text(compose_prompt(mode, personality))
}

pub fn system_prompt_for_mode_with_context_skills_and_session(
    mode: AppMode,
    workspace: &Path,
    _working_set_summary: Option<&str>,
    skills_dir: Option<&Path>,
    instructions: Option<&[InstructionSource]>,
    session_context: PromptSessionContext<'_>,
) -> SystemPrompt {
    system_prompt_for_mode_with_context_skills_session_and_approval(
        mode,
        workspace,
        _working_set_summary,
        skills_dir,
        instructions,
        session_context,
        default_approval_mode_for_mode(mode),
    )
}

pub fn append_section(
    bundle: &mut PromptBundle,
    id: impl Into<String>,
    title: impl Into<String>,
    body: impl Into<String>,
    stability: PromptSectionStability,
    source: PromptSectionSource,
) {
    let body = body.into();
    if body.trim().is_empty() {
        return;
    }
    bundle.push(PromptSection::cacheable(id, title, body, stability, source));
}

pub fn context_management_prompt() -> &'static str {
    "## Context Management\n\n\
     When the conversation gets long (you'll see a context usage indicator), you can:\n\
     1. Use `/compact` to summarize earlier context and free up space\n\
     2. The system will preserve important information (files you're working on, recent messages, tool results)\n\
     3. After compaction, you'll see a summary of what was discussed and can continue seamlessly\n\n\
     If you notice context is getting long (>60% during sustained work), proactively suggest using `/compact` to the user.\n\n\
     ### Prompt-cache awareness\n\n\
     DeepSeek caches the longest *byte-stable prefix* of every request and charges roughly 100× less for cache-hit tokens than miss tokens. The system prompt above is layered most-static-first specifically so the prefix stays stable turn-over-turn. To keep cache hits high:\n\
     - **Working set location:** the current repo working set is stored on new user messages inside a `<turn_meta>` block. Treat it as high-priority turn metadata, not as a stable system-prompt section.\n\
     - **Append, don't reorder.** New context goes at the end (latest user / tool messages). Reshuffling earlier messages or rewriting their content invalidates the cache for everything after the change.\n\
     - **Don't paraphrase quoted content.** If you've already read a file, refer to it by path or line range instead of re-quoting it with different formatting.\n\
     - **Use `/compact` as a hard reset, not a tweak.** Compaction is meant for when the cache is already losing — it intentionally rewrites the prefix to a shorter summary. Don't trigger it for small wins.\n\
     - **Read once, refer back.** Re-reading the same file produces a different tool-result envelope than the prior read; it's cheaper to scroll back than to re-fetch.\n\
     - **Footer chip:** the `cache hit %` chip turns red below 40% and yellow below 80%. If it's been red for several turns, that's a signal to consolidate."
}

pub fn render_append_system_prompt_block(sources: &[PromptAppendSource]) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();
    for source in sources {
        let (raw_source_name, raw_content): (String, String) = match source {
            PromptAppendSource::File(path) => match std::fs::read_to_string(path) {
                Ok(raw) => (path.display().to_string(), raw),
                Err(err) => {
                    tracing::warn!(
                        target: "prompt_runtime",
                        ?err,
                        ?path,
                        "skipping unreadable append-system-prompt file"
                    );
                    continue;
                }
            },
            PromptAppendSource::Inline { name, content } => (name.clone(), content.clone()),
        };
        let trimmed = raw_content.trim();
        if trimmed.is_empty() {
            continue;
        }
        let body = if trimmed.len() > INSTRUCTIONS_FILE_MAX_BYTES {
            let head_end = (0..=INSTRUCTIONS_FILE_MAX_BYTES)
                .rev()
                .find(|&i| trimmed.is_char_boundary(i))
                .unwrap_or(0);
            format!("{}\n[…elided]", &trimmed[..head_end])
        } else {
            trimmed.to_string()
        };
        sections.push(format!(
            "<system_prompt_append source=\"{raw_source_name}\">\n{body}\n</system_prompt_append>"
        ));
    }
    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

pub fn default_prompt_bundle_for_mode_with_context_skills_session_and_approval(
    mode: AppMode,
    workspace: &Path,
    _working_set_summary: Option<&str>,
    _skills_dir: Option<&Path>,
    instructions: Option<&[InstructionSource]>,
    session_context: PromptSessionContext<'_>,
    approval_mode: ApprovalMode,
) -> PromptBundle {
    let mode_prompt = compose_mode_prompt_with_approval_and_model(
        mode,
        session_context.personality,
        approval_mode,
        session_context.model_id,
    );

    let project_context = load_project_context_with_parents(workspace);
    let mut bundle = PromptBundle::new();

    if session_context.show_thinking
        && let Some(preamble) = locale_reinforcement_preamble(session_context.locale_tag)
    {
        append_section(
            &mut bundle,
            "locale_preamble",
            "Locale reinforcement preamble",
            preamble,
            PromptSectionStability::Session,
            PromptSectionSource::Builtin,
        );
    }

    append_section(
        &mut bundle,
        "global_system_prefix",
        "Global system prefix",
        mode_prompt,
        PromptSectionStability::Static,
        PromptSectionSource::Builtin,
    );

    if let Some(project_block) = project_context.as_system_block() {
        append_section(
            &mut bundle,
            "project_context",
            "Project context",
            project_block,
            PromptSectionStability::Workspace,
            PromptSectionSource::ProjectContext,
        );
    } else {
        tracing::warn!("No project context available and auto-generation failed");
    }

    if session_context.project_context_pack_enabled
        && let Some(pack) = crate::project_context::generate_project_context_pack(workspace)
    {
        append_section(
            &mut bundle,
            "project_context_pack",
            "Project context pack",
            pack,
            PromptSectionStability::Workspace,
            PromptSectionSource::ProjectContext,
        );
    }

    if session_context.translation_enabled {
        append_section(
            &mut bundle,
            "translation_requirement",
            "Translation output requirement",
            translation_output_instruction(session_context.locale_tag),
            PromptSectionStability::Session,
            PromptSectionSource::Builtin,
        );
    }

    if session_context.is_simple {
        append_section(
            &mut bundle,
            "conversation_style",
            "Conversation style",
            SIMPLE_CONVERSATION_STYLE,
            PromptSectionStability::Session,
            PromptSectionSource::Builtin,
        );
    }

    // The skills block is pre-rendered by the caller (see
    // `PromptSessionContext::skills_block`) so this builder stays free of
    // skills-discovery dependencies and portable across crates.
    let skills_block = session_context.skills_block;
    if let Some(block) = skills_block {
        append_section(
            &mut bundle,
            "skills",
            "Skills",
            block,
            PromptSectionStability::Workspace,
            PromptSectionSource::Skills,
        );
    }

    if matches!(mode, AppMode::Agent | AppMode::Yolo) {
        append_section(
            &mut bundle,
            "context_management",
            "Context management",
            context_management_prompt(),
            PromptSectionStability::Static,
            PromptSectionSource::Builtin,
        );
    }

    append_section(
        &mut bundle,
        "compact_template",
        "Compact template",
        COMPACT_TEMPLATE,
        PromptSectionStability::Static,
        PromptSectionSource::Builtin,
    );

    append_section(
        &mut bundle,
        "environment",
        "Environment",
        render_environment_block(workspace, session_context.locale_tag),
        PromptSectionStability::Session,
        PromptSectionSource::Builtin,
    );

    if let Some(sources) = instructions
        && let Some(block) = render_instructions_block(sources)
    {
        append_section(
            &mut bundle,
            "configured_instructions",
            "Configured instructions",
            block,
            PromptSectionStability::Session,
            PromptSectionSource::Config,
        );
    }

    if let Some(knowledge_block) = session_context.knowledge_prompt_block
        && !knowledge_block.trim().is_empty()
    {
        append_section(
            &mut bundle,
            "memory_or_knowledge",
            "Knowledge memory",
            format!("{knowledge_block}\n\n{KNOWLEDGE_GUIDANCE}"),
            PromptSectionStability::Session,
            PromptSectionSource::Memory,
        );
    } else if let Some(memory_block) = session_context.user_memory_block
        && !memory_block.trim().is_empty()
    {
        append_section(
            &mut bundle,
            "memory_or_knowledge",
            "User memory",
            format!("{memory_block}\n\n{MEMORY_GUIDANCE}"),
            PromptSectionStability::Session,
            PromptSectionSource::Memory,
        );
    }

    if let Some(goal_objective) = session_context.goal_objective
        && !goal_objective.trim().is_empty()
    {
        append_section(
            &mut bundle,
            "session_goal",
            "Current Hunt",
            format!(
                "## Current Hunt\n\n<session_goal>\n{}\n</session_goal>",
                goal_objective.trim()
            ),
            PromptSectionStability::Session,
            PromptSectionSource::Config,
        );
    }

    if let Some(handoff_block) = load_handoff_block(workspace) {
        append_section(
            &mut bundle,
            "previous_session_relay",
            "Previous session relay",
            handoff_block,
            PromptSectionStability::Dynamic,
            PromptSectionSource::Handoff,
        );
    }

    append_section(
        &mut bundle,
        "authority_recap",
        "Authority recap",
        effective_authority_recap(),
        PromptSectionStability::Session,
        PromptSectionSource::Builtin,
    );

    if let Some(closer) = session_context
        .show_thinking
        .then(|| locale_reinforcement_closer(session_context.locale_tag))
        .flatten()
    {
        append_section(
            &mut bundle,
            "locale_closer",
            "Locale reinforcement closer",
            closer,
            PromptSectionStability::Session,
            PromptSectionSource::Builtin,
        );
    } else if !session_context.show_thinking {
        append_section(
            &mut bundle,
            "hidden_thinking_language",
            "Hidden thinking language",
            hidden_thinking_language_instruction(session_context.locale_tag),
            PromptSectionStability::Session,
            PromptSectionSource::Builtin,
        );
    }

    bundle
}

pub fn effective_prompt_bundle_for_mode_with_runtime_context_and_approval(
    mode: AppMode,
    workspace: &Path,
    working_set_summary: Option<&str>,
    skills_dir: Option<&Path>,
    instructions: Option<&[InstructionSource]>,
    runtime_context: PromptRuntimeContext<'_>,
    approval_mode: ApprovalMode,
) -> PromptBundle {
    let default_bundle = default_prompt_bundle_for_mode_with_context_skills_session_and_approval(
        mode,
        workspace,
        working_set_summary,
        skills_dir,
        instructions,
        runtime_context.session.clone(),
        approval_mode,
    );

    let mut append_sections = Vec::new();
    if let Some(block) = render_append_system_prompt_block(runtime_context.append_system_prompts) {
        append_sections.push(PromptSection::cacheable(
            "append_system_prompt",
            "Append system prompt",
            block,
            PromptSectionStability::Session,
            PromptSectionSource::Cli,
        ));
    }
    if let Some(cache_breaker) = runtime_context.cache_breaker
        && !cache_breaker.trim().is_empty()
    {
        append_sections.push(PromptSection::new(
            "cache_breaker",
            "Cache breaker",
            format!("<cache_breaker>{}</cache_breaker>", cache_breaker.trim()),
            PromptSectionStability::Dynamic,
            PromptCachePolicy::CacheBreaker,
            PromptSectionSource::Debug,
        ));
    }

    build_effective_system_prompt(EffectiveSystemPromptInput {
        default_bundle,
        custom_system_prompt: runtime_context.custom_system_prompt.map(str::to_string),
        agent_system_prompt: runtime_context.agent_system_prompt.map(str::to_string),
        coordinator_system_prompt: runtime_context
            .coordinator_system_prompt
            .map(str::to_string),
        override_system_prompt: runtime_context.override_system_prompt.map(str::to_string),
        append_sections,
    })
}

pub fn effective_prompt_bundle_for_mode_with_context_skills_session_and_approval(
    mode: AppMode,
    workspace: &Path,
    working_set_summary: Option<&str>,
    skills_dir: Option<&Path>,
    instructions: Option<&[InstructionSource]>,
    session_context: PromptSessionContext<'_>,
    approval_mode: ApprovalMode,
) -> PromptBundle {
    effective_prompt_bundle_for_mode_with_runtime_context_and_approval(
        mode,
        workspace,
        working_set_summary,
        skills_dir,
        instructions,
        session_context.runtime(),
        approval_mode,
    )
}

pub fn system_prompt_for_mode_with_context_skills_session_and_approval(
    mode: AppMode,
    workspace: &Path,
    working_set_summary: Option<&str>,
    skills_dir: Option<&Path>,
    instructions: Option<&[InstructionSource]>,
    session_context: PromptSessionContext<'_>,
    approval_mode: ApprovalMode,
) -> SystemPrompt {
    effective_prompt_bundle_for_mode_with_context_skills_session_and_approval(
        mode,
        workspace,
        working_set_summary,
        skills_dir,
        instructions,
        session_context,
        approval_mode,
    )
    .render_system_prompt()
}

/// Build a system prompt with explicit project context
pub fn build_system_prompt(base: &str, project_context: Option<&ProjectContext>) -> SystemPrompt {
    let full_prompt =
        match project_context.and_then(super::project_context::ProjectContext::as_system_block) {
            Some(project_block) => format!("{}\n\n{}", base.trim(), project_block),
            None => base.trim().to_string(),
        };
    SystemPrompt::Text(full_prompt)
}
