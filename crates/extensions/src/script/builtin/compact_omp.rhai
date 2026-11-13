// Opt-in builtin compaction strategy — omp-style band retention.
// Ported from a comparative study of popular coding agents
// (decision points 1-2, 7):
//   reserve : max(⌊window × 0.15⌋, 16,384) — floored AND window-scaled;
//             clamped to the model's output ceiling when the snapshot
//             carries one (a reserve larger than the model can emit back
//             is wasted window)
//   trigger : tokens > window − reserve
//   band    : the kept tail is clamped so post-compaction usage lands
//             inside 0.8 × (window − reserve) — the recovery band
//             (hysteresis; fixes the re-trigger loop of issue #2275)
//   summary : absolute 16,384 cap — "the model copies rather than
//             compresses" at larger budgets (deliberately NOT window- or
//             cap-scaled; that is omp's own counter-argument)
// Enable: [mods] builtin = ["compact-omp"] + [compaction] strategy = "compact-omp".

register_compaction_strategy("compact-omp", #{
    trigger: |snap| {
        if snap.window > 0 {
            let cap = snap.output_cap ?? 0;
            let reserve = max(snap.window * 15 / 100, 16384);
            let reserve = if cap > 0 { min(reserve, cap) } else { reserve };
            snap.total_tokens > snap.window - reserve
        } else {
            snap.total_tokens > snap.threshold
        }
    },
    plan: |snap| {
        let cap = snap.output_cap ?? 0;
        let reserve = if snap.window > 0 {
            let r = max(snap.window * 15 / 100, 16384);
            if cap > 0 { min(r, cap) } else { r }
        } else {
            16384
        };
        let band = if snap.window > 0 {
            (snap.window - reserve) * 8 / 10
        } else {
            snap.threshold * 8 / 10
        };
        // Clamp the kept tail to whole rounds that fit inside the band:
        // accumulate from the tail while the next round still fits, so
        // the host's "keep at least N tokens" budget lands exactly on
        // that same round boundary.
        let kept = 0;
        let rounds = snap.rounds;
        let i = rounds.len() - 1;
        while i >= 0 {
            let next = kept + rounds[i].tokens;
            if next > band {
                break;
            }
            kept = next;
            i -= 1;
        }
        #{
            keep_recent_tokens: max(kept, 1),
            summary_max_tokens: 16384,
        }
    },
});
