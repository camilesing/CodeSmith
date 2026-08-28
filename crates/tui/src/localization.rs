//! Lightweight localization registry for high-visibility TUI strings.
//!
//! This intentionally covers UI chrome only. It does not change model prompts,
//! model output language, provider behavior, or media payload semantics.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDirection {
    Ltr,
    Rtl,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocaleCoverage {
    English,
    Shipped,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocaleSpec {
    pub tag: &'static str,
    pub display_name: &'static str,
    pub script: &'static str,
    pub direction: TextDirection,
    pub fallback: &'static str,
    pub coverage: LocaleCoverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    En,
    ZhHans,
    ZhHant,
    Hi,
    Es419,
}

impl Locale {
    pub fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhHans => "zh-Hans",
            Self::ZhHant => "zh-Hant",
            Self::Hi => "hi",
            Self::Es419 => "es-419",
        }
    }

    pub fn translation_target_name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::ZhHans => "Simplified Chinese (简体中文)",
            Self::ZhHant => "Traditional Chinese (繁體中文)",
            Self::Hi => "Hindi (हिन्दी)",
            Self::Es419 => "Latin American Spanish (Español latinoamericano)",
        }
    }

    #[allow(dead_code)]
    pub fn spec(self) -> LocaleSpec {
        match self {
            Self::En => LocaleSpec {
                tag: "en",
                display_name: "English",
                script: "Latin",
                direction: TextDirection::Ltr,
                fallback: "en",
                coverage: LocaleCoverage::English,
            },
            Self::ZhHans => LocaleSpec {
                tag: "zh-Hans",
                display_name: "Chinese Simplified",
                script: "Hans",
                direction: TextDirection::Ltr,
                fallback: "en",
                coverage: LocaleCoverage::Shipped,
            },
            Self::ZhHant => LocaleSpec {
                tag: "zh-Hant",
                display_name: "Chinese Traditional",
                script: "Hant",
                direction: TextDirection::Ltr,
                fallback: "zh-Hans",
                coverage: LocaleCoverage::Shipped,
            },
            Self::Hi => LocaleSpec {
                tag: "hi",
                display_name: "Hindi",
                script: "Deva",
                direction: TextDirection::Ltr,
                fallback: "en",
                coverage: LocaleCoverage::Shipped,
            },
            Self::Es419 => LocaleSpec {
                tag: "es-419",
                display_name: "Spanish (Latin America)",
                script: "Latin",
                direction: TextDirection::Ltr,
                fallback: "en",
                coverage: LocaleCoverage::Shipped,
            },
        }
    }

    #[allow(dead_code)]
    pub fn shipped() -> &'static [Self] {
        &[Self::En, Self::ZhHans, Self::ZhHant, Self::Hi, Self::Es419]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageId {
    ComposerPlaceholder,
    HistorySearchPlaceholder,
    HistorySearchTitle,
    HistoryHintMove,
    HistoryHintAccept,
    HistoryHintRestore,
    HistoryNoMatches,
    ConfigTitle,
    ConfigModalTitle,
    ConfigSearchPlaceholder,
    ConfigNoSettings,
    ConfigNoMatchesPrefix,
    ConfigFilteredSettings,
    ConfigShowing,
    ConfigFooterDefault,
    ConfigFooterScrollable,
    ConfigFooterFiltered,
    HelpTitle,
    HelpFilterPlaceholder,
    HelpFilterPrefix,
    HelpNoMatches,
    HelpSlashCommands,
    HelpKeybindings,
    HelpFooterTypeFilter,
    HelpFooterMove,
    HelpFooterJump,
    HelpFooterClose,
    CmdAttachDescription,
    CmdAnchorDescription,
    CmdCacheDescription,
    CmdChangeDescription,
    CmdChangeHeader,
    CmdChangeTranslationQueued,
    CmdChangeTranslationUnavailable,
    CmdChangePreviousVersion,
    CmdBalanceDescription,
    CmdClearDescription,
    CmdCompactDescription,
    CmdPurgeDescription,
    CmdConfigDescription,
    CmdContextDescription,
    CmdCostDescription,
    CmdCycleDescription,
    CmdCyclesDescription,
    CmdDiffDescription,
    CmdEditDescription,
    CmdExitDescription,
    CmdExportDescription,
    CmdFeedbackDescription,
    CmdHelpDescription,
    CmdHomeDescription,
    CmdHooksDescription,
    CmdAgentDescription,
    CmdGoalDescription,
    CmdInitDescription,
    CmdJobsDescription,
    CmdLinksDescription,
    CmdLoadDescription,
    CmdLogoutDescription,
    CmdMcpDescription,
    CmdMemoryDescription,
    CmdModeDescription,
    CmdModelDescription,
    CmdModelsDescription,
    CmdNetworkDescription,
    CmdNoteDescription,
    CmdThemeDescription,
    CmdProviderDescription,
    CmdQueueDescription,
    CmdRecallDescription,
    CmdRelayDescription,
    CmdRenameDescription,
    CmdRestoreDescription,
    CmdRetryDescription,
    CmdReviewDescription,
    CmdRlmDescription,
    CmdSaveDescription,
    CmdForkDescription,
    CmdNewDescription,
    CmdSessionsDescription,
    CmdSettingsDescription,
    CmdSkillDescription,
    CmdSkillsDescription,
    CmdSlopDescription,
    CmdStashDescription,
    CmdStatusDescription,
    CmdStatuslineDescription,
    CmdSubagentsDescription,
    CmdSwarmDescription,
    CmdSystemDescription,
    CmdTaskDescription,
    CmdTokensDescription,
    CmdTranslateDescription,
    CmdTranslateOff,
    CmdTranslateOn,
    TranslationInProgress,
    TranslationComplete,
    TranslationFailed,
    CmdTrustDescription,
    CmdLspDescription,
    CmdShareDescription,
    CmdWorkspaceDescription,
    CmdUndoDescription,
    CmdVerboseDescription,
    CmdCacheAdvice,
    CmdCacheFootnote,
    CmdCacheHeader,
    CmdCacheNoData,
    CmdCacheTotals,
    CmdCostReport,
    CmdTokensCacheBoth,
    CmdTokensCacheHitOnly,
    CmdTokensCacheMissOnly,
    CmdTokensContextUnknownWindow,
    CmdTokensContextWithWindow,
    CmdTokensNotReported,
    CmdTokensReport,
    FooterAgentSingular,
    FooterAgentsPlural,
    FooterPressCtrlCAgain,
    FooterWorking,
    FooterBalancePrefix,
    HelpSectionActions,
    HelpSectionClipboard,
    HelpSectionEditing,
    HelpSectionHelp,
    HelpSectionModes,
    HelpSectionNavigation,
    HelpSectionSessions,
    KbScrollTranscript,
    KbNavigateHistory,
    KbScrollTranscriptAlt,
    KbBrowseHistory,
    KbScrollPage,
    KbJumpTopBottom,
    KbJumpTopBottomEmpty,
    KbJumpToolBlocks,
    KbMoveCursor,
    KbJumpLineStartEnd,
    KbDeleteChar,
    KbClearDraft,
    KbStashDraft,
    KbSearchHistory,
    KbInsertNewline,
    KbSendDraft,
    KbCloseMenu,
    KbCancelOrExit,
    KbShellControls,
    KbExitEmpty,
    KbCommandPalette,
    KbFuzzyFilePicker,
    KbCompactInspector,
    KbLastMessagePager,
    KbSelectedDetails,
    KbToolDetailsPager,
    KbThinkingPager,
    KbLiveTranscript,
    KbBacktrackMessage,
    KbCompleteCycleModes,
    KbJumpPlanAgentYolo,
    KbAltJumpPlanAgentYolo,
    KbFocusSidebar,
    KbTogglePlanAgent,
    KbSessionPicker,
    KbPasteAttach,
    KbCopySelection,
    KbContextMenu,
    KbAttachPath,
    KbHelpOverlay,
    KbToggleHelp,
    KbToggleHelpSlash,
    HelpUsageLabel,
    HelpAliasesLabel,
    SettingsTitle,
    SettingsConfigFile,
    ClearConversation,
    ClearConversationBusy,
    ModelChanged,
    LinksTitle,
    LinksDashboard,
    LinksDocs,
    LinksTip,
    SubagentsFetching,
    HelpUnknownCommand,
    HomeDashboardTitle,
    HomeModel,
    HomeMode,
    HomeWorkspace,
    HomeHistory,
    HomeTokens,
    HomeQueued,
    HomeSubagents,
    HomeSkill,
    HomeQuickActions,
    HomeQuickLinks,
    HomeQuickSkills,
    HomeQuickConfig,
    HomeQuickSettings,
    HomeQuickModel,
    HomeQuickSubagents,
    HomeQuickTaskList,
    HomeQuickHelp,
    HomeModeTips,
    HomeAgentModeTip,
    HomeAgentModeReviewTip,
    HomeAgentModeYoloTip,
    HomeYoloModeTip,
    HomeYoloModeCaution,
    HomePlanModeTip,
    HomePlanModeChecklistTip,
    HomeGoalModeTip,
    // Onboarding screens — language picker.
    OnboardLanguageTitle,
    OnboardLanguageBlurb,
    OnboardLanguageFooter,
    // Onboarding screens — API key entry.
    OnboardApiKeyTitle,
    OnboardApiKeyStep1,
    OnboardApiKeyStep2,
    OnboardApiKeySavedHint,
    OnboardApiKeyFormatHint,
    OnboardApiKeyPlaceholder,
    OnboardApiKeyLabel,
    OnboardApiKeyFooter,
    // Onboarding screens — workspace trust prompt.
    OnboardTrustTitle,
    OnboardTrustQuestion,
    OnboardTrustLocationPrefix,
    OnboardTrustRiskHint,
    OnboardTrustEffectHint,
    OnboardTrustFooterPrefix,
    OnboardTrustFooterMiddle,
    OnboardTrustFooterSuffix,
    // Onboarding screens — final tips screen.
    OnboardTipsTitle,
    OnboardTipsLine1,
    OnboardTipsLine2,
    OnboardTipsLine3,
    OnboardTipsLine4,
    OnboardTipsFooterEnter,
    OnboardTipsFooterAction,
    // Context menu.
    CtxMenuTitle,
    CtxMenuCopySelection,
    CtxMenuCopySelectionDesc,
    CtxMenuOpenSelection,
    CtxMenuOpenSelectionDesc,
    CtxMenuClearSelection,
    CtxMenuOpenDetails,
    CtxMenuCopyMessage,
    CtxMenuCopyMessageDesc,
    CtxMenuOpenInEditor,
    CtxMenuOpenInEditorDesc,
    CtxMenuShowCell,
    CtxMenuShowCellDesc,
    CtxMenuHideCell,
    CtxMenuHideCellDesc,
    CtxMenuShowHidden,
    CtxMenuShowHiddenDesc,
    CtxMenuPaste,
    CtxMenuPasteDesc,
    CtxMenuCmdPalette,
    CtxMenuCmdPaletteDesc,
    CtxMenuContextInspector,
    CtxMenuContextInspectorDesc,
    CtxMenuHelp,
    CtxMenuHelpDesc,
}

#[allow(dead_code)]
pub const ALL_MESSAGE_IDS: &[MessageId] = &[
    MessageId::ComposerPlaceholder,
    MessageId::HistorySearchPlaceholder,
    MessageId::HistorySearchTitle,
    MessageId::HistoryHintMove,
    MessageId::HistoryHintAccept,
    MessageId::HistoryHintRestore,
    MessageId::HistoryNoMatches,
    MessageId::ConfigTitle,
    MessageId::ConfigModalTitle,
    MessageId::ConfigSearchPlaceholder,
    MessageId::ConfigNoSettings,
    MessageId::ConfigNoMatchesPrefix,
    MessageId::ConfigFilteredSettings,
    MessageId::ConfigShowing,
    MessageId::ConfigFooterDefault,
    MessageId::ConfigFooterScrollable,
    MessageId::ConfigFooterFiltered,
    MessageId::HelpTitle,
    MessageId::HelpFilterPlaceholder,
    MessageId::HelpFilterPrefix,
    MessageId::HelpNoMatches,
    MessageId::HelpSlashCommands,
    MessageId::HelpKeybindings,
    MessageId::HelpFooterTypeFilter,
    MessageId::HelpFooterMove,
    MessageId::HelpFooterJump,
    MessageId::HelpFooterClose,
    MessageId::CmdAnchorDescription,
    MessageId::CmdAttachDescription,
    MessageId::CmdBalanceDescription,
    MessageId::CmdCacheDescription,
    MessageId::CmdClearDescription,
    MessageId::CmdCompactDescription,
    MessageId::CmdPurgeDescription,
    MessageId::CmdConfigDescription,
    MessageId::CmdContextDescription,
    MessageId::CmdCostDescription,
    MessageId::CmdCycleDescription,
    MessageId::CmdCyclesDescription,
    MessageId::CmdDiffDescription,
    MessageId::CmdEditDescription,
    MessageId::CmdExitDescription,
    MessageId::CmdExportDescription,
    MessageId::CmdFeedbackDescription,
    MessageId::CmdHelpDescription,
    MessageId::CmdHomeDescription,
    MessageId::CmdHooksDescription,
    MessageId::CmdAgentDescription,
    MessageId::CmdInitDescription,
    MessageId::CmdJobsDescription,
    MessageId::CmdLinksDescription,
    MessageId::CmdLoadDescription,
    MessageId::CmdLogoutDescription,
    MessageId::CmdMcpDescription,
    MessageId::CmdMemoryDescription,
    MessageId::CmdModeDescription,
    MessageId::CmdModelDescription,
    MessageId::CmdModelsDescription,
    MessageId::CmdNetworkDescription,
    MessageId::CmdNoteDescription,
    MessageId::CmdProviderDescription,
    MessageId::CmdQueueDescription,
    MessageId::CmdRecallDescription,
    MessageId::CmdRelayDescription,
    MessageId::CmdRenameDescription,
    MessageId::CmdRestoreDescription,
    MessageId::CmdRetryDescription,
    MessageId::CmdReviewDescription,
    MessageId::CmdRlmDescription,
    MessageId::CmdSaveDescription,
    MessageId::CmdNewDescription,
    MessageId::CmdSessionsDescription,
    MessageId::CmdSettingsDescription,
    MessageId::CmdSkillDescription,
    MessageId::CmdSkillsDescription,
    MessageId::CmdSlopDescription,
    MessageId::CmdStashDescription,
    MessageId::CmdStatusDescription,
    MessageId::CmdStatuslineDescription,
    MessageId::CmdSubagentsDescription,
    MessageId::CmdSwarmDescription,
    MessageId::CmdSystemDescription,
    MessageId::CmdTaskDescription,
    MessageId::CmdTokensDescription,
    MessageId::CmdTranslateDescription,
    MessageId::CmdTranslateOff,
    MessageId::CmdTranslateOn,
    MessageId::TranslationInProgress,
    MessageId::TranslationComplete,
    MessageId::TranslationFailed,
    MessageId::CmdTrustDescription,
    MessageId::CmdLspDescription,
    MessageId::CmdShareDescription,
    MessageId::CmdWorkspaceDescription,
    MessageId::CmdUndoDescription,
    MessageId::CmdVerboseDescription,
    MessageId::CmdCacheAdvice,
    MessageId::CmdCacheFootnote,
    MessageId::CmdCacheHeader,
    MessageId::CmdCacheNoData,
    MessageId::CmdCacheTotals,
    MessageId::CmdChangeDescription,
    MessageId::CmdChangeHeader,
    MessageId::CmdChangeTranslationQueued,
    MessageId::CmdChangeTranslationUnavailable,
    MessageId::CmdChangePreviousVersion,
    MessageId::CmdCostReport,
    MessageId::CmdTokensCacheBoth,
    MessageId::CmdTokensCacheHitOnly,
    MessageId::CmdTokensCacheMissOnly,
    MessageId::CmdTokensContextUnknownWindow,
    MessageId::CmdTokensContextWithWindow,
    MessageId::CmdTokensNotReported,
    MessageId::CmdTokensReport,
    MessageId::FooterAgentSingular,
    MessageId::FooterAgentsPlural,
    MessageId::FooterPressCtrlCAgain,
    MessageId::FooterWorking,
    MessageId::FooterBalancePrefix,
    MessageId::HelpSectionActions,
    MessageId::HelpSectionClipboard,
    MessageId::HelpSectionEditing,
    MessageId::HelpSectionHelp,
    MessageId::HelpSectionModes,
    MessageId::HelpSectionNavigation,
    MessageId::HelpSectionSessions,
    MessageId::KbScrollTranscript,
    MessageId::KbNavigateHistory,
    MessageId::KbScrollTranscriptAlt,
    MessageId::KbBrowseHistory,
    MessageId::KbScrollPage,
    MessageId::KbJumpTopBottom,
    MessageId::KbJumpTopBottomEmpty,
    MessageId::KbJumpToolBlocks,
    MessageId::KbMoveCursor,
    MessageId::KbJumpLineStartEnd,
    MessageId::KbDeleteChar,
    MessageId::KbClearDraft,
    MessageId::KbStashDraft,
    MessageId::KbSearchHistory,
    MessageId::KbInsertNewline,
    MessageId::KbSendDraft,
    MessageId::KbCloseMenu,
    MessageId::KbCancelOrExit,
    MessageId::KbShellControls,
    MessageId::KbExitEmpty,
    MessageId::KbCommandPalette,
    MessageId::KbFuzzyFilePicker,
    MessageId::KbCompactInspector,
    MessageId::KbLastMessagePager,
    MessageId::KbSelectedDetails,
    MessageId::KbToolDetailsPager,
    MessageId::KbThinkingPager,
    MessageId::KbLiveTranscript,
    MessageId::KbBacktrackMessage,
    MessageId::KbCompleteCycleModes,
    MessageId::KbJumpPlanAgentYolo,
    MessageId::KbAltJumpPlanAgentYolo,
    MessageId::KbFocusSidebar,
    MessageId::KbTogglePlanAgent,
    MessageId::KbSessionPicker,
    MessageId::KbPasteAttach,
    MessageId::KbCopySelection,
    MessageId::KbContextMenu,
    MessageId::KbAttachPath,
    MessageId::KbHelpOverlay,
    MessageId::KbToggleHelp,
    MessageId::KbToggleHelpSlash,
    MessageId::HelpUsageLabel,
    MessageId::HelpAliasesLabel,
    MessageId::SettingsTitle,
    MessageId::SettingsConfigFile,
    MessageId::ClearConversation,
    MessageId::ClearConversationBusy,
    MessageId::ModelChanged,
    MessageId::LinksTitle,
    MessageId::LinksDashboard,
    MessageId::LinksDocs,
    MessageId::LinksTip,
    MessageId::SubagentsFetching,
    MessageId::HelpUnknownCommand,
    MessageId::HomeDashboardTitle,
    MessageId::HomeModel,
    MessageId::HomeMode,
    MessageId::HomeWorkspace,
    MessageId::HomeHistory,
    MessageId::HomeTokens,
    MessageId::HomeQueued,
    MessageId::HomeSubagents,
    MessageId::HomeSkill,
    MessageId::HomeQuickActions,
    MessageId::HomeQuickLinks,
    MessageId::HomeQuickSkills,
    MessageId::HomeQuickConfig,
    MessageId::HomeQuickSettings,
    MessageId::HomeQuickModel,
    MessageId::HomeQuickSubagents,
    MessageId::HomeQuickTaskList,
    MessageId::HomeQuickHelp,
    MessageId::HomeModeTips,
    MessageId::HomeAgentModeTip,
    MessageId::HomeAgentModeReviewTip,
    MessageId::HomeAgentModeYoloTip,
    MessageId::HomeYoloModeTip,
    MessageId::HomeYoloModeCaution,
    MessageId::HomePlanModeTip,
    MessageId::HomePlanModeChecklistTip,
    MessageId::HomeGoalModeTip,
    MessageId::OnboardLanguageTitle,
    MessageId::OnboardLanguageBlurb,
    MessageId::OnboardLanguageFooter,
    MessageId::OnboardApiKeyTitle,
    MessageId::OnboardApiKeyStep1,
    MessageId::OnboardApiKeyStep2,
    MessageId::OnboardApiKeySavedHint,
    MessageId::OnboardApiKeyFormatHint,
    MessageId::OnboardApiKeyPlaceholder,
    MessageId::OnboardApiKeyLabel,
    MessageId::OnboardApiKeyFooter,
    MessageId::OnboardTrustTitle,
    MessageId::OnboardTrustQuestion,
    MessageId::OnboardTrustLocationPrefix,
    MessageId::OnboardTrustRiskHint,
    MessageId::OnboardTrustEffectHint,
    MessageId::OnboardTrustFooterPrefix,
    MessageId::OnboardTrustFooterMiddle,
    MessageId::OnboardTrustFooterSuffix,
    MessageId::OnboardTipsTitle,
    MessageId::OnboardTipsLine1,
    MessageId::OnboardTipsLine2,
    MessageId::OnboardTipsLine3,
    MessageId::OnboardTipsLine4,
    MessageId::OnboardTipsFooterEnter,
    MessageId::OnboardTipsFooterAction,
    // Context menu.
    MessageId::CtxMenuTitle,
    MessageId::CtxMenuCopySelection,
    MessageId::CtxMenuCopySelectionDesc,
    MessageId::CtxMenuOpenSelection,
    MessageId::CtxMenuOpenSelectionDesc,
    MessageId::CtxMenuClearSelection,
    MessageId::CtxMenuOpenDetails,
    MessageId::CtxMenuCopyMessage,
    MessageId::CtxMenuCopyMessageDesc,
    MessageId::CtxMenuOpenInEditor,
    MessageId::CtxMenuOpenInEditorDesc,
    MessageId::CtxMenuShowCell,
    MessageId::CtxMenuShowCellDesc,
    MessageId::CtxMenuHideCell,
    MessageId::CtxMenuHideCellDesc,
    MessageId::CtxMenuShowHidden,
    MessageId::CtxMenuShowHiddenDesc,
    MessageId::CtxMenuPaste,
    MessageId::CtxMenuPasteDesc,
    MessageId::CtxMenuCmdPalette,
    MessageId::CtxMenuCmdPaletteDesc,
    MessageId::CtxMenuContextInspector,
    MessageId::CtxMenuContextInspectorDesc,
    MessageId::CtxMenuHelp,
    MessageId::CtxMenuHelpDesc,
];

pub fn tr(locale: Locale, id: MessageId) -> &'static str {
    fallback_translation(translation(locale, id), id)
}

pub fn thinking_translation_placeholder(locale: Locale) -> &'static str {
    match locale {
        Locale::En => "Thinking; translating when complete...",
        Locale::ZhHans => "正在思考，完成后翻译为简体中文...",
        Locale::ZhHant => "正在思考，完成後翻譯為繁體中文...",
        Locale::Hi => "सोच रहा है; पूरा होने पर अनुवाद किया जाएगा...",
        Locale::Es419 => "Pensando; traduciendo al finalizar...",
    }
}

pub fn thinking_translation_in_progress(locale: Locale) -> &'static str {
    match locale {
        Locale::En => "Translating thinking content...",
        Locale::ZhHans => "正在翻译思考内容...",
        Locale::ZhHant => "正在翻譯思考內容...",
        Locale::Hi => "thinking सामग्री का अनुवाद हो रहा है...",
        Locale::Es419 => "Traduciendo el contenido de razonamiento...",
    }
}

pub fn thinking_translation_complete(locale: Locale) -> &'static str {
    match locale {
        Locale::En => "Thinking translation complete",
        Locale::ZhHans => "思考内容翻译完成",
        Locale::ZhHant => "思考內容翻譯完成",
        Locale::Hi => "thinking का अनुवाद पूर्ण हुआ",
        Locale::Es419 => "Traducción del razonamiento completada",
    }
}

pub fn thinking_translation_failed(locale: Locale) -> &'static str {
    match locale {
        Locale::En => "Thinking translation failed",
        Locale::ZhHans => "思考内容翻译失败",
        Locale::ZhHant => "思考內容翻譯失敗",
        Locale::Hi => "thinking का अनुवाद विफल हुआ",
        Locale::Es419 => "Falló la traducción del razonamiento",
    }
}

pub fn hidden_translation_failed(locale: Locale) -> &'static str {
    match locale {
        Locale::En => "Translation failed; original text is hidden.",
        Locale::ZhHans => "翻译失败，原文已隐藏。",
        Locale::ZhHant => "翻譯失敗，原文已隱藏。",
        Locale::Hi => "अनुवाद विफल; मूल पाठ छिपा हुआ है।",
        Locale::Es419 => "La traducción falló; el texto original está oculto.",
    }
}

#[allow(dead_code)]
pub fn missing_message_ids(locale: Locale) -> Vec<MessageId> {
    ALL_MESSAGE_IDS
        .iter()
        .copied()
        .filter(|id| translation(locale, *id).is_none())
        .collect()
}

pub fn normalize_configured_locale(input: &str) -> Option<&'static str> {
    let normalized = normalize_locale_input(input);
    if matches!(normalized.as_str(), "" | "auto" | "system") {
        return Some("auto");
    }
    parse_locale(&normalized).map(Locale::tag)
}

pub fn resolve_locale(setting: &str) -> Locale {
    resolve_locale_with_env(setting, |key| std::env::var(key).ok())
}

pub fn resolve_locale_with_env<F>(setting: &str, env: F) -> Locale
where
    F: Fn(&str) -> Option<String>,
{
    let normalized = normalize_locale_input(setting);
    if !matches!(normalized.as_str(), "" | "auto" | "system") {
        return parse_locale(&normalized).unwrap_or(Locale::En);
    }

    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(value) = env(key)
            && let Some(locale) = parse_locale(&normalize_locale_input(&value))
        {
            return locale;
        }
    }

    Locale::En
}

#[allow(dead_code)]
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text.width() <= max_width {
        return text.to_string();
    }

    let ellipsis_width = '…'.width().unwrap_or(1);
    if max_width <= ellipsis_width {
        return "…".to_string();
    }

    let limit = max_width - ellipsis_width;
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width > limit {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push('…');
    out
}

fn normalize_locale_input(input: &str) -> String {
    input
        .split('.')
        .next()
        .unwrap_or(input)
        .split('@')
        .next()
        .unwrap_or(input)
        .trim()
        .replace('_', "-")
        .to_lowercase()
}

fn parse_locale(value: &str) -> Option<Locale> {
    if value == "c" || value == "posix" || value.starts_with("en") {
        return Some(Locale::En);
    }
    if value.starts_with("zh") {
        if value.contains("hant")
            || value.contains("-tw")
            || value.contains("-hk")
            || value.contains("-mo")
        {
            return Some(Locale::ZhHant);
        }
        return Some(Locale::ZhHans);
    }
    if value.starts_with("hi") {
        return Some(Locale::Hi);
    }
    if value.starts_with("es") {
        return Some(Locale::Es419);
    }
    None
}

fn fallback_translation(candidate: Option<&'static str>, id: MessageId) -> &'static str {
    candidate.unwrap_or_else(|| english(id))
}

fn english(id: MessageId) -> &'static str {
    match id {
        MessageId::ComposerPlaceholder => "Write a task or use /.",
        MessageId::HistorySearchPlaceholder => "Search prompt history...",
        MessageId::HistorySearchTitle => "History Search",
        MessageId::HistoryHintMove => "Up/Down move",
        MessageId::HistoryHintAccept => "Enter accept",
        MessageId::HistoryHintRestore => "Esc restore",
        MessageId::HistoryNoMatches => "  No matches",
        MessageId::ConfigTitle => "Session Configuration",
        MessageId::ConfigModalTitle => " Config ",
        MessageId::ConfigSearchPlaceholder => "type to filter",
        MessageId::ConfigNoSettings => "  No settings available.",
        MessageId::ConfigNoMatchesPrefix => "  No settings match ",
        MessageId::ConfigFilteredSettings => "  Filtered settings",
        MessageId::ConfigShowing => "  Showing",
        MessageId::ConfigFooterDefault => {
            " type=filter, Up/Down=select, Enter/e=edit, Esc/q=close "
        }
        MessageId::ConfigFooterScrollable => {
            " type=filter, Up/Down=select, Enter/e=edit, PgUp/PgDn=scroll, Esc/q=close "
        }
        MessageId::ConfigFooterFiltered => {
            " type=filter, Backspace=delete, Ctrl+U/Esc=clear, Enter=edit "
        }
        MessageId::HelpTitle => "Help",
        MessageId::HelpFilterPlaceholder => "Type to filter",
        MessageId::HelpFilterPrefix => "Filter: ",
        MessageId::HelpNoMatches => "  No matches.",
        MessageId::HelpSlashCommands => "Slash commands",
        MessageId::HelpKeybindings => "Keybindings",
        MessageId::HelpFooterTypeFilter => " type to filter ",
        MessageId::HelpFooterMove => "  Up/Down move ",
        MessageId::HelpFooterJump => " PgUp/PgDn jump ",
        MessageId::HelpFooterClose => " Esc close ",
        MessageId::CmdAnchorDescription => {
            "Pin a fact that survives compaction (auto-injected into context)"
        }
        MessageId::CmdAttachDescription => {
            "Attach image/video media; use @path for text files or directories"
        }
        MessageId::CmdCacheDescription => {
            "Show DeepSeek prefix-cache hit/miss stats for the last N turns"
        }
        MessageId::CmdChangeDescription => "Show the latest changelog entry",
        MessageId::CmdChangeHeader => "Latest Changelog",
        MessageId::CmdChangeTranslationQueued => {
            "English release notes are shown below. A translated version will be requested next; if the provider is unavailable, this English text is the fallback."
        }
        MessageId::CmdChangeTranslationUnavailable => {
            "English release notes are shown below. Translation is unavailable because the current session has no API key or is offline."
        }
        MessageId::CmdChangePreviousVersion => {
            "Previous version: {version} — run `/change {version}` to view it"
        }
        MessageId::CmdBalanceDescription => "Check the active provider account balance",
        MessageId::CmdClearDescription => "Clear conversation history",
        MessageId::CmdCompactDescription => {
            "Trigger context compaction to free up space (legacy; v0.6.6 prefers cycle restart)"
        }
        MessageId::CmdPurgeDescription => {
            "Let the agent surgically prune conversation history to free context space"
        }
        MessageId::CmdConfigDescription => "Open interactive configuration editor",
        MessageId::CmdContextDescription => "Open compact session context inspector",
        MessageId::CmdCostDescription => "Show session cost breakdown",
        MessageId::CmdCycleDescription => "Show the carry-forward briefing for a specific cycle",
        MessageId::CmdCyclesDescription => "List checkpoint-restart cycle handoffs in this session",
        MessageId::CmdDiffDescription => "Show file changes since session start",
        MessageId::CmdEditDescription => "Revise and resubmit the last message",
        MessageId::CmdExitDescription => "Exit the application",
        MessageId::CmdExportDescription => "Export conversation to markdown",
        MessageId::CmdFeedbackDescription => "Generate a GitHub feedback URL",
        MessageId::CmdHelpDescription => "Show help information",
        MessageId::CmdHomeDescription => "Show home dashboard with stats and quick actions",
        MessageId::CmdHooksDescription => "List configured lifecycle hooks (read-only)",
        MessageId::CmdAgentDescription => {
            "Open a persistent sub-agent session: /agent [0-3] <task>"
        }
        MessageId::CmdGoalDescription => "Set a session goal with optional token budget",
        MessageId::CmdInitDescription => "Generate AGENTS.md for project",
        MessageId::CmdLspDescription => "Toggle LSP diagnostics on or off",
        MessageId::CmdShareDescription => "Export current session as a shareable web URL",
        MessageId::CmdJobsDescription => "Inspect and control background commands",
        MessageId::CmdLinksDescription => "Show DeepSeek dashboard and docs links",
        MessageId::CmdLoadDescription => "Load session from file",
        MessageId::CmdLogoutDescription => "Clear API key and return to setup",
        MessageId::CmdMcpDescription => "Open or manage MCP servers",
        MessageId::CmdMemoryDescription => "Inspect or manage the persistent user-memory file",
        MessageId::CmdModeDescription => {
            "Switch mode or open picker: /mode [agent|plan|yolo|1|2|3]"
        }
        MessageId::CmdModelDescription => "Switch or view current model",
        MessageId::CmdModelsDescription => "List available models from API",
        MessageId::CmdNetworkDescription => "Manage network allow and deny rules",
        MessageId::CmdNoteDescription => "Add, list, edit, or remove workspace notes",
        MessageId::CmdThemeDescription => "Switch theme or open the theme picker",
        MessageId::CmdProviderDescription => {
            "Switch or view the active LLM backend (deepseek | nvidia-nim | ollama)"
        }
        MessageId::CmdQueueDescription => "View or edit queued messages",
        MessageId::CmdRecallDescription => "Search prior cycle archives (BM25 over message text)",
        MessageId::CmdRelayDescription => "Create a session relay (接力) for a fresh thread",
        MessageId::CmdRenameDescription => "Rename the current session",
        MessageId::CmdRestoreDescription => {
            "Roll back the workspace to a prior pre/post-turn snapshot. With no arg, lists recent snapshots."
        }
        MessageId::CmdRetryDescription => "Retry the last request",
        MessageId::CmdReviewDescription => "Run a structured code review on a file, diff, or PR",
        MessageId::CmdRlmDescription => "Open a persistent RLM context: /rlm [0-3] <file_or_text>",
        MessageId::CmdSaveDescription => "Save session to file",
        MessageId::CmdForkDescription => "Fork the active conversation into a sibling session",
        MessageId::CmdNewDescription => "Start a fresh saved session",
        MessageId::CmdSessionsDescription => "Open session history picker",
        MessageId::CmdSettingsDescription => "Show persistent settings",
        MessageId::CmdSkillDescription => {
            "Activate a skill, or install/update/uninstall/trust a community skill"
        }
        MessageId::CmdSkillsDescription => {
            "List local skills (filter by `/skills <prefix>`; --remote browses the curated registry)"
        }
        MessageId::CmdSlopDescription => "Inspect or export the SlopLedger",
        MessageId::CmdStashDescription => {
            "Park or restore a composer draft (Ctrl+S to push, /stash list/pop)"
        }
        MessageId::CmdStatusDescription => "Show runtime session status",
        MessageId::CmdStatuslineDescription => "Configure which items appear in the footer",
        MessageId::CmdSubagentsDescription => "List sub-agent status",
        MessageId::CmdSwarmDescription => {
            "Run a multi-agent fanout turn (sequential | mixture | distill | deliberate)"
        }
        MessageId::CmdSystemDescription => "Show current system prompt",
        MessageId::CmdTaskDescription => "Manage background tasks",
        MessageId::CmdTokensDescription => "Show token usage for session",
        MessageId::CmdTranslateDescription => {
            "Toggle output translation to the current system language on/off"
        }
        MessageId::CmdTranslateOff => "Output translation disabled (original model output shown)",
        MessageId::CmdTranslateOn => {
            "Output translation enabled: model responses will be shown in your system language"
        }
        MessageId::TranslationInProgress => "Translating assistant output...",
        MessageId::TranslationComplete => "Translation complete",
        MessageId::TranslationFailed => "Translation failed",
        MessageId::CmdTrustDescription => {
            "Manage workspace trust and per-path allowlist (`/trust add <path>`, `/trust list`, `/trust on|off`)"
        }
        MessageId::CmdWorkspaceDescription => "Show or switch the current workspace",
        MessageId::CmdUndoDescription => "Remove last message pair",
        MessageId::CmdVerboseDescription => "Toggle full live thinking in the transcript",
        MessageId::CmdCacheAdvice => {
            "Hit/miss ratios over ~70% after the third turn indicate a stable cache prefix; \n\
             lower than that on long sessions suggests prefix churn worth investigating (#263)."
        }
        MessageId::CmdCacheFootnote => {
            "* miss inferred from input − hit when the provider did not report it explicitly.\n"
        }
        MessageId::CmdCacheHeader => {
            "Cache telemetry — last {count} of {total} turn(s) (model: {model})\n"
        }
        MessageId::CmdCacheNoData => {
            "Cache history: no turns recorded yet.\n\n\
             DeepSeek surfaces `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` \
             on every API turn that the model supports it (V4 family). Run a turn \
             and try /cache again."
        }
        MessageId::CmdCacheTotals => {
            "Σ in: {sum_in}   Σ hit: {sum_hit}   Σ miss: {sum_miss}   avg hit ratio: {avg}\n"
        }
        MessageId::CmdCostReport => {
            "Session Cost:\n\
             ─────────────────────────────\n\
             Approx total spent: {cost}\n\n\
             Cost estimates are approximate and use provider usage telemetry when available.\n\n\
             DeepSeek API Pricing:\n\
             ─────────────────────────────\n\
             Pricing details are not configured in this CLI."
        }
        MessageId::CmdTokensCacheBoth => "{hit} hit / {miss} miss",
        MessageId::CmdTokensCacheHitOnly => "{hit} hit / miss not reported",
        MessageId::CmdTokensCacheMissOnly => "hit not reported / {miss} miss",
        MessageId::CmdTokensContextUnknownWindow => "~{estimated} / unknown window",
        MessageId::CmdTokensContextWithWindow => "~{used} / {window} ({percent}%)",
        MessageId::FooterAgentSingular => "1 agent",
        MessageId::FooterAgentsPlural => "{count} agents",
        MessageId::FooterPressCtrlCAgain => "Press Ctrl+C again to quit",
        MessageId::FooterWorking => "working",
        MessageId::FooterBalancePrefix => "bal",
        MessageId::HelpSectionActions => "Actions",
        MessageId::HelpSectionClipboard => "Clipboard",
        MessageId::HelpSectionEditing => "Input editing",
        MessageId::HelpSectionHelp => "Help",
        MessageId::HelpSectionModes => "Modes",
        MessageId::HelpSectionNavigation => "Navigation",
        MessageId::HelpSectionSessions => "Sessions",
        MessageId::CmdTokensNotReported => "not reported",
        MessageId::CmdTokensReport => {
            "Token Usage:\n\
             ─────────────────────────────\n\
             Active context:        {active}\n\
             Last API input:        {input} (turn telemetry; may count repeated prefix across tool rounds)\n\
             Last API output:       {output}\n\
             Cache hit/miss:        {cache} (telemetry/cost only)\n\
             Cumulative tokens:     {total} (session usage telemetry)\n\
             Approx session cost:   {cost}\n\
             API messages:          {api_messages}\n\
             Chat messages:         {chat_messages}\n\
             Model:                 {model}"
        }
        MessageId::KbScrollTranscript => {
            "Scroll transcript, navigate input history, or select composer attachments"
        }
        MessageId::KbNavigateHistory => "Navigate input history",
        MessageId::KbBrowseHistory => "Browse conversation history",
        MessageId::KbScrollTranscriptAlt => "Scroll transcript",
        MessageId::KbScrollPage => "Scroll transcript by page",
        MessageId::KbJumpTopBottom => "Jump to top / bottom of transcript",
        MessageId::KbJumpTopBottomEmpty => "Jump to top / bottom (when input is empty)",
        MessageId::KbJumpToolBlocks => "Jump between tool output blocks",
        MessageId::KbMoveCursor => "Move cursor in composer",
        MessageId::KbJumpLineStartEnd => "Jump to start / end of line",
        MessageId::KbDeleteChar => {
            "Delete character before / after the cursor, or remove selected attachment"
        }
        MessageId::KbClearDraft => "Clear the current draft",
        MessageId::KbStashDraft => "Stash the current draft (`/stash pop` to restore)",
        MessageId::KbSearchHistory => "Search prompt history and recover local drafts",
        MessageId::KbInsertNewline => "Insert a newline in the composer",
        MessageId::KbSendDraft => "Send the current draft",
        MessageId::KbCloseMenu => "Close menu, cancel request, discard draft, or clear input",
        MessageId::KbCancelOrExit => "Cancel request, or exit when idle",
        MessageId::KbShellControls => "Open shell controls for a running foreground command",
        MessageId::KbExitEmpty => "Exit when input is empty",
        MessageId::KbCommandPalette => "Open the command palette",
        MessageId::KbFuzzyFilePicker => "Open the fuzzy file picker (insert @path on Enter)",
        MessageId::KbCompactInspector => "Open compact session context inspector",
        MessageId::KbLastMessagePager => "Open pager for the last message (when input is empty)",
        MessageId::KbSelectedDetails => {
            "Open details for the selected tool or message (when input is empty)"
        }
        MessageId::KbToolDetailsPager => "Open tool-details pager",
        MessageId::KbThinkingPager => "Open Activity Detail",
        MessageId::KbLiveTranscript => "Open live transcript overlay (sticky-tail auto-scroll)",
        MessageId::KbBacktrackMessage => {
            "Backtrack to a previous user message (Left/Right step, Enter to rewind)"
        }
        MessageId::KbCompleteCycleModes => {
            "Complete /command, queue running-turn follow-up, cycle modes; Shift+Tab cycles reasoning effort"
        }
        MessageId::KbJumpPlanAgentYolo => "Jump directly to Plan / Agent / YOLO mode",
        MessageId::KbAltJumpPlanAgentYolo => "Alternative jump to Plan / Agent / YOLO mode",
        MessageId::KbFocusSidebar => {
            "Focus Work / Tasks / Agents / Context / Auto sidebar; Ctrl+Alt+0 hides it"
        }
        MessageId::KbTogglePlanAgent => "Toggle between Plan and Agent modes",
        MessageId::KbSessionPicker => "Open the session picker",
        MessageId::KbPasteAttach => "Paste text or attach a clipboard image",
        MessageId::KbCopySelection => "Copy the current selection (Cmd+C on macOS)",
        MessageId::KbContextMenu => {
            "Open context actions for paste, selection, message details, context, and help"
        }
        MessageId::KbAttachPath => "Add a local text file or directory to context",
        MessageId::KbHelpOverlay => "Open this help overlay (when input is empty)",
        MessageId::KbToggleHelp => "Toggle help overlay",
        MessageId::KbToggleHelpSlash => "Toggle help overlay",
        MessageId::HelpUsageLabel => "Usage:",
        MessageId::HelpAliasesLabel => "Aliases:",
        MessageId::SettingsTitle => "Settings:",
        MessageId::SettingsConfigFile => "Config file:",
        MessageId::ClearConversation => "Conversation cleared",
        MessageId::ClearConversationBusy => {
            "Conversation cleared (plan state busy; run /clear again if needed)"
        }
        MessageId::ModelChanged => "Model changed: {old} \u{2192} {new}",
        MessageId::LinksTitle => "DeepSeek Links:",
        MessageId::LinksDashboard => "Dashboard:",
        MessageId::LinksDocs => "Docs:",
        MessageId::LinksTip => "Tip: API keys are available in the dashboard console.",
        MessageId::SubagentsFetching => "Fetching sub-agent status...",
        MessageId::HelpUnknownCommand => "Unknown command: {topic}",
        MessageId::HomeDashboardTitle => "codesmith Home Dashboard",
        MessageId::HomeModel => "Model:",
        MessageId::HomeMode => "Mode:",
        MessageId::HomeWorkspace => "Workspace:",
        MessageId::HomeHistory => "History:",
        MessageId::HomeTokens => "Tokens:",
        MessageId::HomeQueued => "Queued:",
        MessageId::HomeSubagents => "Sub-agents:",
        MessageId::HomeSkill => "Skill:",
        MessageId::HomeQuickActions => "Quick Actions",
        MessageId::HomeQuickLinks => "/links      - Dashboard & API links",
        MessageId::HomeQuickSkills => "/skills      - List available skills",
        MessageId::HomeQuickConfig => "/config      - Open interactive configuration editor",
        MessageId::HomeQuickSettings => "/settings    - Show persistent settings",
        MessageId::HomeQuickModel => "/model       - Switch or view model",
        MessageId::HomeQuickSubagents => "/subagents   - List sub-agent status",
        MessageId::HomeQuickTaskList => "/task list   - Show background task queue",
        MessageId::HomeQuickHelp => "/help        - Show help",
        MessageId::HomeModeTips => "Mode Tips",
        MessageId::HomeAgentModeTip => "Agent mode - Use tools for autonomous tasks",
        MessageId::HomeAgentModeReviewTip => "  Use Ctrl+X to review in Plan mode before executing",
        MessageId::HomeAgentModeYoloTip => "  Type /mode yolo to enable full tool access",
        MessageId::HomeYoloModeTip => "YOLO mode - Full tool access, no approvals",
        MessageId::HomeYoloModeCaution => "  Be careful with destructive operations!",
        MessageId::HomePlanModeTip => "Plan mode - Design before implementing",
        MessageId::HomePlanModeChecklistTip => "  Use /mode plan to create structured checklists",
        MessageId::HomeGoalModeTip => "Goal tracking - Set /goal <objective> to pursue objectives",
        // Onboarding — language picker.
        MessageId::OnboardLanguageTitle => "Choose your language",
        MessageId::OnboardLanguageBlurb => {
            "Pick the UI language. You can change it any time with `/settings set locale <tag>`."
        }
        MessageId::OnboardLanguageFooter => {
            "Press 1-6 to choose, or Enter to keep the current setting"
        }
        // Onboarding — API key entry.
        MessageId::OnboardApiKeyTitle => "Connect your DeepSeek API key",
        MessageId::OnboardApiKeyStep1 => {
            "Step 1.  Open https://platform.deepseek.com/api_keys and create a key."
        }
        MessageId::OnboardApiKeyStep2 => "Step 2.  Paste it below and press Enter.",
        MessageId::OnboardApiKeySavedHint => {
            "Saved to ~/.codesmith/config.toml so it works from any folder."
        }
        MessageId::OnboardApiKeyFormatHint => {
            "Paste the full key exactly as issued (no spaces or newlines)."
        }
        MessageId::OnboardApiKeyPlaceholder => "(paste key here)",
        MessageId::OnboardApiKeyLabel => "Key: ",
        MessageId::OnboardApiKeyFooter => "Press Enter to save, Esc to go back.",
        // Onboarding — workspace trust.
        MessageId::OnboardTrustTitle => "Trust Workspace",
        MessageId::OnboardTrustQuestion => "Do you trust the contents of this directory?",
        MessageId::OnboardTrustLocationPrefix => "You are in ",
        MessageId::OnboardTrustRiskHint => {
            "Working with untrusted contents comes with higher risk of prompt injection."
        }
        MessageId::OnboardTrustEffectHint => {
            "Trusting this directory records it in global config and enables trusted workspace mode."
        }
        MessageId::OnboardTrustFooterPrefix => "Press ",
        MessageId::OnboardTrustFooterMiddle => " to trust and continue, ",
        MessageId::OnboardTrustFooterSuffix => " to quit",
        // Onboarding — final tips.
        MessageId::OnboardTipsTitle => "Start Simple",
        MessageId::OnboardTipsLine1 => {
            "Write the task in plain language. Use /help or Ctrl+K when you want a command."
        }
        MessageId::OnboardTipsLine2 => {
            "The bottom composer is multi-line: Enter sends, Alt+Enter or Ctrl+J adds a new line."
        }
        MessageId::OnboardTipsLine3 => {
            "Switch modes only when the job changes: Plan for review-first work, Agent for execution, YOLO when you want auto-approval."
        }
        MessageId::OnboardTipsLine4 => {
            "Ctrl+R resumes earlier sessions, and Esc backs out of the current draft or overlay."
        }
        MessageId::OnboardTipsFooterEnter => "Press Enter",
        MessageId::OnboardTipsFooterAction => " to open the workspace",
        // Context menu.
        MessageId::CtxMenuTitle => " Right click ",
        MessageId::CtxMenuCopySelection => "Copy selection",
        MessageId::CtxMenuCopySelectionDesc => "write selected transcript text",
        MessageId::CtxMenuOpenSelection => "Open selection",
        MessageId::CtxMenuOpenSelectionDesc => "show selected text in pager",
        MessageId::CtxMenuClearSelection => "Clear selection",
        MessageId::CtxMenuOpenDetails => "Open details",
        MessageId::CtxMenuCopyMessage => "Copy message",
        MessageId::CtxMenuCopyMessageDesc => "write clicked transcript cell",
        MessageId::CtxMenuOpenInEditor => "Open in editor",
        MessageId::CtxMenuOpenInEditorDesc => "open file:line in $EDITOR",
        MessageId::CtxMenuShowCell => "Show cell",
        MessageId::CtxMenuShowCellDesc => "unhide this transcript cell",
        MessageId::CtxMenuHideCell => "Hide cell",
        MessageId::CtxMenuHideCellDesc => "collapse this transcript cell",
        MessageId::CtxMenuShowHidden => "Show hidden",
        MessageId::CtxMenuShowHiddenDesc => "unhide all collapsed cells",
        MessageId::CtxMenuPaste => "Paste",
        MessageId::CtxMenuPasteDesc => "insert clipboard into composer",
        MessageId::CtxMenuCmdPalette => "Command palette",
        MessageId::CtxMenuCmdPaletteDesc => "commands, skills, and tools",
        MessageId::CtxMenuContextInspector => "Context inspector",
        MessageId::CtxMenuContextInspectorDesc => "active context and cache hints",
        MessageId::CtxMenuHelp => "Help",
        MessageId::CtxMenuHelpDesc => "keybindings and commands",
    }
}

fn translation(locale: Locale, id: MessageId) -> Option<&'static str> {
    match locale {
        Locale::En => Some(english(id)),
        Locale::ZhHans => chinese_simplified(id),
        Locale::ZhHant => traditional_chinese(id),
        Locale::Hi => hindi(id),
        Locale::Es419 => spanish_latin_america(id),
    }
}

fn traditional_chinese(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::CmdRelayDescription => "為新執行緒建立會話接力摘要",
        MessageId::CmdTranslateDescription => "切換輸出翻譯為目前系統語言的開關狀態",
        MessageId::CmdTranslateOff => "輸出翻譯已關閉（顯示原始模型輸出）",
        MessageId::CmdTranslateOn => "輸出翻譯已開啟：模型回覆將以繁體中文顯示",
        MessageId::TranslationInProgress => "正在翻譯助理輸出...",
        MessageId::TranslationComplete => "翻譯完成",
        MessageId::TranslationFailed => "翻譯失敗",
        MessageId::FooterBalancePrefix => "餘額",
        other => chinese_simplified(other)?,
    })
}

fn chinese_simplified(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::ComposerPlaceholder => "编写任务或使用 /。",
        MessageId::HistorySearchPlaceholder => "搜索提示历史...",
        MessageId::HistorySearchTitle => "历史搜索",
        MessageId::HistoryHintMove => "Up/Down 移动",
        MessageId::HistoryHintAccept => "Enter 接受",
        MessageId::HistoryHintRestore => "Esc 还原",
        MessageId::HistoryNoMatches => "  无匹配",
        MessageId::ConfigTitle => "会话配置",
        MessageId::ConfigModalTitle => " 配置 ",
        MessageId::ConfigSearchPlaceholder => "输入以筛选",
        MessageId::ConfigNoSettings => "  没有可用设置。",
        MessageId::ConfigNoMatchesPrefix => "  没有匹配设置: ",
        MessageId::ConfigFilteredSettings => "  已筛选设置",
        MessageId::ConfigShowing => "  显示",
        MessageId::ConfigFooterDefault => " 输入=筛选, Up/Down=选择, Enter/e=编辑, Esc/q=关闭 ",
        MessageId::ConfigFooterScrollable => {
            " 输入=筛选, Up/Down=选择, Enter/e=编辑, PgUp/PgDn=滚动, Esc/q=关闭 "
        }
        MessageId::ConfigFooterFiltered => {
            " 输入=筛选, Backspace=删除, Ctrl+U/Esc=清除, Enter=编辑 "
        }
        MessageId::HelpTitle => "帮助",
        MessageId::HelpFilterPlaceholder => "输入以筛选",
        MessageId::HelpFilterPrefix => "筛选: ",
        MessageId::HelpNoMatches => "  无匹配。",
        MessageId::HelpSlashCommands => "斜杠命令",
        MessageId::HelpKeybindings => "快捷键",
        MessageId::HelpFooterTypeFilter => " 输入以筛选 ",
        MessageId::HelpFooterMove => "  Up/Down 移动 ",
        MessageId::HelpFooterJump => " PgUp/PgDn 跳转 ",
        MessageId::HelpFooterClose => " Esc 关闭 ",
        MessageId::CmdAnchorDescription => "钉选关键事实，在压缩后自动注入上下文",
        MessageId::CmdAttachDescription => "附加图片或视频媒体；文本文件或目录请使用 @path",
        MessageId::CmdCacheDescription => "显示最近 N 轮的 DeepSeek 前缀缓存命中/未命中统计",
        MessageId::CmdChangeDescription => "显示最新的更新日志",
        MessageId::CmdChangeHeader => "最新更新日志",
        MessageId::CmdChangeTranslationQueued => {
            "下面显示英文发布说明。接下来会请求模型翻译；如果当前提供商不可用，这段英文内容就是备用结果。"
        }
        MessageId::CmdChangeTranslationUnavailable => {
            "下面显示英文发布说明。当前会话没有 API Key 或处于离线状态，无法翻译。"
        }
        MessageId::CmdChangePreviousVersion => {
            "上一个版本: {version} —— 输入 `/change {version}` 查看"
        }
        MessageId::CmdBalanceDescription => "查看当前提供商账户余额",
        MessageId::CmdClearDescription => "清除对话历史",
        MessageId::CmdCompactDescription => {
            "触发上下文压缩以释放空间（旧版命令；v0.6.6 起建议改用循环重启）"
        }
        MessageId::CmdPurgeDescription => "让 Agent 分析对话历史，精确保留有用信息并移除冗余内容",
        MessageId::CmdConfigDescription => "打开交互式配置编辑器",
        MessageId::CmdContextDescription => "打开紧凑会话上下文检查器",
        MessageId::CmdCostDescription => "显示本次会话的费用明细",
        MessageId::CmdCycleDescription => "显示指定循环的延续简报",
        MessageId::CmdCyclesDescription => "列出本次会话中的检查点重启循环交接",
        MessageId::CmdDiffDescription => "显示会话开始以来的文件变更",
        MessageId::CmdEditDescription => "修改并重新提交最后一条消息",
        MessageId::CmdExitDescription => "退出应用",
        MessageId::CmdExportDescription => "将对话导出为 Markdown",
        MessageId::CmdFeedbackDescription => "生成 GitHub 反馈链接",
        MessageId::CmdHelpDescription => "显示帮助信息",
        MessageId::CmdHomeDescription => "显示主页面板，含统计与快捷操作",
        MessageId::CmdHooksDescription => "列出已配置的生命周期钩子（只读）",
        MessageId::CmdAgentDescription => "打开持久子代理会话：/agent [0-3] <task>",
        MessageId::CmdGoalDescription => "设置带有可选令牌预算的会话目标",
        MessageId::CmdInitDescription => "为项目生成 AGENTS.md",
        MessageId::CmdLspDescription => "切换 LSP 诊断的开启或关闭",
        MessageId::CmdShareDescription => "将当前会话导出为可共享的 Web URL",
        MessageId::CmdJobsDescription => "查看并管理后台 shell 作业",
        MessageId::CmdLinksDescription => "显示 DeepSeek 控制台与文档链接",
        MessageId::CmdLoadDescription => "从文件加载会话",
        MessageId::CmdLogoutDescription => "清除 API 密钥并返回设置",
        MessageId::CmdMcpDescription => "打开或管理 MCP 服务器",
        MessageId::CmdMemoryDescription => "查看或管理持久用户记忆文件",
        MessageId::CmdModeDescription => "切换运行模式或打开选择器：/mode [agent|plan|yolo|1|2|3]",
        MessageId::CmdModelDescription => "切换或查看当前模型",
        MessageId::CmdModelsDescription => "列出 API 中可用的模型",
        MessageId::CmdNetworkDescription => "管理网络允许和拒绝规则",
        MessageId::CmdNoteDescription => "添加、列出、编辑或删除工作区笔记",
        MessageId::CmdThemeDescription => "切换主题：深色、浅色、灰度或系统",
        MessageId::CmdProviderDescription => {
            "切换或查看当前 LLM 后端（deepseek | nvidia-nim | ollama）"
        }
        MessageId::CmdQueueDescription => "查看或编辑已排队的消息",
        MessageId::CmdRecallDescription => "搜索此前的循环归档（基于消息文本的 BM25 检索）",
        MessageId::CmdRelayDescription => "为新线程创建会话接力摘要",
        MessageId::CmdRenameDescription => "重命名当前会话",
        MessageId::CmdRestoreDescription => {
            "将工作区回滚到此前的轮次前/后快照。不带参数时列出最近的快照。"
        }
        MessageId::CmdRetryDescription => "重试上一次请求",
        MessageId::CmdReviewDescription => "对文件、diff 或 PR 进行结构化代码审查",
        MessageId::CmdRlmDescription => "打开持久 RLM 上下文：/rlm [0-3] <file_or_text>",
        MessageId::CmdSaveDescription => "将会话保存到文件",
        MessageId::CmdForkDescription => "将当前对话分叉为兄弟会话",
        MessageId::CmdNewDescription => "开始一个新的已保存会话",
        MessageId::CmdSessionsDescription => "打开会话历史选择器",
        MessageId::CmdSettingsDescription => "显示持久化设置",
        MessageId::CmdSkillDescription => "激活技能，或安装/更新/卸载/信任社区技能",
        MessageId::CmdSkillsDescription => {
            "列出本地技能（用 `/skills <prefix>` 按名称前缀过滤，--remote 浏览精选注册表）"
        }
        MessageId::CmdSlopDescription => "Inspect or export the SlopLedger",
        MessageId::CmdStashDescription => "暂存或恢复输入草稿（Ctrl+S 暂存，/stash list|pop）",
        MessageId::CmdStatusDescription => "显示当前运行状态",
        MessageId::CmdStatuslineDescription => "配置底栏要显示哪些条目",
        MessageId::CmdSubagentsDescription => "列出子代理状态",
        MessageId::CmdSwarmDescription => {
            "运行多代理扇出轮次（sequential | mixture | distill | deliberate）"
        }
        MessageId::CmdSystemDescription => "显示当前系统提示词",
        MessageId::CmdTaskDescription => "管理后台任务",
        MessageId::CmdTokensDescription => "显示本次会话的 token 用量",
        MessageId::CmdTranslateDescription => "切换输出翻译为当前系统语言的开/关状态",
        MessageId::CmdTranslateOff => "输出翻译已关闭（显示原始模型输出）",
        MessageId::CmdTranslateOn => "输出翻译已开启：模型回复将以当前系统语言显示",
        MessageId::TranslationInProgress => "正在翻译助手输出...",
        MessageId::TranslationComplete => "翻译完成",
        MessageId::TranslationFailed => "翻译失败",
        MessageId::CmdTrustDescription => {
            "管理工作区信任与按路径的白名单（`/trust add <path>`、`/trust list`、`/trust on|off`）"
        }
        MessageId::CmdWorkspaceDescription => "显示或切换当前工作空间",
        MessageId::CmdUndoDescription => "移除最后一组消息对",
        MessageId::CmdVerboseDescription => "切换实时思考内容的完整显示",
        MessageId::CmdCacheAdvice => {
            "第 3 轮起命中率稳定在 ~70% 以上即表示前缀缓存稳定；\n\
             长会话中明显偏低则意味着前缀有抖动，值得排查（#263）。"
        }
        MessageId::CmdCacheFootnote => "* 当提供方未单独上报未命中时，由「输入 − 命中」推算。\n",
        MessageId::CmdCacheHeader => "缓存遥测 —— 最近 {count} / {total} 轮（模型：{model}）\n",
        MessageId::CmdCacheNoData => {
            "缓存历史：尚未记录任何轮次。\n\n\
             DeepSeek 在受支持的模型（V4 系列）每个 API 轮次都会返回 `prompt_cache_hit_tokens` / \
             `prompt_cache_miss_tokens`。请先运行一个轮次再试 /cache。"
        }
        MessageId::CmdCacheTotals => {
            "Σ 输入：{sum_in}   Σ 命中：{sum_hit}   Σ 未命中：{sum_miss}   平均命中率：{avg}\n"
        }
        MessageId::CmdCostReport => {
            "会话费用：\n\
             ─────────────────────────────\n\
             预估累计消耗：{cost}\n\n\
             费用为估算值；如有提供方用量遥测会优先使用。\n\n\
             DeepSeek API 计费：\n\
             ─────────────────────────────\n\
             此 CLI 中未配置详细计费规则。"
        }
        MessageId::CmdTokensCacheBoth => "命中 {hit} / 未命中 {miss}",
        MessageId::CmdTokensCacheHitOnly => "命中 {hit} / 未命中未上报",
        MessageId::CmdTokensCacheMissOnly => "命中未上报 / 未命中 {miss}",
        MessageId::CmdTokensContextUnknownWindow => "~{estimated} / 窗口未知",
        MessageId::CmdTokensContextWithWindow => "~{used} / {window}（{percent}%）",
        MessageId::FooterAgentSingular => "1 个子代理",
        MessageId::FooterAgentsPlural => "{count} 个子代理",
        MessageId::FooterPressCtrlCAgain => "再次按 Ctrl+C 退出",
        MessageId::FooterWorking => "工作中",
        MessageId::FooterBalancePrefix => "余额",
        MessageId::HelpSectionActions => "操作",
        MessageId::HelpSectionClipboard => "剪贴板",
        MessageId::HelpSectionEditing => "输入编辑",
        MessageId::HelpSectionHelp => "帮助",
        MessageId::HelpSectionModes => "模式",
        MessageId::HelpSectionNavigation => "导航",
        MessageId::HelpSectionSessions => "会话",
        MessageId::CmdTokensNotReported => "未上报",
        MessageId::CmdTokensReport => {
            "令牌用量：\n\
             ─────────────────────────────\n\
             活动上下文：       {active}\n\
             上次 API 输入：    {input}（来自轮次遥测；多轮工具调用中相同前缀可能被重复计入）\n\
             上次 API 输出：    {output}\n\
             缓存命中/未命中：  {cache}（仅用于遥测/计费）\n\
             累计令牌：         {total}（会话用量遥测）\n\
             预估会话费用：     {cost}\n\
             API 消息数：       {api_messages}\n\
             聊天消息数：       {chat_messages}\n\
             模型：             {model}"
        }
        MessageId::KbScrollTranscript => "滚动对话记录、浏览输入历史或选择附件",
        MessageId::KbNavigateHistory => "浏览输入历史",
        MessageId::KbBrowseHistory => "浏览对话历史",
        MessageId::KbScrollTranscriptAlt => "滚动对话记录",
        MessageId::KbScrollPage => "按页滚动对话记录",
        MessageId::KbJumpTopBottom => "跳转到对话顶部/底部",
        MessageId::KbJumpTopBottomEmpty => "跳转到顶部/底部（输入框为空时）",
        MessageId::KbJumpToolBlocks => "在工具输出块之间跳转",
        MessageId::KbMoveCursor => "在输入框中移动光标",
        MessageId::KbJumpLineStartEnd => "跳转到行首/行尾",
        MessageId::KbDeleteChar => "删除光标前/后的字符，或移除已选附件",
        MessageId::KbClearDraft => "清空当前草稿",
        MessageId::KbStashDraft => "暂存当前草稿（用 `/stash pop` 恢复）",
        MessageId::KbSearchHistory => "搜索提示历史并恢复本地草稿",
        MessageId::KbInsertNewline => "在输入框中插入换行",
        MessageId::KbSendDraft => "发送当前草稿",
        MessageId::KbCloseMenu => "关闭菜单、取消请求、丢弃草稿或清空输入",
        MessageId::KbCancelOrExit => "取消请求，或空闲时退出",
        MessageId::KbShellControls => "打开正在运行的前台命令的 shell 控制",
        MessageId::KbExitEmpty => "输入框为空时退出",
        MessageId::KbCommandPalette => "打开命令面板",
        MessageId::KbFuzzyFilePicker => "打开模糊文件选择器（按 Enter 插入 @path）",
        MessageId::KbCompactInspector => "打开紧凑会话上下文检查器",
        MessageId::KbLastMessagePager => "打开最后一条消息的分页器（输入框为空时）",
        MessageId::KbSelectedDetails => "打开选中工具或消息的详情（输入框为空时）",
        MessageId::KbToolDetailsPager => "打开工具详情分页器",
        MessageId::KbThinkingPager => "打开 Activity Detail",
        MessageId::KbLiveTranscript => "打开实时对话覆盖层（自动滚动尾随）",
        MessageId::KbBacktrackMessage => "回退到之前的用户消息（左右键步进，Enter 回退）",
        MessageId::KbCompleteCycleModes => {
            "补全 /command、排队运行轮次跟进、切换模式；Shift+Tab 切换推理强度"
        }
        MessageId::KbJumpPlanAgentYolo => "直接跳转到 Plan / Agent / YOLO 模式",
        MessageId::KbAltJumpPlanAgentYolo => "替代快捷键跳转到 Plan / Agent / YOLO 模式",
        MessageId::KbFocusSidebar => "聚焦 Work / 任务 / 代理 / Context / 自动 / 隐藏侧边栏",
        MessageId::KbTogglePlanAgent => "在 Plan 和 Agent 模式之间切换",
        MessageId::KbSessionPicker => "打开会话选择器",
        MessageId::KbPasteAttach => "粘贴文本或附加剪贴板图片",
        MessageId::KbCopySelection => "复制当前选中内容（macOS 为 Cmd+C）",
        MessageId::KbContextMenu => "打开上下文操作菜单，用于粘贴、选择、消息详情、上下文和帮助",
        MessageId::KbAttachPath => "添加本地文本文件或目录到上下文",
        MessageId::KbHelpOverlay => "打开此帮助覆盖层（输入框为空时）",
        MessageId::KbToggleHelp => "切换帮助覆盖层",
        MessageId::KbToggleHelpSlash => "切换帮助覆盖层",
        MessageId::HelpUsageLabel => "用法：",
        MessageId::HelpAliasesLabel => "别名：",
        MessageId::SettingsTitle => "设置：",
        MessageId::SettingsConfigFile => "配置文件：",
        MessageId::ClearConversation => "对话已清空",
        MessageId::ClearConversationBusy => {
            "对话已清空（Plan 状态忙碌；如需再次清空请运行 /clear）"
        }
        MessageId::ModelChanged => "模型已切换：{old} \u{2192} {new}",
        MessageId::LinksTitle => "DeepSeek 链接：",
        MessageId::LinksDashboard => "控制台：",
        MessageId::LinksDocs => "文档：",
        MessageId::LinksTip => "提示：API 密钥可在控制台中获取。",
        MessageId::SubagentsFetching => "正在获取子代理状态...",
        MessageId::HelpUnknownCommand => "未知命令：{topic}",
        MessageId::HomeDashboardTitle => "codesmith 主面板",
        MessageId::HomeModel => "模型：",
        MessageId::HomeMode => "模式：",
        MessageId::HomeWorkspace => "工作区：",
        MessageId::HomeHistory => "历史：",
        MessageId::HomeTokens => "令牌：",
        MessageId::HomeQueued => "队列：",
        MessageId::HomeSubagents => "子代理：",
        MessageId::HomeSkill => "技能：",
        MessageId::HomeQuickActions => "快捷操作",
        MessageId::HomeQuickLinks => "/links      - 控制台与 API 链接",
        MessageId::HomeQuickSkills => "/skills      - 列出可用技能",
        MessageId::HomeQuickConfig => "/config      - 打开交互式配置编辑器",
        MessageId::HomeQuickSettings => "/settings    - 显示持久化设置",
        MessageId::HomeQuickModel => "/model       - 切换或查看模型",
        MessageId::HomeQuickSubagents => "/subagents   - 列出子代理状态",
        MessageId::HomeQuickTaskList => "/task list   - 显示后台任务队列",
        MessageId::HomeQuickHelp => "/help        - 显示帮助",
        MessageId::HomeModeTips => "模式提示",
        MessageId::HomeAgentModeTip => "Agent 模式 - 使用工具执行自主任务",
        MessageId::HomeAgentModeReviewTip => "  按 Ctrl+X 可在 Plan 模式下审查后再执行",
        MessageId::HomeAgentModeYoloTip => "  输入 /mode yolo 启用完整工具访问",
        MessageId::HomeYoloModeTip => "YOLO 模式 - 完整工具访问，无需审批",
        MessageId::HomeYoloModeCaution => "  请小心破坏性操作！",
        MessageId::HomePlanModeTip => "Plan 模式 - 先设计再实现",
        MessageId::HomePlanModeChecklistTip => "  使用 /mode plan 创建结构化检查清单",
        MessageId::HomeGoalModeTip => "Goal 跟踪 - 设置 /goal <目标> 以跟踪持久目标",
        // Onboarding — language picker.
        MessageId::OnboardLanguageTitle => "选择语言",
        MessageId::OnboardLanguageBlurb => {
            "选择界面语言。可随时使用 `/settings set locale <tag>` 修改。"
        }
        MessageId::OnboardLanguageFooter => "按 1-6 选择，或按 Enter 保留当前设置",
        // Onboarding — API key entry.
        MessageId::OnboardApiKeyTitle => "连接你的 DeepSeek API 密钥",
        MessageId::OnboardApiKeyStep1 => {
            "步骤 1.  打开 https://platform.deepseek.com/api_keys 创建一个密钥。"
        }
        MessageId::OnboardApiKeyStep2 => "步骤 2.  把密钥粘贴到下方并按 Enter。",
        MessageId::OnboardApiKeySavedHint => {
            "保存到 ~/.codesmith/config.toml，因此在任何目录下都生效。"
        }
        MessageId::OnboardApiKeyFormatHint => "请完整粘贴密钥（不要含空格或换行）。",
        MessageId::OnboardApiKeyPlaceholder => "（在此粘贴密钥）",
        MessageId::OnboardApiKeyLabel => "密钥: ",
        MessageId::OnboardApiKeyFooter => "Enter 保存，Esc 返回。",
        // Onboarding — workspace trust.
        MessageId::OnboardTrustTitle => "信任工作目录",
        MessageId::OnboardTrustQuestion => "你信任此目录中的内容吗？",
        MessageId::OnboardTrustLocationPrefix => "当前位置：",
        MessageId::OnboardTrustRiskHint => "处理不受信任的内容会增加提示词注入的风险。",
        MessageId::OnboardTrustEffectHint => {
            "信任此目录会记录在全局配置中，并启用受信任工作区模式。"
        }
        MessageId::OnboardTrustFooterPrefix => "按 ",
        MessageId::OnboardTrustFooterMiddle => " 信任并继续，",
        MessageId::OnboardTrustFooterSuffix => " 退出",
        // Onboarding — final tips.
        MessageId::OnboardTipsTitle => "从简开始",
        MessageId::OnboardTipsLine1 => "用自然语言描述任务。需要命令时使用 /help 或 Ctrl+K。",
        MessageId::OnboardTipsLine2 => "底部输入框支持多行：Enter 发送，Alt+Enter 或 Ctrl+J 换行。",
        MessageId::OnboardTipsLine3 => {
            "按需切换模式：Plan 适合先审后行，Agent 用于执行，YOLO 启用自动批准。"
        }
        MessageId::OnboardTipsLine4 => "Ctrl+R 恢复历史会话，Esc 退出当前输入或弹层。",
        MessageId::OnboardTipsFooterEnter => "按 Enter",
        MessageId::OnboardTipsFooterAction => " 进入工作区",
        // Context menu.
        MessageId::CtxMenuTitle => " 右键菜单 ",
        MessageId::CtxMenuCopySelection => "复制所选",
        MessageId::CtxMenuCopySelectionDesc => "将选中的记录区域文本写入剪贴板",
        MessageId::CtxMenuOpenSelection => "打开所选",
        MessageId::CtxMenuOpenSelectionDesc => "在翻阅器中查看选中文本",
        MessageId::CtxMenuClearSelection => "清除选择",
        MessageId::CtxMenuOpenDetails => "打开详情",
        MessageId::CtxMenuCopyMessage => "复制消息",
        MessageId::CtxMenuCopyMessageDesc => "将点击的记录条目写入剪贴板",
        MessageId::CtxMenuOpenInEditor => "在编辑器中打开",
        MessageId::CtxMenuOpenInEditorDesc => "在 $EDITOR 中打开 file:line",
        MessageId::CtxMenuShowCell => "显示条目",
        MessageId::CtxMenuShowCellDesc => "取消隐藏此记录条目",
        MessageId::CtxMenuHideCell => "隐藏条目",
        MessageId::CtxMenuHideCellDesc => "折叠此记录条目",
        MessageId::CtxMenuShowHidden => "显示已隐藏",
        MessageId::CtxMenuShowHiddenDesc => "取消隐藏所有已折叠条目",
        MessageId::CtxMenuPaste => "粘贴",
        MessageId::CtxMenuPasteDesc => "将剪贴板插入输入框",
        MessageId::CtxMenuCmdPalette => "命令面板",
        MessageId::CtxMenuCmdPaletteDesc => "命令、技能和工具",
        MessageId::CtxMenuContextInspector => "上下文检查器",
        MessageId::CtxMenuContextInspectorDesc => "活动上下文和缓存提示",
        MessageId::CtxMenuHelp => "帮助",
        MessageId::CtxMenuHelpDesc => "快捷键和命令",
    })
}

fn hindi(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::ComposerPlaceholder => "कोई काम लिखें या / का उपयोग करें।",
        MessageId::HistorySearchPlaceholder => "प्रॉम्प्ट इतिहास खोजें...",
        MessageId::HistorySearchTitle => "इतिहास खोज",
        MessageId::HistoryHintMove => "Up/Down से घुमें",
        MessageId::HistoryHintAccept => "Enter स्वीकारें",
        MessageId::HistoryHintRestore => "Esc पुनर्स्थापित",
        MessageId::HistoryNoMatches => "  कोई मिलान नहीं",
        MessageId::ConfigTitle => "सत्र कॉन्फ़िगरेशन",
        MessageId::ConfigModalTitle => " कॉन्फ़िग ",
        MessageId::ConfigSearchPlaceholder => "फ़िल्टर के लिए टाइप करें",
        MessageId::ConfigNoSettings => "  कोई सेटिंग उपलब्ध नहीं।",
        MessageId::ConfigNoMatchesPrefix => "  कोई सेटिंग मेल नहीं खाती ",
        MessageId::ConfigFilteredSettings => "  फ़िल्टर की गई सेटिंग",
        MessageId::ConfigShowing => "  दिखा रहे हैं",
        MessageId::ConfigFooterDefault => " टाइप=फ़िल्टर, Up/Down=चुनें, Enter/e=संपादित, Esc/q=बंद ",
        MessageId::ConfigFooterScrollable => {
            " टाइप=फ़िल्टर, Up/Down=चुनें, Enter/e=संपादित, PgUp/PgDn=स्क्रॉल, Esc/q=बंद "
        }
        MessageId::ConfigFooterFiltered => {
            " टाइप=फ़िल्टर, Backspace=मिटाएँ, Ctrl+U/Esc=साफ़, Enter=संपादित "
        }
        MessageId::HelpTitle => "सहायता",
        MessageId::HelpFilterPlaceholder => "फ़िल्टर के लिए टाइप करें",
        MessageId::HelpFilterPrefix => "फ़िल्टर: ",
        MessageId::HelpNoMatches => "  कोई मिलान नहीं।",
        MessageId::HelpSlashCommands => "स्लैश कमांड",
        MessageId::HelpKeybindings => "कीबाइंडिंग",
        MessageId::HelpFooterTypeFilter => " फ़िल्टर के लिए टाइप करें ",
        MessageId::HelpFooterMove => "  Up/Down घुमाएँ ",
        MessageId::HelpFooterJump => " PgUp/PgDn जाएँ ",
        MessageId::HelpFooterClose => " Esc बंद ",
        MessageId::CmdAnchorDescription => {
            "ऐसा तथ्य पिन करें जो compaction में बचा रहे (context में स्वतः जुड़ता है)"
        }
        MessageId::CmdAttachDescription => {
            "इमेज/वीडियो जोड़ें; टेक्स्ट फ़ाइलों या डायरेक्टरी के लिए @path उपयोग करें"
        }
        MessageId::CmdCacheDescription => "पिछले N टर्न के DeepSeek prefix-cache hit/miss आँकड़े दिखाएँ",
        MessageId::CmdChangeDescription => "नवीनतम changelog प्रविष्टि दिखाएँ",
        MessageId::CmdChangeHeader => "नवीनतम Changelog",
        MessageId::CmdChangeTranslationQueued => {
            "नीचे अंग्रेज़ी रिलीज़ नोट्स दिखाए गए हैं। अनुवादित संस्करण अगले चरण में माँगा जाएगा; यदि provider उपलब्ध न हो तो यह अंग्रेज़ी पाठ ही विकल्प है।"
        }
        MessageId::CmdChangeTranslationUnavailable => {
            "नीचे अंग्रेज़ी रिलीज़ नोट्स दिखाए गए हैं। अनुवाद उपलब्ध नहीं है क्योंकि वर्तमान सत्र में API key नहीं है या यह ऑफ़लाइन है।"
        }
        MessageId::CmdChangePreviousVersion => {
            "पिछला संस्करण: {version} — देखने के लिए `/change {version}` चलाएँ"
        }
        MessageId::CmdBalanceDescription => "सक्रिय provider खाते का बैलेंस देखें",
        MessageId::CmdClearDescription => "बातचीत का इतिहास साफ़ करें",
        MessageId::CmdCompactDescription => {
            "जगह खाली करने के लिए context compaction चलाएँ (पुराना; v0.6.6 में cycle restart बेहतर है)"
        }
        MessageId::CmdPurgeDescription => {
            "context की जगह खाली करने के लिए agent को बातचीत इतिहास सटीक रूप से छँटने दें"
        }
        MessageId::CmdConfigDescription => "इंटरैक्टिव कॉन्फ़िगरेशन संपादक खोलें",
        MessageId::CmdContextDescription => "कॉम्पैक्ट सत्र context इंस्पेक्टर खोलें",
        MessageId::CmdCostDescription => "सत्र की लागत का विवरण दिखाएँ",
        MessageId::CmdCycleDescription => "किसी विशेष cycle का carry-forward ब्रीफिंग दिखाएँ",
        MessageId::CmdCyclesDescription => "इस सत्र के checkpoint-restart cycle हैंडऑफ़ दिखाएँ",
        MessageId::CmdDiffDescription => "सत्र प्रारंभ से फ़ाइल परिवर्तन दिखाएँ",
        MessageId::CmdEditDescription => "अंतिम संदेश संशोधित कर दोबारा भेजें",
        MessageId::CmdExitDescription => "ऐप्लिकेशन से बाहर निकलें",
        MessageId::CmdExportDescription => "बातचीत markdown में निर्यात करें",
        MessageId::CmdFeedbackDescription => "GitHub फ़ीडबैक URL बनाएँ",
        MessageId::CmdHelpDescription => "सहायता जानकारी दिखाएँ",
        MessageId::CmdHomeDescription => "आँकड़ों और त्वरित क्रियाओं सहित होम डैशबोर्ड दिखाएँ",
        MessageId::CmdHooksDescription => "कॉन्फ़िगर किए गए lifecycle hooks दिखाएँ (केवल-पठनीय)",
        MessageId::CmdAgentDescription => "स्थायी sub-agent सत्र खोलें: /agent [0-3] <task>",
        MessageId::CmdGoalDescription => "वैकल्पिक token बजट के साथ सत्र लक्ष्य सेट करें",
        MessageId::CmdInitDescription => "प्रोजेक्ट के लिए AGENTS.md बनाएँ",
        MessageId::CmdLspDescription => "LSP डायग्नोस्टिक्स चालू या बंद करें",
        MessageId::CmdShareDescription => "वर्तमान सत्र को साझा करने योग्य web URL के रूप में निर्यात करें",
        MessageId::CmdJobsDescription => "बैकग्राउंड कमांड देखें और नियंत्रित करें",
        MessageId::CmdLinksDescription => "DeepSeek डैशबोर्ड और docs लिंक दिखाएँ",
        MessageId::CmdLoadDescription => "फ़ाइल से सत्र लोड करें",
        MessageId::CmdLogoutDescription => "API key साफ़ कर सेटअप पर लौटें",
        MessageId::CmdMcpDescription => "MCP सर्वर खोलें या प्रबंधित करें",
        MessageId::CmdMemoryDescription => "स्थायी user-memory फ़ाइल देखें या प्रबंधित करें",
        MessageId::CmdModeDescription => "मोड बदलें या पिकर खोलें: /mode [agent|plan|yolo|1|2|3]",
        MessageId::CmdModelDescription => "वर्तमान मॉडल बदलें या देखें",
        MessageId::CmdModelsDescription => "API से उपलब्ध मॉडल दिखाएँ",
        MessageId::CmdNetworkDescription => "नेटवर्क allow और deny नियम प्रबंधित करें",
        MessageId::CmdNoteDescription => "workspace नोट्स जोड़ें, देखें, संपादित करें या हटाएँ",
        MessageId::CmdThemeDescription => "थीम बदलें या theme picker खोलें",
        MessageId::CmdProviderDescription => {
            "सक्रिय LLM बैकएंड बदलें या देखें (deepseek | nvidia-nim | ollama)"
        }
        MessageId::CmdQueueDescription => "क़तारबद्ध संदेश देखें या संपादित करें",
        MessageId::CmdRecallDescription => "पिछले cycle संग्रह खोजें (संदेश पाठ पर BM25)",
        MessageId::CmdRelayDescription => "नए थ्रेड के लिए सत्र relay (接力) बनाएँ",
        MessageId::CmdRenameDescription => "वर्तमान सत्र का नाम बदलें",
        MessageId::CmdRestoreDescription => {
            "workspace को पिछले pre/post-turn स्नैपशॉट पर वापस ले जाएँ। बिना तर्क के, हाल के स्नैपशॉट दिखाता है।"
        }
        MessageId::CmdRetryDescription => "अंतिम अनुरोध दोबारा भेजें",
        MessageId::CmdReviewDescription => "फ़ाइल, diff या PR पर संरचित code review चलाएँ",
        MessageId::CmdRlmDescription => "स्थायी RLM context खोलें: /rlm [0-3] <file_or_text>",
        MessageId::CmdSaveDescription => "सत्र फ़ाइल में सहेजें",
        MessageId::CmdForkDescription => "सक्रिय बातचीत को सिबलिंग सत्र में fork करें",
        MessageId::CmdNewDescription => "नया सहेजा गया सत्र प्रारंभ करें",
        MessageId::CmdSessionsDescription => "सत्र इतिहास पिकर खोलें",
        MessageId::CmdSettingsDescription => "स्थायी सेटिंग दिखाएँ",
        MessageId::CmdSkillDescription => {
            "skill सक्रिय करें, या community skill इंस्टॉल/अपडेट/अनइंस्टॉल/trust करें"
        }
        MessageId::CmdSkillsDescription => {
            "स्थानीय skills दिखाएँ (`/skills <prefix>` से फ़िल्टर; --remote क्यूरेटेड रजिस्ट्री देखता है)"
        }
        MessageId::CmdSlopDescription => "SlopLedger देखें या निर्यात करें",
        MessageId::CmdStashDescription => {
            "composer ड्राफ़्ट पार्क करें या पुनर्स्थापित करें (Ctrl+S से push, /stash list/pop)"
        }
        MessageId::CmdStatusDescription => "रनटाइम सत्र स्थिति दिखाएँ",
        MessageId::CmdStatuslineDescription => "कॉन्फ़िगर करें कि footer में कौन सी चीज़ें दिखें",
        MessageId::CmdSubagentsDescription => "sub-agent स्थिति दिखाएँ",
        MessageId::CmdSwarmDescription => {
            "मल्टी-agent fanout टर्न चलाएँ (sequential | mixture | distill | deliberate)"
        }
        MessageId::CmdSystemDescription => "वर्तमान सिस्टम प्रॉम्प्ट दिखाएँ",
        MessageId::CmdTaskDescription => "बैकग्राउंड कार्य प्रबंधित करें",
        MessageId::CmdTokensDescription => "सत्र का token उपयोग दिखाएँ",
        MessageId::CmdTranslateDescription => "वर्तमान सिस्टम भाषा में आउटपुट अनुवाद चालू/बंद करें",
        MessageId::CmdTranslateOff => "आउटपुट अनुवाद बंद (मूल मॉडल आउटपुट दिखाया जा रहा है)",
        MessageId::CmdTranslateOn => "आउटपुट अनुवाद चालू: मॉडल उत्तर आपकी सिस्टम भाषा में दिखाए जाएँगे",
        MessageId::TranslationInProgress => "assistant आउटपुट अनुवाद हो रहा है...",
        MessageId::TranslationComplete => "अनुवाद पूर्ण",
        MessageId::TranslationFailed => "अनुवाद विफल",
        MessageId::CmdTrustDescription => {
            "workspace trust और per-path allowlist प्रबंधित करें (`/trust add <path>`, `/trust list`, `/trust on|off`)"
        }
        MessageId::CmdWorkspaceDescription => "वर्तमान workspace दिखाएँ या बदलें",
        MessageId::CmdUndoDescription => "अंतिम संदेश जोड़ी हटाएँ",
        MessageId::CmdVerboseDescription => "transcript में पूरा live thinking चालू/बंद करें",
        MessageId::CmdCacheAdvice => {
            "तीसरे टर्न के बाद ~70% से अधिक hit/miss अनुपात स्थिर cache prefix दर्शाता है; \n\
             लंबे सत्रों में इससे कम होना prefix churn की जाँच के लायक है (#263)।"
        }
        MessageId::CmdCacheFootnote => {
            "* जब provider ने स्पष्ट रूप से नहीं बताया तो miss का अनुमान input − hit से लगाया गया।\n"
        }
        MessageId::CmdCacheHeader => {
            "Cache telemetry — {total} में से अंतिम {count} टर्न (मॉडल: {model})\n"
        }
        MessageId::CmdCacheNoData => {
            "Cache इतिहास: अभी कोई टर्न दर्ज नहीं।\n\n\
             DeepSeek समर्थित मॉडलों (V4 परिवार) के हर API टर्न पर \
             `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` देता है। \
             एक टर्न चलाएँ और /cache दोबारा आज़माएँ।"
        }
        MessageId::CmdCacheTotals => {
            "Σ in: {sum_in}   Σ hit: {sum_hit}   Σ miss: {sum_miss}   औसत hit अनुपात: {avg}\n"
        }
        MessageId::CmdCostReport => {
            "सत्र लागत:\n\
             ─────────────────────────────\n\
             अनुमानित कुल खर्च: {cost}\n\n\
             लागत के आँकड़े अनुमानित हैं और उपलब्ध होने पर provider usage telemetry का उपयोग करते हैं।\n\n\
             DeepSeek API मूल्य:\n\
             ─────────────────────────────\n\
             इस CLI में मूल्य विवरण कॉन्फ़िगर नहीं है।"
        }
        MessageId::CmdTokensCacheBoth => "{hit} hit / {miss} miss",
        MessageId::CmdTokensCacheHitOnly => "{hit} hit / miss सूचित नहीं",
        MessageId::CmdTokensCacheMissOnly => "hit सूचित नहीं / {miss} miss",
        MessageId::CmdTokensContextUnknownWindow => "~{estimated} / अज्ञात window",
        MessageId::CmdTokensContextWithWindow => "~{used} / {window} ({percent}%)",
        MessageId::FooterAgentSingular => "1 agent",
        MessageId::FooterAgentsPlural => "{count} agents",
        MessageId::FooterPressCtrlCAgain => "बाहर निकलने के लिए Ctrl+C दोबारा दबाएँ",
        MessageId::FooterWorking => "काम कर रहा है",
        MessageId::FooterBalancePrefix => "बैल",
        MessageId::HelpSectionActions => "क्रियाएँ",
        MessageId::HelpSectionClipboard => "क्लिपबोर्ड",
        MessageId::HelpSectionEditing => "इनपुट संपादन",
        MessageId::HelpSectionHelp => "सहायता",
        MessageId::HelpSectionModes => "मोड",
        MessageId::HelpSectionNavigation => "नेविगेशन",
        MessageId::HelpSectionSessions => "सत्र",
        MessageId::CmdTokensNotReported => "सूचित नहीं",
        MessageId::CmdTokensReport => {
            "Token उपयोग:\n\
             ─────────────────────────────\n\
             सक्रिय context:        {active}\n\
             अंतिम API input:        {input} (टर्न telemetry; tool राउंड में दोहराया prefix गिन सकता है)\n\
             अंतिम API output:       {output}\n\
             Cache hit/miss:         {cache} (केवल telemetry/लागत)\n\
             संचित tokens:           {total} (सत्र उपयोग telemetry)\n\
             अनुमानित सत्र लागत:     {cost}\n\
             API संदेश:              {api_messages}\n\
             चैट संदेश:              {chat_messages}\n\
             मॉडल:                   {model}"
        }
        MessageId::KbScrollTranscript => {
            "transcript स्क्रॉल करें, input इतिहास देखें, या composer अटैचमेंट चुनें"
        }
        MessageId::KbNavigateHistory => "input इतिहास देखें",
        MessageId::KbBrowseHistory => "बातचीत इतिहास देखें",
        MessageId::KbScrollTranscriptAlt => "transcript स्क्रॉल करें",
        MessageId::KbScrollPage => "transcript पृष्ठ-दर-पृष्ठ स्क्रॉल करें",
        MessageId::KbJumpTopBottom => "transcript के शीर्ष / तल पर जाएँ",
        MessageId::KbJumpTopBottomEmpty => "शीर्ष / तल पर जाएँ (जब input खाली हो)",
        MessageId::KbJumpToolBlocks => "tool आउटपुट ब्लॉकों के बीच जाएँ",
        MessageId::KbMoveCursor => "composer में cursor घुमाएँ",
        MessageId::KbJumpLineStartEnd => "पंक्ति के आरंभ / अंत पर जाएँ",
        MessageId::KbDeleteChar => "cursor से पहले/बाद का अक्षर मिटाएँ, या चुना गया अटैचमेंट हटाएँ",
        MessageId::KbClearDraft => "वर्तमान ड्राफ़्ट साफ़ करें",
        MessageId::KbStashDraft => "वर्तमान ड्राफ़्ट stash करें (पुनर्स्थापित करने के लिए `/stash pop`)",
        MessageId::KbSearchHistory => "प्रॉम्प्ट इतिहास खोजें और स्थानीय ड्राफ़्ट पुनर्प्राप्त करें",
        MessageId::KbInsertNewline => "composer में नई पंक्ति जोड़ें",
        MessageId::KbSendDraft => "वर्तमान ड्राफ़्ट भेजें",
        MessageId::KbCloseMenu => "मेनू बंद करें, अनुरोध रद्द करें, ड्राफ़्ट छोड़ें, या input साफ़ करें",
        MessageId::KbCancelOrExit => "अनुरोध रद्द करें, या निष्क्रिय होने पर बाहर निकलें",
        MessageId::KbShellControls => "चल रहे foreground कमांड के लिए shell controls खोलें",
        MessageId::KbExitEmpty => "input खाली होने पर बाहर निकलें",
        MessageId::KbCommandPalette => "command palette खोलें",
        MessageId::KbFuzzyFilePicker => "fuzzy file picker खोलें (Enter पर @path जोड़ता है)",
        MessageId::KbCompactInspector => "कॉम्पैक्ट सत्र context इंस्पेक्टर खोलें",
        MessageId::KbLastMessagePager => "अंतिम संदेश के लिए pager खोलें (जब input खाली हो)",
        MessageId::KbSelectedDetails => "चुने गए tool या संदेश का विवरण खोलें (जब input खाली हो)",
        MessageId::KbToolDetailsPager => "tool-details pager खोलें",
        MessageId::KbThinkingPager => "Activity Detail खोलें",
        MessageId::KbLiveTranscript => "live transcript overlay खोलें (sticky-tail ऑटो-स्क्रॉल)",
        MessageId::KbBacktrackMessage => {
            "पिछले user संदेश पर वापस जाएँ (Left/Right से क़दम, Enter से rewind)"
        }
        MessageId::KbCompleteCycleModes => {
            "/command पूरा करें, चल रहे टर्न का follow-up क़तारबद्ध करें, मोड बदलें; Shift+Tab reasoning effort बदलता है"
        }
        MessageId::KbJumpPlanAgentYolo => "सीधे Plan / Agent / YOLO मोड पर जाएँ",
        MessageId::KbAltJumpPlanAgentYolo => "Plan / Agent / YOLO मोड पर जाने का वैकल्पिक तरीक़ा",
        MessageId::KbFocusSidebar => {
            "Work / Tasks / Agents / Context / Auto sidebar पर फ़ोकस करें; Ctrl+Alt+0 इसे छिपाता है"
        }
        MessageId::KbTogglePlanAgent => "Plan और Agent मोड के बीच बदलें",
        MessageId::KbSessionPicker => "session picker खोलें",
        MessageId::KbPasteAttach => "टेक्स्ट पेस्ट करें या क्लिपबोर्ड इमेज जोड़ें",
        MessageId::KbCopySelection => "वर्तमान चयन कॉपी करें (macOS पर Cmd+C)",
        MessageId::KbContextMenu => {
            "पेस्ट, चयन, संदेश विवरण, context और सहायता के लिए context actions खोलें"
        }
        MessageId::KbAttachPath => "स्थानीय टेक्स्ट फ़ाइल या डायरेक्टरी context में जोड़ें",
        MessageId::KbHelpOverlay => "यह help overlay खोलें (जब input खाली हो)",
        MessageId::KbToggleHelp => "help overlay चालू/बंद करें",
        MessageId::KbToggleHelpSlash => "help overlay चालू/बंद करें",
        MessageId::HelpUsageLabel => "उपयोग:",
        MessageId::HelpAliasesLabel => "उपनाम:",
        MessageId::SettingsTitle => "सेटिंग:",
        MessageId::SettingsConfigFile => "कॉन्फ़िग फ़ाइल:",
        MessageId::ClearConversation => "बातचीत साफ़ हो गई",
        MessageId::ClearConversationBusy => {
            "बातचीत साफ़ हो गई (plan स्थिति व्यस्त; आवश्यकता हो तो /clear दोबारा चलाएँ)"
        }
        MessageId::ModelChanged => "मॉडल बदला: {old} \u{2192} {new}",
        MessageId::LinksTitle => "DeepSeek लिंक:",
        MessageId::LinksDashboard => "डैशबोर्ड:",
        MessageId::LinksDocs => "Docs:",
        MessageId::LinksTip => "सुझाव: API keys डैशबोर्ड कंसोल में उपलब्ध हैं।",
        MessageId::SubagentsFetching => "sub-agent स्थिति लाई जा रही है...",
        MessageId::HelpUnknownCommand => "अज्ञात कमांड: {topic}",
        MessageId::HomeDashboardTitle => "codesmith होम डैशबोर्ड",
        MessageId::HomeModel => "मॉडल:",
        MessageId::HomeMode => "मोड:",
        MessageId::HomeWorkspace => "Workspace:",
        MessageId::HomeHistory => "इतिहास:",
        MessageId::HomeTokens => "Tokens:",
        MessageId::HomeQueued => "क़तारबद्ध:",
        MessageId::HomeSubagents => "Sub-agents:",
        MessageId::HomeSkill => "Skill:",
        MessageId::HomeQuickActions => "त्वरित क्रियाएँ",
        MessageId::HomeQuickLinks => "/links      - डैशबोर्ड और API लिंक",
        MessageId::HomeQuickSkills => "/skills      - उपलब्ध skills की सूची",
        MessageId::HomeQuickConfig => "/config      - इंटरैक्टिव कॉन्फ़िगरेशन संपादक खोलें",
        MessageId::HomeQuickSettings => "/settings    - स्थायी सेटिंग दिखाएँ",
        MessageId::HomeQuickModel => "/model       - मॉडल बदलें या देखें",
        MessageId::HomeQuickSubagents => "/subagents   - sub-agent स्थिति दिखाएँ",
        MessageId::HomeQuickTaskList => "/task list   - बैकग्राउंड कार्य क़तार दिखाएँ",
        MessageId::HomeQuickHelp => "/help        - सहायता दिखाएँ",
        MessageId::HomeModeTips => "मोड सुझाव",
        MessageId::HomeAgentModeTip => "Agent मोड - स्वायत्त कार्यों के लिए tools उपयोग करें",
        MessageId::HomeAgentModeReviewTip => {
            "  निष्पादन से पहले Plan मोड में समीक्षा के लिए Ctrl+X उपयोग करें"
        }
        MessageId::HomeAgentModeYoloTip => "  पूर्ण tool एक्सेस चालू करने के लिए /mode yolo टाइप करें",
        MessageId::HomeYoloModeTip => "YOLO मोड - पूर्ण tool एक्सेस, कोई स्वीकृति नहीं",
        MessageId::HomeYoloModeCaution => "  विनाशकारी क्रियाओं से सावधान रहें!",
        MessageId::HomePlanModeTip => "Plan मोड - लागू करने से पहले डिज़ाइन करें",
        MessageId::HomePlanModeChecklistTip => "  संरचित checklist बनाने के लिए /mode plan उपयोग करें",
        MessageId::HomeGoalModeTip => "लक्ष्य ट्रैकिंग - लक्ष्य सेट करने के लिए /goal <objective>",
        // Onboarding — language picker.
        MessageId::OnboardLanguageTitle => "अपनी भाषा चुनें",
        MessageId::OnboardLanguageBlurb => {
            "UI भाषा चुनें। आप कभी भी `/settings set locale <tag>` से बदल सकते हैं।"
        }
        MessageId::OnboardLanguageFooter => "चुनने के लिए 1-6 दबाएँ, या वर्तमान सेटिंग रखने के लिए Enter",
        // Onboarding — API key entry.
        MessageId::OnboardApiKeyTitle => "अपनी DeepSeek API key जोड़ें",
        MessageId::OnboardApiKeyStep1 => {
            "चरण 1.  https://platform.deepseek.com/api_keys खोलें और key बनाएँ।"
        }
        MessageId::OnboardApiKeyStep2 => "चरण 2.  इसे नीचे पेस्ट करें और Enter दबाएँ।",
        MessageId::OnboardApiKeySavedHint => {
            "~/.codesmith/config.toml में सहेजा गया ताकि किसी भी फ़ोल्डर से काम करे।"
        }
        MessageId::OnboardApiKeyFormatHint => {
            "पूरी key बिल्कुल वैसी ही पेस्ट करें जैसी जारी हुई थी (बिना रिक्त स्थान या नई पंक्ति)।"
        }
        MessageId::OnboardApiKeyPlaceholder => "(key यहाँ पेस्ट करें)",
        MessageId::OnboardApiKeyLabel => "Key: ",
        MessageId::OnboardApiKeyFooter => "सहेजने के लिए Enter, वापस जाने के लिए Esc दबाएँ।",
        // Onboarding — workspace trust.
        MessageId::OnboardTrustTitle => "Workspace पर भरोसा",
        MessageId::OnboardTrustQuestion => "क्या आप इस डायरेक्टरी की सामग्री पर भरोसा करते हैं?",
        MessageId::OnboardTrustLocationPrefix => "आप यहाँ हैं ",
        MessageId::OnboardTrustRiskHint => {
            "अविश्वसनीय सामग्री के साथ काम करने पर prompt injection का जोखिम अधिक होता है।"
        }
        MessageId::OnboardTrustEffectHint => {
            "इस डायरेक्टरी पर भरोसा करने से यह global config में दर्ज हो जाता है और trusted workspace मोड चालू होता है।"
        }
        MessageId::OnboardTrustFooterPrefix => "दबाएँ ",
        MessageId::OnboardTrustFooterMiddle => " भरोसा कर आगे बढ़ने के लिए, ",
        MessageId::OnboardTrustFooterSuffix => " बाहर निकलने के लिए",
        // Onboarding — final tips.
        MessageId::OnboardTipsTitle => "सरल शुरुआत करें",
        MessageId::OnboardTipsLine1 => {
            "काम सादी भाषा में लिखें। कमांड चाहिए तो /help या Ctrl+K उपयोग करें।"
        }
        MessageId::OnboardTipsLine2 => {
            "नीचे का composer multi-line है: Enter भेजता है, Alt+Enter या Ctrl+J नई पंक्ति जोड़ता है।"
        }
        MessageId::OnboardTipsLine3 => {
            "काम बदलने पर ही मोड बदलें: समीक्षा-पहले काम के लिए Plan, निष्पादन के लिए Agent, और auto-approval चाहिए तो YOLO।"
        }
        MessageId::OnboardTipsLine4 => {
            "Ctrl+R पिछले सत्र फिर खोलता है, और Esc मौजूदा ड्राफ़्ट या overlay से बाहर निकलता है।"
        }
        MessageId::OnboardTipsFooterEnter => "Enter दबाएँ",
        MessageId::OnboardTipsFooterAction => " workspace खोलने के लिए",
        // Context menu.
        MessageId::CtxMenuTitle => " राइट क्लिक ",
        MessageId::CtxMenuCopySelection => "चयन कॉपी करें",
        MessageId::CtxMenuCopySelectionDesc => "चुना transcript टेक्स्ट लिखें",
        MessageId::CtxMenuOpenSelection => "चयन खोलें",
        MessageId::CtxMenuOpenSelectionDesc => "चुने टेक्स्ट को pager में दिखाएँ",
        MessageId::CtxMenuClearSelection => "चयन साफ़ करें",
        MessageId::CtxMenuOpenDetails => "विवरण खोलें",
        MessageId::CtxMenuCopyMessage => "संदेश कॉपी करें",
        MessageId::CtxMenuCopyMessageDesc => "क्लिक किया transcript cell लिखें",
        MessageId::CtxMenuOpenInEditor => "संपादक में खोलें",
        MessageId::CtxMenuOpenInEditorDesc => "$EDITOR में file:line खोलें",
        MessageId::CtxMenuShowCell => "cell दिखाएँ",
        MessageId::CtxMenuShowCellDesc => "इस transcript cell को पुनः दिखाएँ",
        MessageId::CtxMenuHideCell => "cell छिपाएँ",
        MessageId::CtxMenuHideCellDesc => "इस transcript cell को सिकोड़ें",
        MessageId::CtxMenuShowHidden => "छिपे हुए दिखाएँ",
        MessageId::CtxMenuShowHiddenDesc => "सभी सिकुड़े cells पुनः दिखाएँ",
        MessageId::CtxMenuPaste => "पेस्ट",
        MessageId::CtxMenuPasteDesc => "क्लिपबोर्ड composer में जोड़ें",
        MessageId::CtxMenuCmdPalette => "Command palette",
        MessageId::CtxMenuCmdPaletteDesc => "कमांड, skills और tools",
        MessageId::CtxMenuContextInspector => "Context inspector",
        MessageId::CtxMenuContextInspectorDesc => "सक्रिय context और cache संकेत",
        MessageId::CtxMenuHelp => "सहायता",
        MessageId::CtxMenuHelpDesc => "कीबाइंडिंग और कमांड",
    })
}

fn spanish_latin_america(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::ComposerPlaceholder => "Escribe una tarea o usa /.",
        MessageId::HistorySearchPlaceholder => "Buscar en el historial de prompts...",
        MessageId::HistorySearchTitle => "Búsqueda en el historial",
        MessageId::HistoryHintMove => "Arriba/Abajo mover",
        MessageId::HistoryHintAccept => "Enter aceptar",
        MessageId::HistoryHintRestore => "Esc restaurar",
        MessageId::HistoryNoMatches => "  Sin resultados",
        MessageId::ConfigTitle => "Configuración de la sesión",
        MessageId::ConfigModalTitle => " Config ",
        MessageId::ConfigSearchPlaceholder => "escribe para filtrar",
        MessageId::ConfigNoSettings => "  No hay configuraciones disponibles.",
        MessageId::ConfigNoMatchesPrefix => "  Ninguna configuración coincide con ",
        MessageId::ConfigFilteredSettings => "  Configuraciones filtradas",
        MessageId::ConfigShowing => "  Mostrando",
        MessageId::ConfigFooterDefault => {
            " escribir=filtrar, Arriba/Abajo=seleccionar, Enter/e=editar, Esc/q=cerrar "
        }
        MessageId::ConfigFooterScrollable => {
            " escribir=filtrar, Arriba/Abajo=seleccionar, Enter/e=editar, PgUp/PgDn=desplazar, Esc/q=cerrar "
        }
        MessageId::ConfigFooterFiltered => {
            " escribir=filtrar, Backspace=borrar, Ctrl+U/Esc=limpiar, Enter=editar "
        }
        MessageId::HelpTitle => "Ayuda",
        MessageId::HelpFilterPlaceholder => "Escribe para filtrar",
        MessageId::HelpFilterPrefix => "Filtro: ",
        MessageId::HelpNoMatches => "  Sin resultados.",
        MessageId::HelpSlashCommands => "Comandos con barra",
        MessageId::HelpKeybindings => "Atajos de teclado",
        MessageId::HelpFooterTypeFilter => " escribir para filtrar ",
        MessageId::HelpFooterMove => "  Arriba/Abajo mover ",
        MessageId::HelpFooterJump => " PgUp/PgDn saltar ",
        MessageId::HelpFooterClose => " Esc cerrar ",
        MessageId::CmdAnchorDescription => {
            "Fijar un dato que sobrevive a la compactación (inyectado automáticamente en el contexto)"
        }
        MessageId::CmdAttachDescription => {
            "Adjuntar imagen o video; usa @ruta para archivos de texto o directorios"
        }
        MessageId::CmdCacheDescription => {
            "Mostrar estadísticas de hit/miss del caché de prefijo DeepSeek en las últimas N rondas"
        }
        MessageId::CmdChangeDescription => "Mostrar la entrada más reciente del changelog",
        MessageId::CmdChangeHeader => "Changelog más reciente",
        MessageId::CmdChangeTranslationQueued => {
            "Las notas de la versión en inglés se muestran abajo. Se solicitará una versión traducida a continuación; si el proveedor no está disponible, este texto en inglés será el fallback."
        }
        MessageId::CmdChangeTranslationUnavailable => {
            "Las notas de la versión en inglés se muestran abajo. La traducción no está disponible porque la sesión actual no tiene clave de API o está offline."
        }
        MessageId::CmdChangePreviousVersion => {
            "Versión anterior: {version} — ejecuta `/change {version}` para verla"
        }
        MessageId::CmdBalanceDescription => "Consultar el saldo de la cuenta del proveedor activo",
        MessageId::CmdClearDescription => "Limpiar el historial de la conversación",
        MessageId::CmdCompactDescription => {
            "Compactar el contexto para liberar espacio (heredado; v0.6.6 prefiere reinicio de ciclo)"
        }
        MessageId::CmdPurgeDescription => {
            "Permite al agente eliminar quirúrgicamente historial innecesario para liberar espacio de contexto"
        }
        MessageId::CmdConfigDescription => "Abrir el editor interactivo de configuración",
        MessageId::CmdContextDescription => "Abrir el inspector compacto de contexto de la sesión",
        MessageId::CmdCostDescription => "Mostrar el desglose de costo de la sesión",
        MessageId::CmdCycleDescription => {
            "Mostrar el resumen de continuidad de un ciclo específico"
        }
        MessageId::CmdCyclesDescription => {
            "Listar las transferencias de checkpoint-restart de esta sesión"
        }
        MessageId::CmdDiffDescription => "Mostrar cambios en archivos desde el inicio de la sesión",
        MessageId::CmdEditDescription => "Revisar y reenviar el último mensaje",
        MessageId::CmdExitDescription => "Salir de la aplicación",
        MessageId::CmdExportDescription => "Exportar la conversación a markdown",
        MessageId::CmdFeedbackDescription => "Generar una URL de feedback en GitHub",
        MessageId::CmdHelpDescription => "Mostrar información de ayuda",
        MessageId::CmdHomeDescription => {
            "Mostrar el panel inicial con estadísticas y acciones rápidas"
        }
        MessageId::CmdHooksDescription => {
            "Listar hooks de ciclo de vida configurados (solo lectura)"
        }
        MessageId::CmdAgentDescription => {
            "Abrir una sesión persistente de sub-agente: /agent [0-3] <tarea>"
        }
        MessageId::CmdGoalDescription => {
            "Definir una meta de sesión con presupuesto de tokens opcional"
        }
        MessageId::CmdInitDescription => "Generar AGENTS.md para el proyecto",
        MessageId::CmdLspDescription => "Alternar diagnóstico LSP encendido o apagado",
        MessageId::CmdShareDescription => "Exportar la sesión actual como una URL web compartible",
        MessageId::CmdJobsDescription => {
            "Inspeccionar y controlar trabajos de shell en segundo plano"
        }
        MessageId::CmdLinksDescription => "Mostrar enlaces del panel y documentación de DeepSeek",
        MessageId::CmdLoadDescription => "Cargar la sesión desde un archivo",
        MessageId::CmdLogoutDescription => "Limpiar la clave de API y volver a la configuración",
        MessageId::CmdMcpDescription => "Abrir o gestionar servidores MCP",
        MessageId::CmdMemoryDescription => {
            "Inspeccionar o gestionar el archivo persistente de memoria del usuario"
        }
        MessageId::CmdModeDescription => {
            "Alternar modo o abrir selector: /mode [agent|plan|yolo|1|2|3]"
        }
        MessageId::CmdModelDescription => "Cambiar o mostrar el modelo actual",
        MessageId::CmdModelsDescription => "Listar los modelos disponibles por la API",
        MessageId::CmdNetworkDescription => "Gestionar reglas de red permitidas y bloqueadas",
        MessageId::CmdNoteDescription => {
            "Agregar nota al archivo persistente (.codesmith/notes.md)"
        }
        MessageId::CmdThemeDescription => "Alternar entre tema claro y oscuro",
        MessageId::CmdProviderDescription => {
            "Cambiar o mostrar el backend LLM activo (deepseek | nvidia-nim | ollama)"
        }
        MessageId::CmdQueueDescription => "Ver o editar mensajes en cola",
        MessageId::CmdRecallDescription => {
            "Buscar archivos de ciclos anteriores (BM25 sobre el texto de los mensajes)"
        }
        MessageId::CmdRelayDescription => "Crear un relay de sesión (接力) para un hilo nuevo",
        MessageId::CmdRenameDescription => "Renombrar la sesión actual",
        MessageId::CmdRestoreDescription => {
            "Revertir el workspace a un snapshot pre/post-turno anterior. Sin argumento, lista los snapshots recientes."
        }
        MessageId::CmdRetryDescription => "Repetir la última solicitud",
        MessageId::CmdReviewDescription => {
            "Ejecutar una revisión de código estructurada en un archivo, diff o PR"
        }
        MessageId::CmdRlmDescription => {
            "Turno del Recursive Language Model (RLM) — guarda el prompt en un REPL Python y deja que el modelo escriba el código que lo procesa; usa `llm_query()` / `sub_rlm()` para llamadas a sub-LLMs."
        }
        MessageId::CmdSaveDescription => "Guardar la sesión en archivo",
        MessageId::CmdForkDescription => "Bifurcar la conversación activa a una sesión hermana",
        MessageId::CmdNewDescription => "Iniciar una nueva sesión guardada",
        MessageId::CmdSessionsDescription => "Abrir el selector de sesiones",
        MessageId::CmdSettingsDescription => "Mostrar las configuraciones persistidas",
        MessageId::CmdSkillDescription => {
            "Activar una skill, o instalar/actualizar/desinstalar/confiar en una skill de la comunidad"
        }
        MessageId::CmdSkillsDescription => {
            "Listar skills locales (filtra con `/skills <prefijo>`; --remote navega el registro curado)"
        }
        MessageId::CmdSlopDescription => "Inspect or export the SlopLedger",
        MessageId::CmdStashDescription => {
            "Estacionar o restaurar borrador del compositor (Ctrl+S estaciona, /stash list|pop)"
        }
        MessageId::CmdStatusDescription => "Mostrar el estado de la sesión en ejecución",
        MessageId::CmdStatuslineDescription => {
            "Configurar qué elementos aparecen en el pie de página"
        }
        MessageId::CmdSubagentsDescription => "Listar el estado de los sub-agentes",
        MessageId::CmdSwarmDescription => {
            "Ejecutar turno fanout multi-agente (sequential | mixture | distill | deliberate)"
        }
        MessageId::CmdSystemDescription => "Mostrar el prompt de sistema actual",
        MessageId::CmdTaskDescription => "Gestionar tareas en segundo plano",
        MessageId::CmdTokensDescription => "Mostrar el uso de tokens de la sesión",
        MessageId::CmdTranslateDescription => {
            "Activar o desactivar la traducción de salida al idioma actual del sistema"
        }
        MessageId::CmdTranslateOff => {
            "Traducción de salida desactivada (se muestra la salida original del modelo)"
        }
        MessageId::CmdTranslateOn => {
            "Traducción de salida activada: las respuestas del modelo se mostrarán en el idioma del sistema"
        }
        MessageId::TranslationInProgress => "Traduciendo la salida del asistente...",
        MessageId::TranslationComplete => "Traducción completada",
        MessageId::TranslationFailed => "Traducción fallida",
        MessageId::CmdTrustDescription => {
            "Gestionar la confianza del workspace y la lista de paths permitidos (`/trust add <ruta>`, `/trust list`, `/trust on|off`)"
        }
        MessageId::CmdWorkspaceDescription => "Mostrar o cambiar el workspace actual",
        MessageId::CmdUndoDescription => "Eliminar el último par de mensajes",
        MessageId::CmdVerboseDescription => {
            "Alternar pensamiento en vivo completo en la transcripción"
        }
        MessageId::CmdCacheAdvice => {
            "Tasas de hit/miss arriba del ~70% a partir del tercer turno indican un prefijo de caché estable;\n\
             valores menores en sesiones largas sugieren inestabilidad en el prefijo, vale investigar (#263)."
        }
        MessageId::CmdCacheFootnote => {
            "* miss inferido a partir de entrada − hit cuando el proveedor no lo reporta por separado.\n"
        }
        MessageId::CmdCacheHeader => {
            "Telemetría del caché — últimos {count} de {total} turno(s) (modelo: {model})\n"
        }
        MessageId::CmdCacheNoData => {
            "Historial del caché: ningún turno registrado todavía.\n\n\
             DeepSeek expone `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` en cada turno \
             de la API donde el modelo lo soporta (familia V4). Ejecuta un turno y prueba /cache de nuevo."
        }
        MessageId::CmdCacheTotals => {
            "Σ entrada: {sum_in}   Σ hit: {sum_hit}   Σ miss: {sum_miss}   tasa promedio de hit: {avg}\n"
        }
        MessageId::CmdCostReport => {
            "Costo de la sesión:\n\
             ─────────────────────────────\n\
             Total aproximado: {cost}\n\n\
             Las estimaciones de costo son aproximadas y usan la telemetría de uso del proveedor cuando está disponible.\n\n\
             Precios de la API DeepSeek:\n\
             ─────────────────────────────\n\
             Los detalles de precio no están configurados en esta CLI."
        }
        MessageId::CmdTokensCacheBoth => "{hit} hit / {miss} miss",
        MessageId::CmdTokensCacheHitOnly => "{hit} hit / miss no reportado",
        MessageId::CmdTokensCacheMissOnly => "hit no reportado / {miss} miss",
        MessageId::CmdTokensContextUnknownWindow => "~{estimated} / ventana desconocida",
        MessageId::CmdTokensContextWithWindow => "~{used} / {window} ({percent}%)",
        MessageId::FooterAgentSingular => "1 sub-agente",
        MessageId::FooterAgentsPlural => "{count} sub-agentes",
        MessageId::FooterPressCtrlCAgain => "Presiona Ctrl+C de nuevo para salir",
        MessageId::FooterWorking => "trabajando",
        MessageId::FooterBalancePrefix => "saldo",
        MessageId::HelpSectionActions => "Acciones",
        MessageId::HelpSectionClipboard => "Portapapeles",
        MessageId::HelpSectionEditing => "Edición de entrada",
        MessageId::HelpSectionHelp => "Ayuda",
        MessageId::HelpSectionModes => "Modos",
        MessageId::HelpSectionNavigation => "Navegación",
        MessageId::HelpSectionSessions => "Sesiones",
        MessageId::CmdTokensNotReported => "no reportado",
        MessageId::CmdTokensReport => {
            "Uso de tokens:\n\
             ─────────────────────────────\n\
             Contexto activo:           {active}\n\
             Última entrada de API:     {input} (telemetría por turno; puede contar el mismo prefijo varias veces en rondas con herramientas)\n\
             Última salida de API:      {output}\n\
             Hit/miss del caché:        {cache} (solo para telemetría/costo)\n\
             Tokens acumulados:         {total} (telemetría de uso de la sesión)\n\
             Costo aproximado:          {cost}\n\
             Mensajes de API:           {api_messages}\n\
             Mensajes del chat:         {chat_messages}\n\
             Modelo:                    {model}"
        }
        MessageId::KbScrollTranscript => {
            "Desplazar transcripción, navegar historial de entrada o seleccionar adjuntos del compositor"
        }
        MessageId::KbNavigateHistory => "Navegar historial de entrada",
        MessageId::KbBrowseHistory => "Explorar historial de conversación",
        MessageId::KbScrollTranscriptAlt => "Desplazar transcripción",
        MessageId::KbScrollPage => "Desplazar transcripción por página",
        MessageId::KbJumpTopBottom => "Saltar al inicio / fin de la transcripción",
        MessageId::KbJumpTopBottomEmpty => "Saltar al inicio / fin (cuando la entrada está vacía)",
        MessageId::KbJumpToolBlocks => "Saltar entre bloques de salida de herramientas",
        MessageId::KbMoveCursor => "Mover cursor en el compositor",
        MessageId::KbJumpLineStartEnd => "Saltar al inicio / fin de la línea",
        MessageId::KbDeleteChar => {
            "Eliminar carácter antes / después del cursor, o quitar adjunto seleccionado"
        }
        MessageId::KbClearDraft => "Limpiar borrador actual",
        MessageId::KbStashDraft => "Estacionar borrador actual (`/stash pop` restaura)",
        MessageId::KbSearchHistory => "Buscar historial de prompts y recuperar borradores locales",
        MessageId::KbInsertNewline => "Insertar nueva línea en el compositor",
        MessageId::KbSendDraft => "Enviar borrador actual",
        MessageId::KbCloseMenu => {
            "Cerrar menú, cancelar solicitud, descartar borrador o limpiar entrada"
        }
        MessageId::KbCancelOrExit => "Cancelar solicitud o salir cuando está inactivo",
        MessageId::KbShellControls => "Abrir controles de shell para comando en primer plano",
        MessageId::KbExitEmpty => "Salir cuando la entrada está vacía",
        MessageId::KbCommandPalette => "Abrir paleta de comandos",
        MessageId::KbFuzzyFilePicker => {
            "Abrir selector de archivo fuzzy (inserta @ruta al presionar Enter)"
        }
        MessageId::KbCompactInspector => "Abrir inspector compacto de contexto de la sesión",
        MessageId::KbLastMessagePager => {
            "Abrir paginador para el último mensaje (cuando la entrada está vacía)"
        }
        MessageId::KbSelectedDetails => {
            "Abrir detalles de la herramienta o mensaje seleccionado (cuando la entrada está vacía)"
        }
        MessageId::KbToolDetailsPager => "Abrir paginador de detalles de la herramienta",
        MessageId::KbThinkingPager => "Abrir paginador de razonamiento",
        MessageId::KbLiveTranscript => "Abrir superposición de transcripción en vivo (auto-scroll)",
        MessageId::KbBacktrackMessage => {
            "Retroceder al mensaje anterior del usuario (izquierda/derecha, Enter para rebobinar)"
        }
        MessageId::KbCompleteCycleModes => {
            "Completar /command, encolar follow-up, ciclar modos; Shift+Tab cicla esfuerzo de razonamiento"
        }
        MessageId::KbJumpPlanAgentYolo => "Saltar directo a modo Plan / Agent / YOLO",
        MessageId::KbAltJumpPlanAgentYolo => "Salto alternativo a modo Plan / Agent / YOLO",
        MessageId::KbFocusSidebar => {
            "Enfocar barra lateral Work / Tasks / Agents / Context / Auto / Ocultar"
        }
        MessageId::KbTogglePlanAgent => "Alternar entre modos Plan y Agent",
        MessageId::KbSessionPicker => "Abrir selector de sesiones",
        MessageId::KbPasteAttach => "Pegar texto o adjuntar imagen del portapapeles",
        MessageId::KbCopySelection => "Copiar selección actual (Cmd+C en macOS)",
        MessageId::KbContextMenu => {
            "Abrir acciones de contexto para pegar, selección, detalles, contexto y ayuda"
        }
        MessageId::KbAttachPath => "Agregar archivo o directorio local al contexto",
        MessageId::KbHelpOverlay => {
            "Abrir esta superposición de ayuda (cuando la entrada está vacía)"
        }
        MessageId::KbToggleHelp => "Alternar superposición de ayuda",
        MessageId::KbToggleHelpSlash => "Alternar superposición de ayuda",
        MessageId::HelpUsageLabel => "Uso:",
        MessageId::HelpAliasesLabel => "Alias:",
        MessageId::SettingsTitle => "Configuraciones:",
        MessageId::SettingsConfigFile => "Archivo de configuración:",
        MessageId::ClearConversation => "Conversación limpia",
        MessageId::ClearConversationBusy => {
            "Conversación limpia (estado del plan ocupado; ejecuta /clear de nuevo si es necesario)"
        }
        MessageId::ModelChanged => "Modelo cambiado: {old} \u{2192} {new}",
        MessageId::LinksTitle => "Enlaces de DeepSeek:",
        MessageId::LinksDashboard => "Panel:",
        MessageId::LinksDocs => "Documentación:",
        MessageId::LinksTip => "Tip: las claves de API están disponibles en la consola del panel.",
        MessageId::SubagentsFetching => "Obteniendo estado de los sub-agentes...",
        MessageId::HelpUnknownCommand => "Comando desconocido: {topic}",
        MessageId::HomeDashboardTitle => "Panel Inicial de codesmith",
        MessageId::HomeModel => "Modelo:",
        MessageId::HomeMode => "Modo:",
        MessageId::HomeWorkspace => "Workspace:",
        MessageId::HomeHistory => "Historial:",
        MessageId::HomeTokens => "Tokens:",
        MessageId::HomeQueued => "En cola:",
        MessageId::HomeSubagents => "Sub-agentes:",
        MessageId::HomeSkill => "Skill:",
        MessageId::HomeQuickActions => "Acciones Rápidas",
        MessageId::HomeQuickLinks => "/links      - Enlaces del panel y API",
        MessageId::HomeQuickSkills => "/skills      - Listar skills disponibles",
        MessageId::HomeQuickConfig => "/config      - Abrir editor interactivo de configuración",
        MessageId::HomeQuickSettings => "/settings    - Mostrar configuraciones persistentes",
        MessageId::HomeQuickModel => "/model       - Alternar o visualizar modelo",
        MessageId::HomeQuickSubagents => "/subagents   - Listar estado de los sub-agentes",
        MessageId::HomeQuickTaskList => "/task list   - Mostrar fila de tareas en segundo plano",
        MessageId::HomeQuickHelp => "/help        - Mostrar ayuda",
        MessageId::HomeModeTips => "Tips de Modo",
        MessageId::HomeAgentModeTip => "Modo Agent - Usar herramientas para tareas autónomas",
        MessageId::HomeAgentModeReviewTip => {
            "  Usa Ctrl+X para revisar en modo Plan antes de ejecutar"
        }
        MessageId::HomeAgentModeYoloTip => {
            "  Escribe /mode yolo para habilitar acceso total a las herramientas"
        }
        MessageId::HomeYoloModeTip => "Modo YOLO - Acceso total a herramientas, sin aprobaciones",
        MessageId::HomeYoloModeCaution => "  ¡Ten cuidado con operaciones destructivas!",
        MessageId::HomePlanModeTip => "Modo Plan - Planear antes de implementar",
        MessageId::HomePlanModeChecklistTip => {
            "  Usa /mode plan para crear checklists estructurados"
        }
        MessageId::HomeGoalModeTip => {
            "Seguimiento de Goal - Usa /goal <objetivo> para seguir un objetivo persistente"
        }
        MessageId::OnboardLanguageTitle => "Elige el idioma",
        MessageId::OnboardLanguageBlurb => {
            "Elige el idioma de la interfaz. Puedes cambiarlo en cualquier momento con `/settings set locale <etiqueta>`."
        }
        MessageId::OnboardLanguageFooter => {
            "Presiona 1-6 para elegir, o Enter para mantener la configuración actual"
        }
        MessageId::OnboardApiKeyTitle => "Conecta tu clave de API DeepSeek",
        MessageId::OnboardApiKeyStep1 => {
            "Paso 1.  Abre https://platform.deepseek.com/api_keys y crea una clave."
        }
        MessageId::OnboardApiKeyStep2 => "Paso 2.  Pégala abajo y presiona Enter.",
        MessageId::OnboardApiKeySavedHint => {
            "Guardada en ~/.codesmith/config.toml para funcionar en cualquier carpeta."
        }
        MessageId::OnboardApiKeyFormatHint => {
            "Pega la clave completa tal como fue emitida (sin espacios ni saltos de línea)."
        }
        MessageId::OnboardApiKeyPlaceholder => "(pega la clave acá)",
        MessageId::OnboardApiKeyLabel => "Clave: ",
        MessageId::OnboardApiKeyFooter => "Enter para guardar, Esc para volver.",
        MessageId::OnboardTrustTitle => "Confiar en el directorio",
        MessageId::OnboardTrustQuestion => "¿Confías en el contenido de este directorio?",
        MessageId::OnboardTrustLocationPrefix => "Estás en ",
        MessageId::OnboardTrustRiskHint => {
            "Trabajar con contenido no confiable aumenta el riesgo de inyección de prompt."
        }
        MessageId::OnboardTrustEffectHint => {
            "Confiar en este directorio lo registra en la configuración global y habilita el modo workspace confiable."
        }
        MessageId::OnboardTrustFooterPrefix => "Presiona ",
        MessageId::OnboardTrustFooterMiddle => " para confiar y continuar, ",
        MessageId::OnboardTrustFooterSuffix => " para salir",
        MessageId::OnboardTipsTitle => "Empieza simple",
        MessageId::OnboardTipsLine1 => {
            "Escribe la tarea en lenguaje natural. Usa /help o Ctrl+K para comandos."
        }
        MessageId::OnboardTipsLine2 => {
            "El composer inferior es multilínea: Enter envía, Alt+Enter o Ctrl+J agrega una nueva línea."
        }
        MessageId::OnboardTipsLine3 => {
            "Cambia de modo solo cuando el trabajo cambie: Plan para revisar antes, Agent para ejecución, YOLO para auto-aprobación."
        }
        MessageId::OnboardTipsLine4 => {
            "Ctrl+R retoma sesiones anteriores, y Esc cancela el borrador o superposición actual."
        }
        MessageId::OnboardTipsFooterEnter => "Presiona Enter",
        MessageId::OnboardTipsFooterAction => " para abrir el workspace",
        // Context menu.
        MessageId::CtxMenuTitle => " Clic derecho ",
        MessageId::CtxMenuCopySelection => "Copiar selección",
        MessageId::CtxMenuCopySelectionDesc => "copiar texto seleccionado de la transcripción",
        MessageId::CtxMenuOpenSelection => "Abrir selección",
        MessageId::CtxMenuOpenSelectionDesc => "mostrar texto seleccionado en el visor",
        MessageId::CtxMenuClearSelection => "Limpiar selección",
        MessageId::CtxMenuOpenDetails => "Abrir detalles",
        MessageId::CtxMenuCopyMessage => "Copiar mensaje",
        MessageId::CtxMenuCopyMessageDesc => "copiar celda de transcripción seleccionada",
        MessageId::CtxMenuOpenInEditor => "Abrir en editor",
        MessageId::CtxMenuOpenInEditorDesc => "abrir file:line en $EDITOR",
        MessageId::CtxMenuShowCell => "Mostrar celda",
        MessageId::CtxMenuShowCellDesc => "volver a mostrar esta celda de transcripción",
        MessageId::CtxMenuHideCell => "Ocultar celda",
        MessageId::CtxMenuHideCellDesc => "colapsar esta celda de transcripción",
        MessageId::CtxMenuShowHidden => "Mostrar ocultas",
        MessageId::CtxMenuShowHiddenDesc => "volver a mostrar todas las celdas colapsadas",
        MessageId::CtxMenuPaste => "Pegar",
        MessageId::CtxMenuPasteDesc => "insertar portapapeles en el compositor",
        MessageId::CtxMenuCmdPalette => "Paleta de comandos",
        MessageId::CtxMenuCmdPaletteDesc => "comandos, habilidades y herramientas",
        MessageId::CtxMenuContextInspector => "Inspector de contexto",
        MessageId::CtxMenuContextInspectorDesc => "contexto activo y sugerencias de caché",
        MessageId::CtxMenuHelp => "Ayuda",
        MessageId::CtxMenuHelpDesc => "atajos de teclado y comandos",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        widgets::{Paragraph, Widget, Wrap},
    };

    #[test]
    fn locale_setting_normalizes_supported_tags() {
        assert_eq!(normalize_configured_locale("auto"), Some("auto"));
        assert_eq!(normalize_configured_locale("en_US.UTF-8"), Some("en"));
        assert_eq!(normalize_configured_locale("zh-CN"), Some("zh-Hans"));
        assert_eq!(normalize_configured_locale("zh-TW"), Some("zh-Hant"));
        assert_eq!(normalize_configured_locale("zh_HK.UTF-8"), Some("zh-Hant"));
        assert_eq!(normalize_configured_locale("hi_IN.UTF-8"), Some("hi"));
        assert_eq!(normalize_configured_locale("es"), Some("es-419"));
        assert_eq!(normalize_configured_locale("es-MX"), Some("es-419"));
    }

    #[test]
    fn locale_resolution_uses_config_then_environment_then_english() {
        assert_eq!(
            resolve_locale_with_env("hi", |_| Some("zh_CN.UTF-8".to_string())),
            Locale::Hi
        );
        assert_eq!(
            resolve_locale_with_env("auto", |key| {
                (key == "LANG").then(|| "zh_CN.UTF-8".to_string())
            }),
            Locale::ZhHans
        );
        assert_eq!(
            resolve_locale_with_env("auto", |key| {
                (key == "LANG").then(|| "zh_TW.UTF-8".to_string())
            }),
            Locale::ZhHant
        );
        assert_eq!(
            resolve_locale_with_env("auto", |key| {
                (key == "LANG").then(|| "hi_IN.UTF-8".to_string())
            }),
            Locale::Hi
        );
        assert_eq!(resolve_locale_with_env("auto", |_| None), Locale::En);
    }

    #[test]
    fn shipped_first_pack_has_no_missing_core_messages() {
        for locale in Locale::shipped() {
            assert!(
                missing_message_ids(*locale).is_empty(),
                "{} is missing messages",
                locale.tag()
            );
        }
    }

    #[test]
    fn unsupported_locale_falls_back_to_english() {
        assert_eq!(
            resolve_locale_with_env("ar", |_| None),
            Locale::En,
            "Arabic is not a shipped locale"
        );
        assert_eq!(
            resolve_locale_with_env("ja", |_| None),
            Locale::En,
            "Japanese support was dropped and must fall back to English"
        );
        assert_eq!(
            resolve_locale_with_env("vi", |_| None),
            Locale::En,
            "Vietnamese support was dropped and must fall back to English"
        );
        assert_eq!(
            resolve_locale_with_env("pt-BR", |_| None),
            Locale::En,
            "Brazilian Portuguese support was dropped and must fall back to English"
        );
    }

    #[test]
    fn missing_translation_falls_back_to_english() {
        assert_eq!(
            fallback_translation(None, MessageId::ComposerPlaceholder),
            english(MessageId::ComposerPlaceholder)
        );
    }

    #[test]
    fn provider_description_names_deepseek_backend() {
        for locale in Locale::shipped() {
            let description = tr(*locale, MessageId::CmdProviderDescription);
            assert!(
                description.contains("deepseek"),
                "{} provider description should mention deepseek: {description}",
                locale.tag()
            );
            assert!(
                !description.contains("codesmith |"),
                "{} provider description should not name codesmith as a backend: {description}",
                locale.tag()
            );
        }
    }

    #[test]
    fn width_truncation_handles_cjk_indic_and_latin_samples() {
        let samples = [
            ("zh-Hans", "输入以筛选配置"),
            ("hi", "सेटिंग खोजें"),
            ("es-419", "configuraciones filtradas"),
        ];

        for (tag, sample) in samples {
            let truncated = truncate_to_width(sample, 12);
            assert!(
                truncated.width() <= 12,
                "{tag} sample overflowed: {truncated:?}"
            );
        }
    }

    #[test]
    fn shipped_script_samples_render_in_narrow_terminal_buffer() {
        let samples = [
            ("CJK", "输入以筛选配置"),
            ("Indic", "सेटिंग खोजें"),
            ("Latin", "configuraciones filtradas"),
        ];

        for (label, sample) in samples {
            let area = Rect::new(0, 0, 18, 4);
            let mut buf = Buffer::empty(area);
            Paragraph::new(sample)
                .wrap(Wrap { trim: false })
                .render(area, &mut buf);
            let dump = buffer_text(&buf, area);

            assert!(
                dump.chars().any(|ch| !ch.is_whitespace()),
                "{label} sample produced an empty render"
            );
        }
    }

    fn buffer_text(buf: &Buffer, area: Rect) -> String {
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }
}
