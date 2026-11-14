// Builtin default policy: micro-compaction. The compaction engine (what
// to keep, ordering, retries, session-memory merge) stays in the host
// (`crates/agent-runtime/src/compaction/`); this policy owns the knobs.
//
//   cache_trigger_threshold   — accumulated cleared bytes before a
//                               cache-trigger micro-compaction
//   time_trigger_gap_secs     — silent gap that trips a time trigger
//   micro_compactable_tools   — tool names whose results may be cleared
//   cleared_placeholder       — the text a cleared result becomes
//   keep_head_chars           — dsh-style partial clear: characters of the
//                               head kept around the placeholder (0 = bare
//                               placeholder; dsh ships 4096 — search hits
//                               live at the head)
//   keep_tail_chars           — same for the tail (dsh ships 1024 — shell
//                               errors live at the tail)
register_policy("compact-policy", #{
    cache_trigger_threshold: 32768,
    time_trigger_gap_secs: 3600,
    // Canonical registered names first; the legacy spellings stay so a
    // resumed pre-rename transcript (tool_use blocks carrying the old
    // names) still sweeps.
    micro_compactable_tools: [
        "read_file", "grep_files", "file_search", "fetch_url",
        "exec_shell", "exec_shell_wait", "exec_shell_interact",
        "web_search", "write_file", "apply_patch", "edit_file",
        "file_read", "grep", "glob", "web_fetch",
    ],
    cleared_placeholder: "[tool result cleared for context economy]",
    keep_head_chars: 0,
    keep_tail_chars: 0,
});
