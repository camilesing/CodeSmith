//! Minimal trigger source — GitHub notifications (fast path in code, slow
//! path is the coordinator itself).
//!
//! The DSH super-agent pipeline adapted to what CodeSmith has: no inbound
//! webhooks (outbound only) and MCP is pull-only, so the source **polls**.
//! Two-level filtering, the prototype's split:
//!
//! - **Fast path** ([`admit_notifications`]) — pure code, no model: reason
//!   allow-list, optional repo allow-list, dedup by notification id against
//!   a persisted seen-set. High-noise/low-trigger reality means most
//!   notifications die here at zero cost.
//! - **Slow path** — the coordinator model itself: admitted candidates are
//!   composed into one card ([`candidate_card`]) delivered as a user-role
//!   message through the REPL's submit path, clearly framed as trigger
//!   input, not human text. The coordinator decides — attach to an open
//!   ticket (steer its worker) or ignore — and answers to the user.
//!
//! # Known limitations
//!
//! - Interactive REPL sessions only; runtime-API / desktop threads get no
//!   polling (the poller is spawned from `run_repl`).
//! - No LLM pre-assessment, no email/group-chat sources; GitHub
//!   notifications carry no actor, so cards do not name the human.
//! - Enabling the trigger is enabling action: there is no observe-only mode
//!   (the config default `enabled = false` is the gate). Deliberate — the
//!   prototype's `observe` tier needs a trigger volume CodeSmith does not
//!   have yet.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Persisted seen-set cap: notifications older than this many entries roll
/// off (GitHub itself only serves the last 50 per page anyway).
const SEEN_RETAIN: usize = 500;
const CARD_TEXT_LIMIT: usize = 200;

/// Resolved `[coordinator.trigger]` runtime settings.
#[derive(Debug, Clone)]
pub struct TriggerSettings {
    pub interval_minutes: u64,
    /// Notification reasons the fast path admits (lowercase).
    pub reasons: Vec<String>,
    /// Optional `owner/name` allow-list; empty = every repo.
    pub repos: Vec<String>,
}

impl Default for TriggerSettings {
    fn default() -> Self {
        Self {
            interval_minutes: 15,
            reasons: vec!["mention".to_string(), "review_requested".to_string()],
            repos: Vec::new(),
        }
    }
}

impl TriggerSettings {
    /// Clamp the configured interval (minutes) to 1..=1440.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.interval_minutes = self.interval_minutes.clamp(1, 1440);
        self.reasons = self
            .reasons
            .iter()
            .map(|reason| reason.trim().to_lowercase())
            .filter(|reason| !reason.is_empty())
            .collect();
        if self.reasons.is_empty() {
            self.reasons = TriggerSettings::default().reasons;
        }
        self.repos = self
            .repos
            .iter()
            .map(|repo| repo.trim().to_lowercase())
            .filter(|repo| !repo.is_empty())
            .collect();
        self
    }
}

/// One admitted notification, already deduplicated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedNotification {
    pub id: String,
    pub repo: String,
    pub kind: String,
    pub title: String,
    pub reason: String,
    pub url: String,
}

/// Why one raw notification was dropped by the fast path (for counters, not
/// for the model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    ReasonNotAllowed,
    RepoNotAllowed,
    AlreadySeen,
    Malformed,
}

/// Fast path: filter one page of `gh api notifications` JSON against the
/// settings and the seen-set, returning the admitted candidates and the
/// updated seen ids. Pure function — the poller tests this, not `gh`.
#[must_use]
pub fn admit_notifications(
    page: &serde_json::Value,
    settings: &TriggerSettings,
    seen: &mut std::collections::HashSet<String>,
) -> (Vec<AdmittedNotification>, Vec<(String, DropReason)>) {
    let mut admitted = Vec::new();
    let mut dropped = Vec::new();
    let Some(items) = page.as_array() else {
        return (admitted, dropped);
    };
    for item in items {
        let Some(id) = item.get("id").and_then(|v| v.as_str()).map(str::to_string) else {
            dropped.push((String::new(), DropReason::Malformed));
            continue;
        };
        let reason = item
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_lowercase();
        let repo_full = item
            .pointer("/repository/full_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_lowercase();
        let title = item
            .pointer("/subject/title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let kind = item
            .pointer("/subject/type")
            .and_then(|v| v.as_str())
            .unwrap_or("Issue")
            .to_string();
        let url = item
            .get("html_url")
            .or_else(|| item.pointer("/subject/url"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let label = format!("{}:{repo_full}:{reason}", id);
        if !settings.reasons.iter().any(|allowed| allowed == &reason) {
            dropped.push((label, DropReason::ReasonNotAllowed));
            continue;
        }
        if !settings.repos.is_empty() && !settings.repos.iter().any(|allowed| allowed == &repo_full)
        {
            dropped.push((label, DropReason::RepoNotAllowed));
            continue;
        }
        if seen.contains(&label) {
            dropped.push((label, DropReason::AlreadySeen));
            continue;
        }
        seen.insert(label);
        admitted.push(AdmittedNotification {
            id,
            repo: repo_full,
            kind,
            title,
            reason,
            url,
        });
    }
    (admitted, dropped)
}

/// The slow path's input: one candidate card for a poll cycle's admitted
/// notifications, framed so the model knows it is trigger input, not human
/// text. The board check rides along — the first routing decision is attach
/// vs ignore, and that needs the open tickets.
#[must_use]
pub fn candidate_card(admitted: &[AdmittedNotification]) -> String {
    let mut lines = String::from(
        "[github trigger] candidate event(s) admitted by the notification filter. \
         This is trigger input, not a message from the user. For each candidate decide: \
         attach to an open ticket (steer its worker via agent_eval's message parameter), \
         open new work, or ignore (say nothing to the user). Check ticket_list first.\n",
    );
    for notification in admitted {
        let title = if notification.title.chars().count() <= CARD_TEXT_LIMIT {
            notification.title.clone()
        } else {
            let mut capped: String = notification.title.chars().take(CARD_TEXT_LIMIT).collect();
            capped.push('…');
            capped
        };
        lines.push_str(&format!(
            "- {} {} in {} (reason: {}): {} — {}",
            notification.kind,
            notification.id,
            notification.repo,
            notification.reason,
            title,
            notification.url
        ));
        if !lines.is_empty() {
            lines.push('\n');
        }
    }
    lines
}

/// Persisted seen-set for dedup, `<workspace>/.codesmith/state/triggers.v1.json`.
#[derive(Debug, Default)]
pub struct SeenNotifications {
    seen: std::collections::HashSet<String>,
    path: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedSeen {
    schema_version: u32,
    seen: Vec<String>,
}

impl SeenNotifications {
    #[must_use]
    pub fn for_workspace(workspace: &std::path::Path) -> Self {
        Self {
            seen: Self::load(workspace),
            path: Some(
                workspace
                    .join(".codesmith")
                    .join("state")
                    .join("triggers.v1.json"),
            ),
        }
    }

    fn load(workspace: &std::path::Path) -> std::collections::HashSet<String> {
        let path = workspace
            .join(".codesmith")
            .join("state")
            .join("triggers.v1.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return std::collections::HashSet::new();
        };
        serde_json::from_str::<PersistedSeen>(&text)
            .ok()
            .filter(|state| state.schema_version == 1)
            .map(|state| state.seen.into_iter().collect())
            .unwrap_or_default()
    }

    /// The mutable seen-set the fast-path filter reads and updates.
    pub fn set_mut(&mut self) -> &mut std::collections::HashSet<String> {
        &mut self.seen
    }

    /// Best-effort persist with a bounded retain (oldest entries roll off —
    /// insertion order is lost to the set, so the cap is approximate, which
    /// is fine: a rolled-off id at worst re-admits one stale notification).
    pub fn persist(&self) {
        let Some(path) = &self.path else { return };
        if self.seen.len() > SEEN_RETAIN {
            let retained: Vec<String> = self.seen.iter().take(SEEN_RETAIN).cloned().collect();
            let state = PersistedSeen {
                schema_version: 1,
                seen: retained,
            };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, serde_json::to_string(&state).unwrap_or_default());
            return;
        }
        let state = PersistedSeen {
            schema_version: 1,
            seen: self.seen.iter().cloned().collect(),
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, serde_json::to_string(&state).unwrap_or_default());
    }
}

/// Run `gh api notifications` and return the parsed JSON page. Shared
/// `gh`-invocation stance with `tools/github.rs` (env override, PATH
/// resolve, loud failure) but standalone: the poller runs outside a tool
/// context, off the runtime threads via `spawn_blocking` at the caller.
pub async fn poll_github_notifications(
    workspace: &std::path::Path,
) -> anyhow::Result<serde_json::Value> {
    let bin = codesmith_config::codesmith_env("GH_BIN").unwrap_or_else(|| {
        use codesmith_agent_runtime::dependencies::ExternalTool as _;
        codesmith_agent_runtime::dependencies::Gh::resolve().unwrap_or_else(|| "gh".to_string())
    });
    let workspace = workspace.to_path_buf();
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&bin)
            .args(["api", "notifications", "--paginate"])
            .current_dir(&workspace)
            .output()
    })
    .await
    .map_err(|err| anyhow::anyhow!("gh poll worker failed: {err}"))?
    .map_err(|err| anyhow::anyhow!("failed to run gh: {err}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "gh api notifications failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|err| anyhow::anyhow!("failed to parse gh notifications: {err}"))
}

/// The poll task: on every interval tick — but only while the host reports
/// the session is in coordinator mode — poll, fast-path filter, persist the
/// seen-set, and hand the candidate card to the submit callback. The
/// mode gate runs BEFORE polling so notifications that arrive outside
/// coordinator mode stay unseen and are still admitted by a later poll.
/// Stops when the cancel token fires.
pub async fn run_trigger_poller(
    workspace: std::path::PathBuf,
    settings: TriggerSettings,
    is_coordinator: Box<dyn Fn() -> bool + Send>,
    mut submit: Box<dyn FnMut(String) + Send>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut seen = SeenNotifications::for_workspace(&workspace);
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
        settings.interval_minutes.saturating_mul(60),
    ));
    // The first tick fires immediately; a fresh session should not hammer
    // GitHub before the user has even settled — skip it.
    interval.tick().await;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = interval.tick() => {}
        }
        if !is_coordinator() {
            continue;
        }
        match poll_github_notifications(&workspace).await {
            Ok(page) => {
                let (admitted, _dropped) = admit_notifications(&page, &settings, seen.set_mut());
                seen.persist();
                if admitted.is_empty() {
                    continue;
                }
                submit(candidate_card(&admitted));
            }
            Err(err) => {
                tracing::warn!("coordinator trigger poll failed: {err:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_item(id: &str, reason: &str, repo: &str, title: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "reason": reason,
            "repository": { "full_name": repo },
            "subject": { "title": title, "type": "PullRequest" },
            "html_url": format!("https://github.com/{repo}/pull/1")
        })
    }

    #[test]
    fn fast_path_admits_allowed_reason_and_dedups() {
        let settings = TriggerSettings::default();
        let page = serde_json::json!([
            page_item("1", "mention", "org/repo", "Ping"),
            page_item("2", "ci_activity", "org/repo", "Build done"),
            page_item("3", "mention", "other/org", "Elsewhere"),
        ]);
        let mut seen = std::collections::HashSet::new();
        let (admitted, dropped) = admit_notifications(&page, &settings, &mut seen);
        // default settings: no repo filter, so both mentions admit; only the
        // ci_activity reason drops
        assert_eq!(admitted.len(), 2);
        assert_eq!(dropped.len(), 1);
        assert!(
            dropped
                .iter()
                .any(|(_, reason)| *reason == DropReason::ReasonNotAllowed)
        );

        // second poll: same ids are already seen
        let (admitted, dropped) = admit_notifications(&page, &settings, &mut seen);
        assert!(admitted.is_empty());
        assert_eq!(dropped.len(), 3);
        assert!(
            dropped
                .iter()
                .any(|(_, reason)| *reason == DropReason::AlreadySeen)
        );
    }

    #[test]
    fn repo_allowlist_filters() {
        let settings = TriggerSettings {
            repos: vec!["org/repo".to_string()],
            ..TriggerSettings::default()
        };
        let page = serde_json::json!([
            page_item("1", "mention", "org/repo", "Keep"),
            page_item("2", "mention", "other/org", "Drop"),
        ]);
        let (admitted, dropped) =
            admit_notifications(&page, &settings, &mut std::collections::HashSet::new());
        assert_eq!(admitted.len(), 1);
        assert!(
            dropped
                .iter()
                .any(|(_, reason)| *reason == DropReason::RepoNotAllowed)
        );
    }

    #[test]
    fn card_frames_as_trigger_input_and_caps_titles() {
        let admitted = vec![AdmittedNotification {
            id: "1".to_string(),
            repo: "org/repo".to_string(),
            kind: "PullRequest".to_string(),
            title: "x".repeat(500),
            reason: "mention".to_string(),
            url: "https://github.com/org/repo/pull/1".to_string(),
        }];
        let card = candidate_card(&admitted);
        assert!(card.contains("[github trigger]"));
        assert!(card.contains("not a message from the user"));
        assert!(card.contains("ticket_list"));
        assert!(card.contains('…'));
    }
}
