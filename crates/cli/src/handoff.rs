// Used by the deferred context-limit relay feature (#667). The implementation
// path is staged but not yet wired from the engine; suppress dead-code warnings
// rather than delete the table until the follow-up feature consumes it.
#[allow(dead_code)]
pub const THRESHOLDS: [(f32, &str); 3] = [
    (
        0.9,
        "Context at 90%: stop and write relay to .codesmith/handoff.md now",
    ),
    (
        0.8,
        "Context at 80%: urgent hard-limit pressure; compact or write relay now",
    ),
    (
        0.6,
        "Context at 60%: prepare relay or suggest /compact for sustained work",
    ),
];
/// Message for the highest threshold the context `ratio` has reached.
///
/// `ratio` is the used/total context fraction; it is expected to be finite
/// and within `[0.0, 1.0]`. A non-finite value (the "broken upstream
/// computation" case) fails every comparison and must not read as
/// "below 60%" — the least safe outcome for a hard-limit warning — so it
/// fails closed by surfacing the most urgent message. Values outside
/// `[0.0, 1.0]` follow the same comparison chain as clamped values. The
/// lookup picks the maximum satisfied threshold and therefore does not
/// depend on the table's declaration order.
#[allow(dead_code)]
pub fn threshold_message(ratio: f32) -> Option<&'static str> {
    if !ratio.is_finite() {
        // NaN/±inf: fail closed — surface the most urgent message (the
        // table is descending, verified by
        // `thresholds_are_sorted_descending`), never the "below 60%"
        // silence that a false comparison chain yields.
        return THRESHOLDS.first().map(|(_, m)| *m);
    }
    let saturated = ratio.clamp(0.0, 1.0);
    THRESHOLDS
        .iter()
        .filter(|(t, _)| saturated >= *t)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, m)| *m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_are_sorted_descending() {
        assert!(THRESHOLDS.windows(2).all(|w| w[0].0 > w[1].0));
    }

    /// NaN compares false against every threshold; without the fail-closed
    /// arm it read as "below 60%" — the least safe outcome for a
    /// hard-limit warning.
    #[test]
    fn non_finite_ratio_fails_closed_to_most_urgent() {
        let urgent = THRESHOLDS.first().map(|(_, m)| *m);
        assert_eq!(threshold_message(f32::NAN), urgent);
        assert_eq!(threshold_message(f32::INFINITY), urgent);
        assert_eq!(threshold_message(f32::NEG_INFINITY), urgent);
    }

    #[test]
    fn messages_agree_with_threshold_percentages() {
        // The percentages are spelled out in the message strings; editing a
        // threshold without its message would misstate when the warning
        // fires, and the table is dead code until #667 lands — this test is
        // what catches the drift.
        for (t, m) in THRESHOLDS {
            let pct = format!("{}%", (t * 100.0).round() as u8);
            assert!(
                m.contains(&pct),
                "{m:?} does not mention {pct} for threshold {t}"
            );
        }
    }

    #[test]
    fn selects_highest_reached_threshold() {
        assert_eq!(threshold_message(0.91), Some(THRESHOLDS[0].1));
        assert_eq!(threshold_message(0.9), Some(THRESHOLDS[0].1));
        assert_eq!(threshold_message(0.8), Some(THRESHOLDS[1].1));
        assert_eq!(threshold_message(0.7), Some(THRESHOLDS[2].1));
        assert_eq!(threshold_message(0.6), Some(THRESHOLDS[2].1));
        assert_eq!(threshold_message(0.5), None);
    }
}
