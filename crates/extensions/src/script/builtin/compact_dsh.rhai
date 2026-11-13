// Opt-in builtin compaction strategy — dsh-style 16% tail retention.
// Ported from a comparative study of popular coding agents
// (decision points 1-2):
//   trigger : floor(min(0.8 × window, window − 65,536 headroom))
//   plan    : retain 16% of the window as the kept tail, deliberately
//             even mid-turn (the host re-aligns to whole rounds — pairing
//             is an immune zone)
//   summary : cap 65,536
// Deviation: dsh also subtracts a configured reservedCompletion from the
// trigger; the snapshot does not carry the per-model output ceiling, so
// the 65,536 headroom absorbs it (documented in the seam's known
// limitations).
// Enable: [mods] builtin = ["compact-dsh"] + [compaction] strategy = "compact-dsh".

register_compaction_strategy("compact-dsh", #{
    trigger: |snap| {
        if snap.window > 0 {
            let threshold = min(snap.window * 8 / 10, snap.window - 65536);
            snap.total_tokens > threshold
        } else {
            snap.total_tokens > snap.threshold
        }
    },
    plan: |snap| #{
        keep_recent_tokens: if snap.window > 0 { snap.window * 16 / 100 } else { 16384 },
        summary_max_tokens: 65536,
    },
});
