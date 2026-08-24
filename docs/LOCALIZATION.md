# Localization Matrix

Status date: 2026-08-24

This document tracks UI localization only. It does not change model output language or provider behavior. Media attachments remain local path text references unless native media payload support is added separately.

## Source Audit

The v0.7.6 parity check used live GitHub sources with `/opt/homebrew/bin/gh`.

| Project | Ref | Evidence | Result |
|---|---:|---|---|
| Codex CLI | `openai/codex@df966996a75333add031fca47b72655e9ee504fd` | `gh repo view openai/codex`; recursive tree scan for `locale`, `i18n`, `l10n`, `translation`, `messages`; README language scan | No checked-in CLI UI localization registry found in the audited tree. Treat Codex CLI parity as English-first terminal UI behavior, not a source for shipped locale tags. |
| opencode | `anomalyco/opencode@00bb9836a60f1dcdd0ce5078b05d12f749fdde66` | `packages/console/app/src/lib/language.ts`, `packages/app/src/context/language.tsx`, `packages/web/src/i18n/locales.ts`, `packages/app/src/i18n/parity.test.ts` | opencode ships app/docs locale infrastructure with language detection, locale labels, docs locale aliases, RTL direction for Arabic, and parity tests for targeted keys. |

## Shipped Locales

These locales are supported by `locale` in `settings.toml` and by `LANG` / `LC_ALL` auto-detection. Language support was narrowed to this set on 2026-08-24; previously shipped `ja`, `pt-BR`, and `vi` fall back to English, and the planned Global South QA matrix was dropped.

| Locale | Display | Script | Direction | Fallback | Review status | Notes |
|---|---|---|---|---|---|---|
| `en` | English | Latin | LTR | `en` | Source strings remain canonical. | English is always available. |
| `zh-Hans` | Chinese Simplified | Hans | LTR | `en` | Native review. | `zh`, `zh-CN`, and `zh-Hans` resolve here. Core TUI chrome plus prompt-side reinforcement bookends. |
| `zh-Hant` | Chinese Traditional | Hant | LTR | `zh-Hans` | Native review. | `zh-TW`, `zh-HK`, `zh-MO`, and `zh-Hant` resolve here; shares the Simplified table except for divergent strings. |
| `hi` | Hindi | Deva | LTR | `en` | Automated QA only; native review preferred. | Full UI chrome coverage; narrow-width and truncation tests cover Devanagari. Prompt-side reinforcement bookends included. |
| `es-419` | Spanish (Latin America) | Latin | LTR | `en` | Automated QA only; native review preferred. | `es` and regional variants resolve here. Prompt-side reinforcement bookends included. |

Selection:

```toml
locale = "auto"     # default; checks LC_ALL, LC_MESSAGES, then LANG
locale = "en"
locale = "zh-Hans"
locale = "zh-Hant"
locale = "hi"
locale = "es-419"
```

Fallback:

- Missing or unsupported configured locales fall back to English.
- `auto` falls back to English when no supported environment locale is detected.
- The resolved locale is included in the system prompt as the fallback natural
  language for V4 reasoning and replies. The latest user message takes priority,
  including for `reasoning_content`, so a Chinese turn should remain Chinese
  even when the resolved locale is English.
- For `zh-Hans`, `hi`, and `es-419` the system prompt additionally carries
  native-script reinforcement bookends (preamble + closer) that steer
  `reasoning_content` and final replies toward the locale language; see
  `crates/agent-runtime/src/prompts.rs`.

## Message Coverage

The registry covers stable message IDs for high-visibility terminal chrome:

- composer placeholder
- composer history search title, placeholder, hints, and no-match state
- `/config` title, filter placeholder, no-match state, filtered count, and footer hints
- help overlay title, filter placeholder, no-match state, section labels, and footer hints
- slash-command descriptions, keybinding labels, onboarding screens, and the context menu

Not translated:

- model/system prompts and personalities
- provider or tool schemas
- full slash-command descriptions and every status/toast/error path beyond the registry above
- README/docs content beyond this configuration note

## Translator Notes

Keep these technical terms stable unless a later glossary explicitly changes
them: `Plan`, `Agent`, `YOLO`, `/config`, `/mcp`, `@path`, `/attach`, `DeepSeek`,
`MCP`, `CLI`, `TUI`, and key chords such as `Enter`, `Esc`, `Tab`, `PgUp`, and
`PgDn`.

## QA Checklist

Before shipping a new locale:

1. Add complete message coverage in `crates/tui/src/localization.rs`.
2. Add locale resolution tests and missing-key tests.
3. Add narrow-width render coverage for at least composer, help, and `/config`.
4. Verify CJK width, combining marks, and truncation.
5. Record native/fluent review status, or mark the locale as automated-QA-only.
