//! `/verify` — readouts for the result claim verifier (P3-8 / P3-9).
//!
//! The verifier's verdicts are appended to the local evolution log
//! (`~/.codesmith/evolution/verdicts.jsonl`, see
//! `codesmith_agent_runtime::evolution_log`). This command is the
//! observation surface: aggregate counts, the claim-match rate over
//! checkable claims, and the failure-type histogram.

use std::path::Path;

use super::CommandResult;
use crate::tui::app::App;

pub fn verify(_app: &mut App, arg: Option<&str>) -> CommandResult {
    let arg = arg.map(str::trim).filter(|s| !s.is_empty());
    match arg {
        None | Some("stats") => CommandResult::message(stats_text()),
        Some(other) => CommandResult::error(format!(
            "Unknown subcommand '{other}' — usage: /verify [stats]"
        )),
    }
}

/// Build the stats readout from the default evolution log path.
fn stats_text() -> String {
    stats_text_from(codesmith_agent_runtime::evolution_log::verdicts_path().as_deref())
}

/// `stats_text` over an explicit log path (tests inject a tempdir; the
/// default resolver is env-dependent and must not be perturbed from a
/// parallel test run).
fn stats_text_from(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return "claim-check log unavailable (home directory not resolvable)".to_string();
    };
    if !path.exists() {
        return "No claim-check verdicts recorded yet — they appear when a completed \
turn's final message claims \"tests pass / build succeeds\" and the verifier \
checks it (see [verification] result_claims in config.example.toml)."
            .to_string();
    }
    let records = codesmith_agent_runtime::evolution_log::load_verdicts(path);
    if records.is_empty() {
        return format!(
            "claim-check log at {} contains no readable records",
            crate::utils::display_path(path)
        );
    }
    format!(
        "{}\nlog: {}",
        codesmith_agent_runtime::evolution_log::summarize(&records).render(),
        crate::utils::display_path(path)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolvable_home_reports_unavailable() {
        let text = stats_text_from(None);
        assert!(text.contains("unavailable"), "got: {text}");
    }

    #[test]
    fn missing_log_reports_empty_state() {
        let tmp = tempfile::tempdir().unwrap();
        let text = stats_text_from(Some(&tmp.path().join("evolution").join("verdicts.jsonl")));
        assert!(
            text.contains("No claim-check verdicts recorded yet"),
            "got: {text}"
        );
    }

    #[test]
    fn recorded_verdicts_render_into_the_readout() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("evolution").join("verdicts.jsonl");
        codesmith_agent_runtime::evolution_log::append_record(
            &path,
            &codesmith_agent_runtime::evolution_log::VerdictRecord {
                ts: "2026-09-26T00:00:00+00:00".to_string(),
                verdict: codesmith_agent_runtime::events::ResultVerdict::VerifiedFail,
                failure_type: Some(codesmith_agent_runtime::events::ResultFailureType::TestFailure),
                claim: "测试通过".to_string(),
                command: Some("cargo test".to_string()),
                exit_code: Some(1),
                model: "mock-model".to_string(),
            },
        )
        .unwrap();
        let text = stats_text_from(Some(&path));
        assert!(
            text.contains("Claim-check verdicts (recent history, 1 MiB log cap): 1"),
            "got: {text}"
        );
        assert!(text.contains("verified-fail:   1 (100%)"));
        assert!(text.contains("test-failure: 1"));
        assert!(text.contains("log:"), "readout names the log path");
    }
}
