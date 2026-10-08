//! Evolution event log — P3-9 step 1, the observation layer.
//!
//! The continuous-evolution machinery (P3-8) now emits structured events:
//! claim-check verdicts today, doctor analyses and consolidation runs
//! next. Before any of that can be *evaluated* (P3-9's four evolution
//! metrics), it has to be observable — so every verdict is appended to a
//! local jsonl log, mirroring the audit-log posture: always-local,
//! best-effort, never allowed to break the engine that produced the
//! event. Recording rides the `[verification] result_claims` switch — the
//! feature's own gate is the log's gate; there is no separate opt-in
//! telemetry surface to configure.
//!
//! Layout: `<codesmith_home>/evolution/verdicts.jsonl`, one JSON object
//! per line (`ts` rfc3339 + the four-element verdict fields + the model
//! that made the claim). `CODESMITH_HOME` redirects it like every other
//! state path.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::events::{ResultFailureType, ResultVerdict};

/// Filename of the verdict log inside the evolution directory.
pub const VERDICTS_FILE: &str = "verdicts.jsonl";

/// Retention cap for the verdict log. The log is append-only with no natural
/// bound, while the only consumer (`/verify stats`) folds it for a readout —
/// so once the file exceeds this size, the next append rotates it down to the
/// trailing half-cap of records. Bounded I/O on long-lived installs, at the
/// cost of "all time" becoming "recent history" past the cap.
const MAX_VERDICTS_BYTES: u64 = 1024 * 1024;

/// One recorded claim-check verdict. Field vocabulary matches the injected
/// runtime-event message (`verified-pass` / `verified-fail` /
/// `verify-error` / `unsubstantiated` — the typed [`ResultVerdict`], whose
/// kebab-case serde form keeps existing jsonl lines loadable) so the log and
/// the conversation agree on what happened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerdictRecord {
    /// RFC-3339 UTC timestamp of the check.
    pub ts: String,
    pub verdict: ResultVerdict,
    pub failure_type: Option<ResultFailureType>,
    /// The matched claim phrase (e.g. "测试通过").
    pub claim: String,
    /// The re-run command, when one was found.
    pub command: Option<String>,
    pub exit_code: Option<i64>,
    /// The model that made the claim.
    pub model: String,
}

/// The evolution state directory (`<codesmith_home>/evolution`).
/// `None` when the home directory cannot be resolved — callers treat
/// that as "logging unavailable" and move on.
pub fn evolution_dir() -> Option<PathBuf> {
    codesmith_config::codesmith_home()
        .ok()
        .map(|home| home.join("evolution"))
}

/// Full path of the verdict log.
pub fn verdicts_path() -> Option<PathBuf> {
    evolution_dir().map(|dir| dir.join(VERDICTS_FILE))
}

/// Record one verdict to the default log path. Best-effort: resolution
/// or IO failure is logged at WARN and swallowed — the log must never
/// break the engine that produced the event (telemetry-sink precedent).
/// The append itself (sync fs I/O; rotation = full read + fsync + rename)
/// runs fire-and-forget on the blocking pool so an async worker is never
/// stalled (post-turn-snapshot precedent).
pub fn record_verdict(
    model: &str,
    verdict: ResultVerdict,
    failure_type: Option<ResultFailureType>,
    claim: &str,
    command: Option<&str>,
    exit_code: Option<i64>,
) {
    let Some(path) = verdicts_path() else {
        tracing::warn!("evolution log unavailable: codesmith home not resolved");
        return;
    };
    let record = VerdictRecord {
        ts: chrono::Utc::now().to_rfc3339(),
        verdict,
        failure_type,
        claim: claim.to_string(),
        command: command.map(str::to_string),
        exit_code,
        model: model.to_string(),
    };
    crate::utils::spawn_blocking_supervised("evolution-log-append", move || {
        if let Err(err) = append_record(&path, &record) {
            tracing::warn!("evolution log append failed at {}: {err}", path.display());
        }
    });
}

/// Append one record as a jsonl line. Directory is created lazily. The
/// size-check/rotate/append sequence holds a sidecar fd-lock so a
/// concurrent appender (overlapping verifier tasks, another CodeSmith
/// process sharing the home) cannot lose records to the rotation rename —
/// the lock cannot live on the log itself because `write_atomic` replaces
/// its inode. A lock failure is an IO failure like any other: the caller
/// warns and the record is dropped, never the engine.
pub fn append_record(path: &Path, record: &VerdictRecord) -> std::io::Result<()> {
    let Ok(mut line) = serde_json::to_string(record) else {
        // The record would vanish with no trace while the caller's WARN
        // posture claims failures are logged — keep that honest.
        tracing::warn!("evolution log record serialization failed; record dropped");
        return Ok(());
    };
    line.push('\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.with_extension("lock"))?;
    let mut lock = fd_lock::RwLock::new(lock_file);
    let _guard = lock
        .write()
        .map_err(|err| std::io::Error::other(format!("evolution log lock failed: {err}")))?;
    rotate_if_oversized(path, MAX_VERDICTS_BYTES);
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())
}

/// Size-based retention: when the log exceeds `max_bytes`, rewrite it
/// keeping only the trailing records that fit in half the cap. Best-effort —
/// a rotation failure is logged and must not block the append.
fn rotate_if_oversized(path: &Path, max_bytes: u64) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() <= max_bytes {
        return;
    }
    let records = load_verdicts(path);
    let keep_bytes = max_bytes / 2;
    let line_len =
        |record: &VerdictRecord| serde_json::to_string(record).map(|s| s.len() as u64 + 1);
    let mut retained: Vec<&VerdictRecord> = Vec::new();
    let mut retained_bytes = 0u64;
    for record in records.iter().rev() {
        let Ok(len) = line_len(record) else {
            continue;
        };
        if retained_bytes + len > keep_bytes && !retained.is_empty() {
            break;
        }
        retained_bytes += len;
        retained.push(record);
    }
    let mut out = String::new();
    for record in retained.into_iter().rev() {
        if let Ok(line) = serde_json::to_string(record) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    if let Err(err) = crate::utils::write_atomic(path, out.as_bytes()) {
        tracing::warn!("evolution log rotation failed at {}: {err}", path.display());
    }
}

/// Load records from a jsonl file, skipping malformed lines (a truncated
/// tail line from a crash mid-append must not hide the rest).
pub fn load_verdicts(path: &Path) -> Vec<VerdictRecord> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Aggregated stats over the verdict log — the first evaluation surface
/// for the evolution machinery (claim-match rate is the number that
/// matters: of the claims that had a command behind them, how many
/// survived the re-run).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VerdictStats {
    pub total: usize,
    pub verified_pass: usize,
    pub verified_fail: usize,
    pub unsubstantiated: usize,
    pub verify_error: usize,
    /// Failure-type histogram (`test-failure` / `build-failure` /
    /// `unsubstantiated-claim` / `verify-error`).
    pub failure_types: BTreeMap<String, usize>,
}

/// Count a single record into the stats.
pub fn summarize(records: &[VerdictRecord]) -> VerdictStats {
    let mut stats = VerdictStats::default();
    for record in records {
        stats.total += 1;
        match record.verdict {
            ResultVerdict::VerifiedPass => stats.verified_pass += 1,
            ResultVerdict::VerifiedFail => stats.verified_fail += 1,
            ResultVerdict::Unsubstantiated => stats.unsubstantiated += 1,
            ResultVerdict::VerifyError => stats.verify_error += 1,
        }
        if let Some(failure_type) = record.failure_type {
            *stats
                .failure_types
                .entry(failure_type.as_str().to_string())
                .or_default() += 1;
        }
    }
    stats
}

impl VerdictStats {
    /// Of the claims that had a verification command behind them, the
    /// fraction whose re-run matched the claim. `None` when nothing was
    /// checkable yet.
    pub fn claim_match_rate(&self) -> Option<f64> {
        let checked = self.verified_pass + self.verified_fail;
        if checked == 0 {
            return None;
        }
        Some(self.verified_pass as f64 / checked as f64)
    }

    /// Render the `/verify stats` readout.
    pub fn render(&self) -> String {
        let pct = |n: usize| -> String {
            if self.total == 0 {
                "0%".to_string()
            } else {
                // Round to nearest: truncation renders a nonzero row as 0%
                // (1 of 300) and the four rows sum below 100%.
                format!("{}%", (n * 100 + self.total / 2) / self.total)
            }
        };
        let mut out = format!(
            "Claim-check verdicts (recent history, 1 MiB log cap): {}\n\
             \x20 verified-pass:   {} ({})\n\
             \x20 verified-fail:   {} ({})\n\
             \x20 unsubstantiated: {} ({})\n\
             \x20 verify-error:    {} ({})",
            self.total,
            self.verified_pass,
            pct(self.verified_pass),
            self.verified_fail,
            pct(self.verified_fail),
            self.unsubstantiated,
            pct(self.unsubstantiated),
            self.verify_error,
            pct(self.verify_error),
        );
        if let Some(rate) = self.claim_match_rate() {
            out.push_str(&format!(
                "\n claim-match rate (of checkable claims): {:.0}%",
                rate * 100.0
            ));
        }
        if !self.failure_types.is_empty() {
            out.push_str("\n failure types:");
            for (failure_type, count) in &self.failure_types {
                out.push_str(&format!("\n   {failure_type}: {count}"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(verdict: ResultVerdict, failure_type: Option<ResultFailureType>) -> VerdictRecord {
        VerdictRecord {
            ts: "2026-09-26T00:00:00+00:00".to_string(),
            verdict,
            failure_type,
            claim: "测试通过".to_string(),
            command: Some("cargo test".to_string()),
            exit_code: Some(0),
            model: "mock-model".to_string(),
        }
    }

    #[test]
    fn append_and_load_round_trip_skips_bad_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("evolution").join(VERDICTS_FILE);
        append_record(&path, &record(ResultVerdict::VerifiedPass, None)).unwrap();
        append_record(
            &path,
            &record(
                ResultVerdict::VerifiedFail,
                Some(ResultFailureType::TestFailure),
            ),
        )
        .unwrap();
        // Simulate a crash-truncated tail.
        std::fs::write(
            &path,
            format!(
                "{}\n{{\"ts\": \"broken",
                std::fs::read_to_string(&path).unwrap()
            ),
        )
        .unwrap();
        let loaded = load_verdicts(&path);
        assert_eq!(loaded.len(), 2, "malformed tail must be skipped");
        assert_eq!(loaded[0].verdict, ResultVerdict::VerifiedPass);
        assert_eq!(loaded[1].failure_type, Some(ResultFailureType::TestFailure));
    }

    #[test]
    fn legacy_string_verdict_lines_still_load() {
        // jsonl lines written before the typed enum carry kebab-case strings;
        // the serde rename must keep them readable.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(VERDICTS_FILE);
        std::fs::write(
            &path,
            concat!(
                "{\"ts\":\"2026-09-26T00:00:00+00:00\",\"verdict\":\"verified-pass\",",
                "\"failure_type\":\"test-failure\",\"claim\":\"测试通过\",",
                "\"command\":\"cargo test\",\"exit_code\":1,\"model\":\"mock-model\"}\n"
            ),
        )
        .unwrap();
        let loaded = load_verdicts(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].verdict, ResultVerdict::VerifiedPass);
        assert_eq!(loaded[0].failure_type, Some(ResultFailureType::TestFailure));
    }

    #[test]
    fn load_missing_file_is_empty() {
        assert!(load_verdicts(Path::new("/nonexistent/verdicts.jsonl")).is_empty());
    }

    #[test]
    fn summarize_counts_and_match_rate() {
        let records = vec![
            record(ResultVerdict::VerifiedPass, None),
            record(ResultVerdict::VerifiedPass, None),
            record(
                ResultVerdict::VerifiedFail,
                Some(ResultFailureType::TestFailure),
            ),
            record(
                ResultVerdict::Unsubstantiated,
                Some(ResultFailureType::UnsubstantiatedClaim),
            ),
            record(
                ResultVerdict::VerifyError,
                Some(ResultFailureType::VerifyError),
            ),
        ];
        let stats = summarize(&records);
        assert_eq!(stats.total, 5);
        assert_eq!(stats.verified_pass, 2);
        assert_eq!(stats.verified_fail, 1);
        assert_eq!(stats.unsubstantiated, 1);
        assert_eq!(stats.verify_error, 1);
        assert_eq!(stats.failure_types.get("test-failure"), Some(&1));
        let rate = stats.claim_match_rate().expect("checkable claims exist");
        assert!((rate - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn match_rate_none_without_checkable_claims() {
        let records = vec![record(
            ResultVerdict::Unsubstantiated,
            Some(ResultFailureType::UnsubstantiatedClaim),
        )];
        assert!(summarize(&records).claim_match_rate().is_none());
    }

    #[test]
    fn render_carries_counts_rate_and_types() {
        let records = vec![
            record(ResultVerdict::VerifiedPass, None),
            record(
                ResultVerdict::VerifiedFail,
                Some(ResultFailureType::TestFailure),
            ),
        ];
        let text = summarize(&records).render();
        assert!(text.contains("Claim-check verdicts (recent history, 1 MiB log cap): 2"));
        assert!(text.contains("verified-pass:   1 (50%)"));
        assert!(text.contains("claim-match rate"));
        assert!(text.contains("test-failure: 1"));
    }

    #[test]
    fn rotation_keeps_the_tail_and_drops_the_head() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(VERDICTS_FILE);
        for i in 0..8 {
            let mut rec = record(ResultVerdict::VerifiedPass, None);
            rec.claim = format!("claim-{i:03}");
            append_record(&path, &rec).unwrap();
        }
        // Cap below the current size so rotation triggers; the kept tail must
        // fit in half the cap.
        rotate_if_oversized(&path, 512);
        let loaded = load_verdicts(&path);
        assert!(!loaded.is_empty(), "tail survives rotation");
        let names: Vec<&str> = loaded.iter().map(|r| r.claim.as_str()).collect();
        let newest = names.last().copied();
        assert_eq!(newest, Some("claim-007"), "newest record is kept");
        assert!(names.len() < 8, "oldest records are dropped: {names:?}");
    }
}
