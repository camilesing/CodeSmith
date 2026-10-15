// Builtin default policy: shell-output truncation and summarization
// budgets. The mechanism (char-boundary slicing, spill writes, high-signal
// summary-line detection) stays in the host
// (`crates/agent-runtime/src/tools/shell_output.rs`); this policy owns
// the numbers.
//
//   max_output_size          — bytes before truncation applies
//   head_bytes / tail_bytes  — kept when truncated (head + tail ≤ max)
//   summary_max_lines        — summary string line cap (tool metadata)
//   summary_max_chars        — summary string char cap (tool metadata)
//   preserved_summary_lines  — high-signal lines kept from the omitted
//                              middle (errors, test results, panics)
//   preserved_line_bytes     — per-line cap for a preserved line
//   preserved_total_bytes    — total cap for preserved lines

register_policy("shell-output", #{
    max_output_size: 30000,
    head_bytes: 22000,
    tail_bytes: 8000,
    summary_max_lines: 3,
    summary_max_chars: 240,
    preserved_summary_lines: 80,
    preserved_line_bytes: 512,
    preserved_total_bytes: 8192,
});
