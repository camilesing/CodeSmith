//! Staleness helpers for memory surfacing.
//!
//! Adds age labels like "47 days ago" or "yesterday" to surfaced memories,
//! and generates freshness warnings when memories may be outdated.

use chrono::Utc;

/// Return the number of days since the file was last modified.
/// Returns 0 for future mtimes (clock skew / just-written files).
pub fn memory_age_days(mtime_ms: i64) -> u64 {
    let now_ms = Utc::now().timestamp_millis();
    let diff = now_ms - mtime_ms;
    if diff < 0 {
        return 0;
    }
    (diff / 86_400_000) as u64
}

/// Human-readable age label: "today", "yesterday", or "N days ago".
pub fn memory_age_label(mtime_ms: i64) -> String {
    let days = memory_age_days(mtime_ms);
    match days {
        0 => String::from("today"),
        1 => String::from("yesterday"),
        d => format!("{d} days ago"),
    }
}

/// Generate a freshness warning when a memory is older than 1 day.
/// Returns empty string for fresh memories — no warning needed.
pub fn memory_freshness_text(mtime_ms: i64) -> String {
    let days = memory_age_days(mtime_ms);
    if days <= 1 {
        return String::new();
    }
    format!(
        "This memory is {days} days old. Memories are point-in-time observations, \
         not live state — claims about code behavior or file:line citations may \
         be outdated. Verify against current code before asserting as fact."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_days_zero_for_now() {
        let now_ms = Utc::now().timestamp_millis();
        assert_eq!(memory_age_days(now_ms), 0);
    }

    #[test]
    fn age_days_zero_for_future() {
        let future_ms = Utc::now().timestamp_millis() + 100_000;
        assert_eq!(memory_age_days(future_ms), 0);
    }

    #[test]
    fn age_days_one_day() {
        let one_day_ago_ms = Utc::now().timestamp_millis() - 86_400_000;
        assert_eq!(memory_age_days(one_day_ago_ms), 1);
    }

    #[test]
    fn age_label_today() {
        let now_ms = Utc::now().timestamp_millis();
        assert_eq!(memory_age_label(now_ms), "today");
    }

    #[test]
    fn age_label_yesterday() {
        let one_day_ago_ms = Utc::now().timestamp_millis() - 86_400_000;
        assert_eq!(memory_age_label(one_day_ago_ms), "yesterday");
    }

    #[test]
    fn freshness_empty_for_today() {
        let now_ms = Utc::now().timestamp_millis();
        assert!(memory_freshness_text(now_ms).is_empty());
    }

    #[test]
    fn freshness_empty_for_yesterday() {
        let one_day_ago_ms = Utc::now().timestamp_millis() - 86_400_000;
        assert!(memory_freshness_text(one_day_ago_ms).is_empty());
    }

    #[test]
    fn freshness_warning_for_old_memory() {
        let seven_days_ago_ms = Utc::now().timestamp_millis() - 7 * 86_400_000;
        let text = memory_freshness_text(seven_days_ago_ms);
        assert!(text.contains("7 days old"));
        assert!(text.contains("Verify against current code"));
    }
}
