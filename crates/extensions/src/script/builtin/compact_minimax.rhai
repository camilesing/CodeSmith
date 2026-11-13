// Opt-in builtin compaction strategy — minimax-style 95% trigger.
// Ported from a comparative study of popular coding agents
// (decision points 1-2):
//   trigger : input > 95% × (window − min(outputCap, window/4)) — integer
//             percent, so the trigger is bit-reproducible; the output
//             reserve is the model's own output ceiling, capped at
//             window/4 (a larger declared output cap must not wedge the
//             session at a tiny input budget); window/4 stands in when
//             the snapshot carries no ceiling
//   plan    : keep the last 5 rounds; Todo/Plan state stays out of the
//             summary (the host keeps that state outside the checkpoint)
//   summary : cap min(0.8 × reserve, window/8, output ceiling when known)
// Enable: [mods] builtin = ["compact-minimax"]
//       + [compaction] strategy = "compact-minimax".

register_compaction_strategy("compact-minimax", #{
    trigger: |snap| {
        if snap.window > 0 {
            let cap = snap.output_cap ?? 0;
            let reserve = if cap > 0 { min(cap, snap.window / 4) } else { snap.window / 4 };
            let budget = snap.window - reserve;
            snap.total_tokens > budget * 95 / 100
        } else {
            snap.total_tokens > snap.threshold
        }
    },
    plan: |snap| #{
        keep_recent_rounds: 5,
        summary_instructions: "Exclude todo-list and plan state from the summary sections — the host carries that state outside the compaction checkpoint; summarize conversation and findings only.",
        summary_max_tokens: if snap.window > 0 {
            let cap = snap.output_cap ?? 0;
            let reserve = if cap > 0 { min(cap, snap.window / 4) } else { snap.window / 4 };
            let summary = min(reserve * 8 / 10, snap.window / 8);
            if cap > 0 { min(summary, cap) } else { summary }
        } else {
            4096
        },
    },
});
