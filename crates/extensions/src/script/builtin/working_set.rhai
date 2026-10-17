// Builtin default policy: working-set sizing — the recent-read-files
// window the session keeps so compacted-away file reads stay available to
// the model. The mechanism (dedup by path, ordering, eviction, the
// hidden-Unicode sanitization at the recording choke point) stays in the
// host (`crates/agent-runtime/src/session.rs`); this policy owns the
// numbers.
//
//   recent_read_file_limit          — how many distinct files the window
//                                      retains
//   recent_read_file_snippet_chars  — preview snippet kept per file

register_policy("working-set", #{
    recent_read_file_limit: 12,
    recent_read_file_snippet_chars: 1200,
});
