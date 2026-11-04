// Builtin default policy: Knowledge On Demand (memory surfacing). The
// scan / relevance side-query / truncation mechanics stay in the host
// (`crates/context/src/knowledge/`); this policy owns the knobs.
//
//   max_session_bytes     — session-wide byte budget for surfaced memories
//   max_entrypoint_lines  — MEMORY.md index line budget (curator pass)
//   max_entrypoint_bytes  — MEMORY.md index byte budget (curator pass)
//   enabled_types         — memory types eligible for surfacing
//                           (user / feedback / project / reference;
//                           untyped memory files stay eligible)
//
// Deliberately absent: a relevance threshold — the relevance step is an
// LLM side-query that picks filenames, not a scored ranking, so there is
// no number to gate on (the selection prompt and per-turn cap stay in
// the host).

register_policy("knowledge-on-demand", #{
    max_session_bytes: 500000,
    max_entrypoint_lines: 200,
    max_entrypoint_bytes: 25000,
    enabled_types: ["user", "feedback", "project", "reference"],
});
