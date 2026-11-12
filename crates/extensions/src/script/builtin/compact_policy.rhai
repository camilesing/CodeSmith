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
//   elide_uneventful          — omp-style per-turn elision lane: 1 = an
//                               uneventful result (successful, media-free,
//                               no error marker) in a completed round
//                               elides on its presence alone; the live
//                               round stays byte-for-byte
//   elide_placeholder         — what an elided result becomes (omp ships
//                               "[Uneventful result elided]")

register_policy("compact-policy", #{
    cache_trigger_threshold: 32768,
    time_trigger_gap_secs: 3600,
    micro_compactable_tools: [
        "file_read", "exec_shell", "exec_shell_wait", "exec_shell_interact",
        "grep", "glob", "web_search", "web_fetch", "write_file",
        "apply_patch", "edit_file",
    ],
    cleared_placeholder: "[tool result cleared for context economy]",
    keep_head_chars: 0,
    keep_tail_chars: 0,
    elide_uneventful: 0,
    elide_placeholder: "[Uneventful result elided]",
});
