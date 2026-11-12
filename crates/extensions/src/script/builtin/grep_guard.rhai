// Opt-in builtin tool hardening — grep input normalization.
// Ported from minimax-code @3c21163c (articles/CodingAgent/Compare/
// CodingTools.md decision point 1): the argv hardening points that have a
// referent under structured (non-argv) parameters. The `--` separator and
// last-match-wins deny-list are argv-specific and have no referent here;
// CodeSmith's grep already reports `truncated` (the --max-count surplus
// row's purpose) and takes include patterns as a typed array.
//   1. an empty / whitespace-only pattern fails with a corrective action
//      instead of matching everything;
//   2. include glob entries split on commas (preserving `{ts,tsx}` brace
//      groups, minimax's rule) — `"ts, tsx"` becomes two entries.
// Enable: [mods] builtin = ["grep-guard"].

replace_tool(#{ name: "grep" }, |input, ctx| {
    let pattern = input.get("pattern");
    pattern.trim();
    if pattern == "" {
        return err("grep: `pattern` is empty or whitespace — that matches everything. NEVER retry with the same input; pass a concrete pattern (or a path/include glob if you meant to browse).");
    }
    // Normalize include globs: comma-separated entries split; brace
    // groups pass through verbatim so `{ts,tsx}` stays one glob.
    if "include" in input {
        let normalized = [];
        for glob in input.include {
            if glob.contains("{") {
                normalized.push(glob);
            } else if glob.contains(",") {
                for part in glob.split(",") {
                    part.trim();
                    if part != "" {
                        normalized.push(part);
                    }
                }
            } else {
                normalized.push(glob);
            }
        }
        input.include = normalized;
    }
    delegate(input)
});
