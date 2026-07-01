# Plan 02: Tool-Result Scrubbing Before Transcript

**Finding:** 5b (log-layer sanitization — tool results before transcript)
**Status:** Implemented
**Depends on:** Plan 01 (`partially_sanitize_unicode`)
**Blocks:** none

## Context

Claude Code runs MCP tool return values through `recursivelySanitizeUnicode`
before recording them to the transcript. CodeSmith does not:
`compact_tool_result_for_context` (`crates/agent-runtime/src/engine/context.rs:300`)
only truncates/compacts large outputs to protect the context window; the
compacted-but-unscrubbed content is then appended directly to the session
transcript via `add_session_message(... ContentBlock::ToolResult ...)` at
`crates/agent-runtime/src/engine/turn_loop.rs:2489-2498`. A tool result
containing hidden Unicode can therefore reach the model context.

## Deliverables

### 1. Sanitize the success path

In `compact_tool_result_for_context` (`crates/agent-runtime/src/engine/context.rs:300-335`),
replace:

```rust
let raw = output.content.trim();
```

with:

```rust
let raw = crate::sanitization::partially_sanitize_unicode(output.content.trim());
```

`raw` becomes `String`; adjust the `!should_compact` verbatim return at
`context.rs:319` (`raw.to_string()` → `raw.clone()` or return `raw` directly)
and the head/tail path at `context.rs:322`
(`summarize_text_head_tail(raw, ...)` → `summarize_text_head_tail(&raw, ...)` /
`summarize_text_head_tail(raw.as_str(), ...)`). This is the single chokepoint
feeding `output_for_context` at `turn_loop.rs:2493`.

### 2. Sanitize the error path

The error path at `turn_loop.rs:2531-2538` builds its content from
`format!("Error: {error}")` (where `error = format_tool_error(&e, &outcome.name)`,
`turn_loop.rs:2523`) and bypasses `compact_tool_result_for_context`. Sanitize
the error string before formatting:

```rust
let error = crate::sanitization::partially_sanitize_unicode(&format_tool_error(&e, &outcome.name));
```

### 3. Tests (in `context.rs` test module)

- A tool result whose content contains `U+200B` / `U+FEFF` is sanitized before
  the returned string.
- Idempotence on an already-clean result.
- A large result is still compacted (head/tail) after sanitization — i.e. the
  sanitization does not bypass the compaction path.

## Scope boundary

This plan does **Unicode scrubbing of tool outputs** only (the reference's
`recursivelySanitizeUnicode`-on-tool-results behavior). **Secret redaction of
tool outputs** is explicitly out of scope here — existing redaction covers
log/error/export paths (`mcp.rs:324/343/372`, `config/src/lib.rs:~2194`,
`slop_ledger.rs:930`); transcript secret redaction is a separate concern.

## Stop rules

- Do not alter the truncation/compaction logic of `compact_tool_result_for_context`
  — only insert one sanitization step after `raw` is computed.
- Do not change `ContentBlock::ToolResult` shape or the `add_session_message`
  call site.
- Do not redact secrets in this plan.

## Files

- `crates/agent-runtime/src/engine/context.rs` (`:300-335`)
- `crates/agent-runtime/src/engine/turn_loop.rs` (`:2523`, `:2531-2538`)
