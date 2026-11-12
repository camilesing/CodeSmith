// Opt-in builtin tool hardening — edit input guards.
// Ported from minimax-code/deepseek-harness (articles/CodingAgent/Compare/
// CodingTools.md decision point 2): the parameter-level layers of the
// fidelity/disproportion guards. CodeSmith's edit_file already enforces
// the deeper span-level checks in Rust (exact match → indentation fuzzy →
// punctuation fuzzy, occurrence disambiguation, start/end anchors); this
// guard rejects the two calls that should never reach them:
//   1. `search == replace` (dsh's param-layer rule) — a no-op edit;
//   2. a replacement disproportionately larger than the search span
//      (minimax disproportion, input-level approximation: replace lines >
//      max(search lines × 2, search lines + 3)) — the classic wrong-span
//      rewrite. Both denials carry the corrective next step and the
//      "NEVER retry verbatim" discipline.
// Enable: [mods] builtin = ["edit-input-guard"].

on("tool-call", |e| {
    if e.name != "edit_file" {
        return proceed();
    }
    let search = e.input.get("search");
    let replace = e.input.get("replace");
    if search != "" && search == replace {
        return block("edit_file: `search` and `replace` are identical — that edits nothing. NEVER retry the same call; if you meant to verify content, use read_file instead.");
    }
    let s_lines = if search == "" { 0 } else { search.split("\n").len() };
    let r_lines = if replace == "" { 0 } else { replace.split("\n").len() };
    if r_lines > max(s_lines * 2, s_lines + 3) {
        return block("edit_file: the replacement is disproportionately larger than the search span — this is the wrong-match rewrite pattern. NEVER retry verbatim; re-read the file around the target and narrow `search` (or use search_start/search_end anchors).");
    }
    proceed()
});
