// Opt-in builtin sweep rule — omp-style per-turn elision.
// Ported from oh-my-pi @33ea58aa (articles/CodingAgent/Compare/
// Compaction.md decision point 3, "sweep before move"): a successful,
// media-free result in a COMPLETED round whose body carries no error
// marker collapses to `[Uneventful result elided]` — and its presence
// alone trips the pass (loading this rule IS the cadence switch; no host
// knob gates it). The live round stays byte-for-byte (omp's cache-safe
// lookback window, as an AppendLog rebuild). Failure lines stay for
// later turns — the error-marker set mirrors
// codesmith_session::transcript::ERROR_MARKERS.
// Enable: [mods] builtin = ["elide-uneventful"].

register_sweep_rule("uneventful", #{
    eligible: |r| {
        if r.in_live_round {
            return false;
        }
        if r.is_error {
            return false;
        }
        if r.has_media {
            return false;
        }
        // `content_head` arrives lowercased from the host (≤512 chars).
        for marker in [
            "error:", "error ", "failed", "panic",
            "traceback", "stack trace", "assertion failed", "test failed",
        ] {
            if r.content_head.contains(marker) {
                return false;
            }
        }
        true
    },
    placeholder: "[Uneventful result elided]",
});
