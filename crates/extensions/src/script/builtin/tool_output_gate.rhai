// Builtin default policy: tool-output gate budgets for the read/search
// surfaces (the omp-style unified OutputSink tiers). The gates themselves
// (UTF-8-safe slicing, continuation notices phrased as executable
// read-more instructions) stay in Rust; this policy owns the tiers.
//
//   read_max_lines       — default lines per read_file window
//   read_hard_max_lines  — ceiling a caller's explicit max_lines clamps to
//   read_max_bytes       — byte budget of the rendered window
//   grep_max_results     — default match cap for grep
//
// Defaults are the pre-policy compiled constants. The omp unified tier
// (3000 lines / 50 KiB / 512 bytes-per-line) is one register_policy away.

register_policy("tool-output-gate", #{
    read_max_lines: 200,
    read_hard_max_lines: 500,
    read_max_bytes: 16384,
    grep_max_results: 100,
});
