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

/// Filename of the verdict log inside the evolution directory.
pub const VERDICTS_FILE: &str = "verdicts.jsonl";

/// One recorded claim-check verdict. Field names match the injected
/// runtime-event vocabulary (`verified-pass` / `verified-fail` /
/// `verify-error` / `unsubstantiated`) so the log and the conversation
/// agree on what happened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerdictRecord {
    /// RFC-3339 UTC timestamp of the check.
    pub ts: String,
    pub verdict: String,
    pub failure_type: Option<String>,
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
pub fn record_verdict(
    model: &str,
    verdict: &str,
    failure_type: Option<&str>,
    claim: &str,
    command: Option<&str>,
    exit_code: Option<i64>,
) {
    let Some(path) = verdicts_path() else {
        return;
    };
    let record = VerdictRecord {
        ts: chrono::Utc::now().to_rfc3339(),
        verdict: verdict.to_string(),
        failure_type: failure_type.map(str::to_string),
        claim: claim.to_string(),
        command: command.map(str::to_string),
        exit_code,
        model: model.to_string(),
    };
    if let Err(err) = append_record(&path, &record) {
        tracing::warn!("evolution log append failed at {}: {err}", path.display());
    }
}

/// Append one record as a jsonl line. Directory is created lazily.
pub fn append_record(path: &Path, record: &VerdictRecord) -> std::io::Result<()> {
    let Ok(mut line) = serde_json::to_string(record) else {
        return Ok(());
    };
    line.push('\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())
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
        match record.verdict.as_str() {
            "verified-pass" => stats.verified_pass += 1,
            "verified-fail" => stats.verified_fail += 1,
            "unsubstantiated" => stats.unsubstantiated += 1,
            "verify-error" => stats.verify_error += 1,
            _ => {}
        }
        if let Some(failure_type) = &record.failure_type {
            *stats.failure_types.entry(failure_type.clone()).or_default() += 1;
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
                format!("{}%", n * 100 / self.total)
            }
        };
        let mut out = format!(
            "Claim-check verdicts (all time): {}\n\
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

    fn record(verdict: &str, failure_type: Option<&str>) -> VerdictRecord {
        VerdictRecord {
            ts: "2026-09-26T00:00:00+00:00".to_string(),
            verdict: verdict.to_string(),
            failure_type: failure_type.map(str::to_string),
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
        append_record(&path, &record("verified-pass", None)).unwrap();
        append_record(&path, &record("verified-fail", Some("test-failure"))).unwrap();
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
        assert_eq!(loaded[0].verdict, "verified-pass");
        assert_eq!(loaded[1].failure_type.as_deref(), Some("test-failure"));
    }

    #[test]
    fn load_missing_file_is_empty() {
        assert!(load_verdicts(Path::new("/nonexistent/verdicts.jsonl")).is_empty());
    }

    #[test]
    fn summarize_counts_and_match_rate() {
        let records = vec![
            record("verified-pass", None),
            record("verified-pass", None),
            record("verified-fail", Some("test-failure")),
            record("unsubstantiated", Some("unsubstantiated-claim")),
            record("verify-error", Some("verify-error")),
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
        let records = vec![record("unsubstantiated", Some("unsubstantiated-claim"))];
        assert!(summarize(&records).claim_match_rate().is_none());
    }

    #[test]
    fn render_carries_counts_rate_and_types() {
        let records = vec![
            record("verified-pass", None),
            record("verified-fail", Some("test-failure")),
        ];
        let text = summarize(&records).render();
        assert!(text.contains("Claim-check verdicts (all time): 2"));
        assert!(text.contains("verified-pass:   1 (50%)"));
        assert!(text.contains("claim-match rate"));
        assert!(text.contains("test-failure: 1"));
    }
}
