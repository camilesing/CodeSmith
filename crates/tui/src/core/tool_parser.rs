//! Re-export of the tool-call text parser.
//!
//! Canonical home: `codesmith_agent_runtime::tool_parser`. The implementation
//! moved to `codesmith-agent-runtime` so the engine body (which also moves
//! there in a later phase) can call `has_tool_call_markers` /
//! `parse_tool_calls` without a TUI dependency.

