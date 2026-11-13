// Opt-in builtin compaction strategy — pi-style token-granular retention.
// Ported from a comparative study of popular coding agents
// (decision points 1-2, 4):
//   trigger : tokens > window − 16,384 (the output reserve)
//   plan    : keepRecentTokens 20,000 — the host backward-accumulates
//             whole rounds until the budget is met, then aligns the cut
//             to a pairing-safe boundary
//   summary : cap 0.8 × reserve = 13,107; a split turn opens a mini
//             per-turn summary section instead of vanishing
// Enable: [mods] builtin = ["compact-pi"] + [compaction] strategy = "compact-pi".

register_compaction_strategy("compact-pi", #{
    trigger: |snap| {
        if snap.window > 0 {
            snap.total_tokens > snap.window - 16384
        } else {
            // Window unknown (embeds/tests) — fall back to the configured
            // builtin threshold rather than firing early.
            snap.total_tokens > snap.threshold
        }
    },
    plan: |snap| #{
        keep_recent_tokens: 20000,
        summary_instructions: "If the retained tail starts mid-way through a turn that the summary dropped, open the summary with a section titled '**Turn Context (split turn):**' stating what that partial turn was doing before the drop, so the model can resume it without re-reading.",
        summary_max_tokens: 13107,
    },
});
