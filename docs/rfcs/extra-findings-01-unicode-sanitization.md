# Plan 01: Unicode Steganography Defense

**Finding:** 3 (Unicode hidden-character attack mitigation)
**Status:** Implemented
**Depends on:** none
**Blocks:** Plan 02

## Context

Claude Code's `src/utils/sanitization.ts` implements `partiallySanitizeUnicode`
and `recursivelySanitizeUnicode` to defend against Unicode-based hidden prompt
injection (HackerOne #3086545 — Unicode Tag characters invisible to users but
processed by the model). CodeSmith has no equivalent: every `sanitiz*` function
in the workspace is about schema/name/path hygiene, not string-content
steganography. Crucially, `McpPool::call_tool` forwards MCP tool arguments
untouched, so steganographic data can ride tool-call `input` fields into the
model context.

## Deliverables

### 1. New dependencies

Add to `[workspace.dependencies]` in the root `Cargo.toml`:

- `unicode-normalization = "0.1"`
- `unicode-properties = "0.1"`

Reference them in `crates/agent-runtime/Cargo.toml` as
`unicode-normalization.workspace = true` / `unicode-properties.workspace = true`.
(`regex` is already a direct dependency.)

### 2. New module `crates/agent-runtime/src/sanitization.rs`

Registered via `pub mod sanitization;` in `crates/agent-runtime/src/lib.rs`
(alphabetical neighbor of `sandbox`/`session`). Module style mirrors
`crates/agent-runtime/src/tools/schema_sanitize.rs` (`//!` doc, `use` lines,
public entry point, private helpers, recursive walk).

Public API:

```rust
/// Maximum fixpoint iterations. Mirrors Claude Code's MAX_ITERATIONS = 10.
/// Exceeding this is a defense-in-depth signal, not a crash: we log and return.
const SANITIZE_MAX_ITERATIONS: usize = 10;

/// NFKC-normalize and strip dangerous Unicode (Cf/Co/Cn + explicit ranges,
/// including the U+E0000-E007F Tag characters that are the #3086545 vector).
/// Iterates to a fixpoint up to SANITIZE_MAX_ITERATIONS.
pub fn partially_sanitize_unicode(input: &str) -> String

/// Recursively sanitize a JSON value: Object keys AND values, Array elements,
/// and String leaves. Numbers/bools/null pass through.
pub fn recursively_sanitize_unicode(value: serde_json::Value) -> serde_json::Value
```

Algorithm per iteration of `partially_sanitize_unicode`:

1. `unicode_normalization::nfkc(&current)`.
2. Strip chars whose `unicode_properties` general category is `Format` (Cf),
   `PrivateUse` (Co), or `Unassigned` (Cn).
3. Explicit-range fallback (belt-and-suspenders, matches the reference's
   Step 3): `U+200B-200F`, `U+202A-202E`, `U+2066-2069`, `U+FEFF`,
   `U+E000-F8FF` (BMP PUA), `U+E0000-E007F` (Tags), `U+F0000-FFFFD` and
   `U+100000-10FFFD` (supplementary PUA).

If the fixpoint is not reached within `SANITIZE_MAX_ITERATIONS`,
`tracing::warn!` and return the current value. **Do not panic** on untrusted
input.

`recursively_sanitize_unicode` matches on `Value::String` / `Value::Array` /
`Value::Object` (sanitize keys via `partially_sanitize_unicode`), recursing
exactly as `schema_sanitize::sanitize` walks the schema tree.

### 3. MCP input injection

At the top of `McpPool::call_tool` (`crates/agent-runtime/src/mcp.rs:3008`,
immediately after the function signature, before the pseudo-tool dispatchers
`list_mcp_resources` / `read_mcp_resource` / etc.) insert:

```rust
let arguments = crate::sanitization::recursively_sanitize_unicode(arguments);
```

This single chokepoint covers both `conn.call_tool` forwards at `mcp.rs:3086`
(`.clone()`) and `mcp.rs:3102` (move), plus the pseudo-tool readers that read
from the same `arguments`.

### 4. Tests (`#[cfg(test)] mod tests` in `sanitization.rs`)

- Zero-width space `U+200B` stripped.
- BOM `U+FEFF` stripped.
- BMP PUA `U+E000` stripped.
- Tag character `U+E0001` stripped (the #3086545 vector).
- NFKC composes a compatibility-equivalent sequence.
- Recursive: object key and value both sanitized; array elements sanitized;
  numbers/bools/null untouched.
- Idempotence: sanitizing an already-sanitized string is a no-op.
- Iteration cap: a pathological input does not panic (returns a string).

## Stop rules

- Do not change MCP call semantics — only sanitize the `arguments` value.
- Do not introduce any panic path on untrusted input.
- Do not apply sanitization to non-MCP tool paths in this plan (that is Plan 02).

## Files

- `Cargo.toml` (workspace deps)
- `crates/agent-runtime/Cargo.toml`
- `crates/agent-runtime/src/lib.rs` (module registration)
- `crates/agent-runtime/src/sanitization.rs` (new)
- `crates/agent-runtime/src/mcp.rs` (injection at `:3008`)
