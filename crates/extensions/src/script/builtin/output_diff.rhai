// Builtin default policy: unified-diff rendering for file-edit tool
// results. The diff algorithm and header escaping stay in the host
// (`crates/agent-runtime/src/tools/diff_format.rs`); this policy owns the
// rendering knob.
//
//   context_radius — lines of unchanged context around each hunk.
//     The TUI's diff detector requires the header layout (a/, b/, @@ at
//     line 3); the radius itself is free. Keep it small: a large radius
//     floods the model's context with unchanged lines.

register_policy("output-diff", #{
    context_radius: 3,
});
