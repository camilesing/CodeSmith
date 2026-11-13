// Opt-in builtin compaction strategy — minimax-style 95% trigger.
// Ported from a comparative study of popular coding agents
// (decision points 1-2):
//   trigger : input > 95% × (window − min(outputCap, window/4)) — integer
//             percent, so the trigger is bit-reproducible; the output
//             reserve is capped at window/4 (a larger declared output cap
//             must not wedge the session at a tiny input budget)
//   plan    : keep the last 5 rounds; Todo/Plan state stays out of the
//             summary (the host keeps that state outside the checkpoint)
//   summary : cap window/8
// Deviation: outputCap is not carried in the snapshot — window/4 is
// minimax's own cap for every model with a larger declared output, so the
// formula degenerates to exactly that (documented in the seam's known
// limitations).
// Enable: [mods] builtin = ["compact-minimax"]
//       + [compaction] strategy = "compact-minimax".

register_compaction_strategy("compact-minimax", #{
    trigger: |snap| {
        if snap.window > 0 {
            let budget = snap.window - snap.window / 4;
            snap.total_tokens > budget * 95 / 100
        } else {
            snap.total_tokens > snap.threshold
        }
    },
    plan: |snap| #{
        keep_recent_rounds: 5,
        summary_instructions: "Exclude todo-list and plan state from the summary sections — the host carries that state outside the compaction checkpoint; summarize conversation and findings only.",
        summary_max_tokens: if snap.window > 0 { snap.window / 8 } else { 4096 },
    },
});
