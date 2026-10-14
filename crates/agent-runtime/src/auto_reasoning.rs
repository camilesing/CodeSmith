//! Adaptive reasoning-effort tier selection for `Auto` mode (#663).
//!
//! When the user sets `reasoning_effort = "auto"`, the engine calls
//! [`select`] before each turn-level request to pick the actual tier
//! based on the current message.
//!
//! Migrated from the TUI (`crates/tui/src/auto_reasoning.rs`) as part of
//! the engine-closure extraction — the engine body references this via
//! `crate::auto_reasoning::select`.

use unicode_normalization::UnicodeNormalization;

use crate::mode::ReasoningEffort;

/// Choose a concrete `ReasoningEffort` tier for the next API request.
///
/// Rules:
/// - Sub-agent contexts (`is_subagent == true`) → `Low`
/// - Last user message contains a high-effort keyword
///   (English: `debug`, `error`, `bug`; Chinese: 调试 / 错误 / 报错 / 出错 /
///   崩溃 / 調試 / 錯誤; Japanese: デバッグ / エラー / バグ) → `Max`
/// - Last user message contains a low-effort keyword
///   (English: `search`, `lookup`; Chinese: 搜索 / 查找 / 查询;
///   Japanese: 検索) → `Low`
/// - Everything else → `High`
///
/// ASCII keywords match on word boundaries and CJK keywords on substrings
/// (see [`matches_keyword`]).
#[must_use]
pub fn select(is_subagent: bool, last_msg: &str) -> ReasoningEffort {
    if is_subagent {
        return ReasoningEffort::Low;
    }

    // NFKC first, then Unicode lowercasing: `to_ascii_lowercase` alone left
    // full-width Latin (ｅｒｒｏｒ, common CJK-IME output) and half-width
    // katakana (ﾃﾞﾊﾞｯｸﾞ) unmatched against every keyword — the exact gap the
    // CJK expansion meant to close.
    let lower: String = last_msg.nfkc().flat_map(char::to_lowercase).collect();

    if HIGH_EFFORT_KEYWORDS
        .iter()
        .any(|kw| matches_keyword(&lower, kw))
    {
        return ReasoningEffort::Max;
    }

    if LOW_EFFORT_KEYWORDS
        .iter()
        .any(|kw| matches_keyword(&lower, kw))
    {
        return ReasoningEffort::Low;
    }

    ReasoningEffort::High
}

/// Keyword containment test. ASCII keywords match on word boundaries so a
/// keyword cannot fire on an unrelated superstring — `"research this"`
/// must not hit `search` (dropping an open-ended task to `Low`), and
/// `"terror"` must not hit `error` (spiking to `Max`). CJK text has no
/// word separators, so those keywords keep substring matching.
fn matches_keyword(lower_haystack: &str, kw: &str) -> bool {
    if kw.is_ascii() {
        lower_haystack
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word == kw)
    } else {
        lower_haystack.contains(kw)
    }
}

/// Keywords that bump `reasoning_effort` to `Max`. Latin terms are
/// lowercase because the caller lowercases the message; CJK has no
/// case so the literal form matches as-is. Covers the Chinese and
/// Japanese vocabulary a non-English user reaches for when reporting
/// the same kind of problem the original `"debug" | "error"` rule was
/// trying to catch — without those terms a Chinese-speaking user
/// paying for Auto mode silently got `High` even on hard debugging
/// tasks.
const HIGH_EFFORT_KEYWORDS: &[&str] = &[
    // English (the original keyword set, plus `bug` to keep parity with
    // Japanese バグ below — safe under word-boundary matching, where the
    // substring "bug" would otherwise collide with unrelated words).
    "debug",
    "error",
    "bug",
    // Simplified / Traditional Chinese.
    "\u{8c03}\u{8bd5}", // 调试
    "\u{9519}\u{8bef}", // 错误
    "\u{62a5}\u{9519}", // 报错
    "\u{51fa}\u{9519}", // 出错
    "\u{5d29}\u{6e83}", // 崩溃
    "\u{8abf}\u{8a66}", // 調試
    "\u{932f}\u{8aa4}", // 錯誤
    // Japanese.
    "\u{30c7}\u{30d0}\u{30c3}\u{30b0}", // デバッグ
    "\u{30a8}\u{30e9}\u{30fc}",         // エラー
    "\u{30d0}\u{30b0}",                 // バグ
];

/// Keywords that drop `reasoning_effort` to `Low`. Same locale coverage
/// as [`HIGH_EFFORT_KEYWORDS`].
const LOW_EFFORT_KEYWORDS: &[&str] = &[
    "search",
    "lookup",
    "\u{641c}\u{7d22}", // 搜索
    "\u{67e5}\u{627e}", // 查找
    "\u{67e5}\u{8be2}", // 查询
    "\u{691c}\u{7d22}", // 検索
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subagent_returns_low() {
        assert_eq!(select(true, "anything"), ReasoningEffort::Low);
        assert_eq!(select(true, "debug this"), ReasoningEffort::Low);
        assert_eq!(select(true, "search query"), ReasoningEffort::Low);
    }

    #[test]
    fn debug_or_error_returns_max() {
        // `bug` is a Max keyword (parity with Japanese バグ) — safe under
        // word-boundary matching.
        assert_eq!(select(false, "find a bug"), ReasoningEffort::Max);
        assert_eq!(select(false, "debug crash"), ReasoningEffort::Max);
        assert_eq!(select(false, "Error: timeout"), ReasoningEffort::Max);
        assert_eq!(select(false, "fix this error"), ReasoningEffort::Max);
        assert_eq!(select(false, "DEBUG output"), ReasoningEffort::Max);
        // Boundary matching: a keyword must not fire on an unrelated
        // superstring.
        assert_eq!(
            select(false, "research this algorithm"),
            ReasoningEffort::High
        );
        assert_eq!(select(false, "a terror of clouds"), ReasoningEffort::High);
    }

    #[test]
    fn search_or_lookup_returns_low() {
        assert_eq!(select(false, "search for the file"), ReasoningEffort::Low);
        assert_eq!(select(false, "lookup docs"), ReasoningEffort::Low);
        // `lookup_user` splits on `_` (non-alphanumeric separator), so the
        // `lookup` word still matches. "SearchQuery" is one word — the
        // identifier mention is not a low-effort request under boundary
        // matching.
        assert_eq!(select(false, "lookup_user"), ReasoningEffort::Low);
        assert_eq!(select(false, "SearchQuery"), ReasoningEffort::High);
    }

    #[test]
    fn high_effort_keyword_wins_over_low() {
        // Both "search" (Low) and "error" (Max) present → Max. Pins the
        // check order: the high list must stay first.
        assert_eq!(
            select(false, "search the logs for this error"),
            ReasoningEffort::Max,
        );
    }

    #[test]
    fn full_width_latin_and_half_width_kana_match_after_nfkc() {
        // ｅｒｒｏｒ (full-width, CJK-IME output) and ﾃﾞﾊﾞｯｸﾞ (half-width
        // katakana) must reach the same keywords as their NFKC folds.
        assert_eq!(select(false, "ｅｒｒｏｒが発生した"), ReasoningEffort::Max);
        assert_eq!(select(false, "ﾃﾞﾊﾞｯｸﾞしたい"), ReasoningEffort::Max);
    }

    #[test]
    fn default_returns_high() {
        assert_eq!(select(false, "hello"), ReasoningEffort::High);
        assert_eq!(select(false, "write a test"), ReasoningEffort::High);
        assert_eq!(select(false, "refactor this module"), ReasoningEffort::High);
        assert_eq!(select(false, ""), ReasoningEffort::High);
    }

    #[test]
    fn chinese_debug_keywords_return_max() {
        // The original keyword set was English-only; Chinese-speaking
        // Auto-mode users paid for `High` even on real debugging tasks.
        for msg in [
            "\u{5e2e}\u{6211}\u{8c03}\u{8bd5}\u{4ee3}\u{7801}", // 帮我调试代码
            "\u{8fd9}\u{91cc}\u{6709}\u{4e2a}\u{9519}\u{8bef}", // 这里有个错误
            "\u{4ee3}\u{7801}\u{62a5}\u{9519}\u{4e86}",         // 代码报错了
            "\u{7a0b}\u{5e8f}\u{51fa}\u{9519}",                 // 程序出错
            "\u{7cfb}\u{7edf}\u{5d29}\u{6e83}",                 // 系统崩溃
            "\u{4ee3}\u{78bc}\u{8abf}\u{8a66}",                 // 代碼調試 (zh-Hant)
            "\u{6709}\u{500b}\u{932f}\u{8aa4}",                 // 有個錯誤 (zh-Hant)
        ] {
            assert_eq!(
                select(false, msg),
                ReasoningEffort::Max,
                "expected Max for `{msg}`",
            );
        }
    }

    #[test]
    fn japanese_debug_keywords_return_max() {
        for msg in [
            "\u{30b3}\u{30fc}\u{30c9}\u{3092}\u{30c7}\u{30d0}\u{30c3}\u{30b0}", // コードをデバッグ
            "\u{30a8}\u{30e9}\u{30fc}\u{304c}\u{51fa}\u{305f}",                 // エラーが出た
            "\u{30d0}\u{30b0}\u{3092}\u{4fee}\u{6b63}",                         // バグを修正
        ] {
            assert_eq!(
                select(false, msg),
                ReasoningEffort::Max,
                "expected Max for `{msg}`",
            );
        }
    }

    #[test]
    fn chinese_search_keywords_return_low() {
        for msg in [
            "\u{641c}\u{7d22}\u{4e00}\u{4e0b}\u{6587}\u{4ef6}", // 搜索一下文件
            "\u{5e2e}\u{6211}\u{67e5}\u{627e}\u{5b9a}\u{4e49}", // 帮我查找定义
            "\u{67e5}\u{8be2}\u{6587}\u{6863}",                 // 查询文档
        ] {
            assert_eq!(
                select(false, msg),
                ReasoningEffort::Low,
                "expected Low for `{msg}`",
            );
        }
    }

    #[test]
    fn japanese_search_keyword_returns_low() {
        // 検索 → "search"
        assert_eq!(
            select(
                false,
                "\u{30c9}\u{30ad}\u{30e5}\u{30e1}\u{30f3}\u{30c8}\u{691c}\u{7d22}"
            ),
            ReasoningEffort::Low,
        );
    }

    #[test]
    fn cjk_default_still_returns_high() {
        // No keyword hits — ordinary Chinese/Japanese prose stays on
        // the `High` default like English does.
        for msg in [
            "\u{5e2e}\u{6211}\u{5199}\u{4e2a}\u{6d4b}\u{8bd5}", // 帮我写个测试
            "\u{91cd}\u{6784}\u{8fd9}\u{4e2a}\u{6a21}\u{5757}", // 重构这个模块
            "\u{30c6}\u{30b9}\u{30c8}\u{3092}\u{66f8}\u{304f}", // テストを書く
        ] {
            assert_eq!(
                select(false, msg),
                ReasoningEffort::High,
                "expected High for `{msg}`",
            );
        }
    }
}
