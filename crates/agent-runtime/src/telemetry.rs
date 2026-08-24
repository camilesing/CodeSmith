//! Local-only telemetry scaffolding (Plan 05 / findings 2, 5a, 5c).
//!
//! CodeSmith ships **no networked telemetry** by design — every outbound
//! request targets an LLM provider, MCP server, web-search provider,
//! localhost runtime, sandbox, hook webhook, or OAuth endpoint. This module
//! provides the defensive scaffolding that would protect a real sink, plus a
//! minimal **local-only** jsonl sink so the gating, type barrier, and
//! ephemeral id have something concrete to protect.
//!
//! ## Trust-timed attach
//!
//! The sink is constructed *pre-trust* via [`TelemetrySink::new_skeleton`]:
//! it registers the in-memory queue but writes nothing. Only after the
//! workspace trust check passes does the host call [`TelemetrySink::attach`],
//! which flips `attached` and drains the queue to the jsonl file. An
//! untrusted workspace never attaches, so no jsonl is ever written.
//!
//! ## Type barrier
//!
//! [`VerifiedAnalyticsMetadata`] mirrors Claude Code's
//! `AnalyticsMetadata_I_VERIFIED_THIS_IS_NOT_CODE_OR_FILEPATHS`: a newtype
//! over `String` with **no `From<String>`/`From<&str>` impl**, so callers
//! must construct it consciously via [`VerifiedAnalyticsMetadata::verified`].
//! Every string field assembled into a sink event should be a
//! `VerifiedAnalyticsMetadata` so a stray file path or code snippet cannot
//! leak into telemetry without an explicit assertion that it is neither.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A local-only jsonl telemetry sink.
///
/// Gated by the existing `telemetry: bool` config (`enabled`). The sink path
/// is `~/.codesmith/telemetry/events.jsonl` (resolved by the host; never
/// networked). Pre-`attach`, [` Self::emit`] enqueues events in memory; after
/// `attach`, the queue is drained to the file and new events are written
/// directly. IO errors are swallowed — the sink must never break the engine.
///
/// All fields are `Arc`-shared so cheap clones can be handed to long-lived
/// tasks (the engine, sub-agent runtimes).
#[derive(Clone, Debug)]
pub struct TelemetrySink {
    enabled: Arc<AtomicBool>,
    sink_path: Option<PathBuf>,
    queue: Arc<Mutex<VecDeque<serde_json::Value>>>,
    attached: Arc<AtomicBool>,
}

impl TelemetrySink {
    /// Pre-trust constructor: register the queue, do NOT emit. `enabled`
    /// mirrors the resolved `telemetry` config flag — when `false`, [`emit`]
    /// drops events immediately (the sink is opt-in). `sink_path` is the jsonl
    /// target; `None` disables writing entirely (queue only, useful in tests
    /// or when the home dir is unavailable).
    ///
    /// [`emit`]: Self::emit
    #[must_use]
    pub fn new_skeleton(enabled: bool, sink_path: Option<PathBuf>) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            sink_path,
            queue: Arc::new(Mutex::new(VecDeque::new())),
            attached: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether the sink is enabled (the `telemetry` config flag).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Update the `enabled` flag at runtime (Plan 06 / 6.2). Called by the
    /// host after the project-config overlay is merged so the durable flag
    /// reflects the combined user + project `telemetry` setting. Because the
    /// field is `Arc`-shared, the engine's clone observes the flip
    /// immediately — no separate setter needs to reach the engine.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Whether [`attach`] has been called (post-trust).
    ///
    /// [`attach`]: Self::attach
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.attached.load(Ordering::Relaxed)
    }

    /// Post-trust: flip `attached` and drain the queue to the jsonl file.
    /// Safe to call once, after the workspace trust check passes. If the sink
    /// is disabled or has no path, this is a no-op (the queue is cleared so
    /// pre-trust events do not accumulate unbounded).
    pub fn attach(&self) {
        self.attached.store(true, Ordering::Relaxed);
        // Drain the queue regardless of enabled/path so pre-trust events do
        // not pile up forever when the sink is disabled or pathless.
        let drained: Vec<serde_json::Value> = {
            let mut q = match self.queue.lock() {
                Ok(q) => q,
                Err(_) => return,
            };
            q.drain(..).collect()
        };
        if !self.is_enabled() {
            return;
        }
        let Some(path) = self.sink_path.as_ref() else {
            return;
        };
        for event in drained {
            let _ = write_event(path, &event);
        }
    }

    /// Emit one event. Pre-`attach` → enqueue; post-`attach` → write directly
    /// (after first draining any queued events so ordering is preserved). When
    /// the sink is disabled, events are dropped.
    pub fn emit(&self, event: serde_json::Value) {
        if !self.is_enabled() {
            return;
        }
        if !self.is_attached() {
            if let Ok(mut q) = self.queue.lock() {
                q.push_back(event);
            }
            return;
        }
        // Post-attach: drain anything still queued (ordering) then write.
        let queued: Vec<serde_json::Value> = match self.queue.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => Vec::new(),
        };
        let Some(path) = self.sink_path.as_ref() else {
            return;
        };
        for e in queued {
            let _ = write_event(path, &e);
        }
        let _ = write_event(path, &event);
    }
}

/// Privacy type barrier mirroring Claude Code's
/// `AnalyticsMetadata_I_VERIFIED_THIS_IS_NOT_CODE_OR_FILEPATHS`.
///
/// A newtype over `String` with **no `From<String>`/`From<&str>` impl**: the
/// absence forces conscious construction via [`verified`](Self::verified), so
/// a stray file path, code snippet, or PII cannot slip into a telemetry event
/// without an explicit assertion at the call site.
///
/// ```
/// # use codesmith_agent_runtime::telemetry::VerifiedAnalyticsMetadata;
/// // There is no `From<&str>` impl, so this would NOT compile:
/// //   let v: VerifiedAnalyticsMetadata = "x".into();
/// // Construct consciously instead:
/// let v = VerifiedAnalyticsMetadata::verified("not-code-not-a-path");
/// assert_eq!(v.as_str(), "not-code-not-a-path");
/// ```
// NOTE: deliberately NO `impl From<String> for VerifiedAnalyticsMetadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAnalyticsMetadata(pub String);

impl VerifiedAnalyticsMetadata {
    /// Caller asserts this value is NOT source code, a file path, or PII.
    #[must_use]
    pub fn verified(s: &str) -> Self {
        Self(s.to_string())
    }

    /// Borrow the verified value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for VerifiedAnalyticsMetadata {
    /// Render the verified value. This does **not** weaken the construction
    /// barrier (there is still no `From<String>`): it only lets display
    /// consumers (e.g. `format!("{action}")` in the TUI) render a value that
    /// was already consciously constructed via [`verified`](Self::verified).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A string newtype whose value has been **redacted** before entering
/// telemetry: file-path-like substrings and quoted spans (which may carry
/// code or file content / PII) are replaced with placeholders and the
/// result is truncated to a fixed cap.
///
/// Like [`VerifiedAnalyticsMetadata`] it has **no `From<String>`/`From<&str>`**
/// impl, so it cannot be bypassed with a raw `String` — callers must
/// consciously route potentially-leaky runtime diagnostics (e.g. a capacity
/// replay outcome or a persistence-error string) through
/// [`redact`](Self::redact). Use [`VerifiedAnalyticsMetadata::verified`]
/// for values that can be statically verified as path/code-free; use this
/// type for everything that cannot.
// NOTE: deliberately NO `impl From<String> for RedactedAnalyticsMetadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedAnalyticsMetadata(pub String);

impl RedactedAnalyticsMetadata {
    /// Max length (in `char`s) of a redacted value before truncation. An
    /// ellipsis is appended when truncated, so the worst-case length is
    /// `MAX_LEN + 1` chars.
    const MAX_LEN: usize = 280;

    /// Best-effort sanitize `s` for telemetry: replace file-path-like
    /// substrings (unix absolute `/x`, home-relative `~x`, and windows
    /// drive `C:\x` paths) and quoted spans (`"..."`, `'...'`, `` `...` ``)
    /// — which may carry code/PII — with placeholders, then truncate to
    /// [`MAX_LEN`](Self::MAX_LEN) chars. The result is *not* guaranteed
    /// leak-free; this is a deliberate barrier against the common
    /// path/code-leak shapes, not a guarantee.
    #[must_use]
    pub fn redact(s: &str) -> Self {
        use std::sync::OnceLock;
        static PATH_RE: OnceLock<regex::Regex> = OnceLock::new();
        static QUOTE_RE: OnceLock<regex::Regex> = OnceLock::new();
        let path_re = PATH_RE.get_or_init(|| {
            // unix absolute, home-relative, and windows drive paths; stop at
            // whitespace, quotes, or angle brackets.
            regex::Regex::new(r#"(?:/[^\s"'<>`]+)|(?:~[^\s"'<>`]+)|(?:[A-Za-z]:\\[^\s"'<>`]+)"#)
                .expect("path regex is a compile-time constant")
        });
        let quote_re = QUOTE_RE.get_or_init(|| {
            // any double/single/backtick quoted span (may span newlines,
            // since negated char classes match `\n`).
            regex::Regex::new(r#""[^"]*"|'[^']*'|`[^`]*`"#)
                .expect("quote regex is a compile-time constant")
        });
        let scrubbed = path_re.replace_all(s, "<path>");
        let mut scrubbed: String = quote_re.replace_all(&scrubbed, "<redacted>").into_owned();
        if scrubbed.chars().count() > Self::MAX_LEN {
            scrubbed = scrubbed.chars().take(Self::MAX_LEN).collect();
            scrubbed.push('…');
        }
        Self(scrubbed)
    }

    /// Borrow the redacted value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RedactedAnalyticsMetadata {
    /// Render the redacted value. Like
    /// [`VerifiedAnalyticsMetadata`]'s `Display`, this does **not** weaken
    /// the construction barrier (there is still no `From<String>`): it only
    /// lets display consumers render a value already sanitized via
    /// [`redact`](Self::redact).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Append one event as a JSON line to the jsonl sink. Creates the parent
/// directory if needed. Returns early on any IO/serialization error — the sink
/// must never break the engine.
fn write_event(path: &Path, event: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(event).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp_sink_path() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("codesmith-telemetry-test-{}", uuid::Uuid::new_v4()));
        dir.join("events.jsonl")
    }

    #[test]
    fn pre_attach_events_are_queued_not_written() {
        let path = tmp_sink_path();
        let sink = TelemetrySink::new_skeleton(true, Some(path.clone()));
        assert!(!sink.is_attached());
        sink.emit(json!({"e": 1}));
        sink.emit(json!({"e": 2}));
        // Not attached → nothing on disk.
        assert!(!path.exists(), "no file should exist before attach");
        // Both events queued.
        let q = sink.queue.lock().unwrap();
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn attach_drains_queue_to_jsonl() {
        let path = tmp_sink_path();
        let sink = TelemetrySink::new_skeleton(true, Some(path.clone()));
        sink.emit(json!({"i": 1}));
        sink.emit(json!({"i": 2}));
        sink.attach();
        assert!(sink.is_attached());
        assert!(path.exists());
        let lines: Vec<String> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"i\":1"));
        assert!(lines[1].contains("\"i\":2"));
        // Queue drained.
        assert!(sink.queue.lock().unwrap().is_empty());
    }

    #[test]
    fn emit_after_attach_writes_directly() {
        let path = tmp_sink_path();
        let sink = TelemetrySink::new_skeleton(true, Some(path.clone()));
        sink.attach();
        sink.emit(json!({"i": 10}));
        sink.emit(json!({"i": 11}));
        let lines: Vec<String> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"i\":10"));
        assert!(lines[1].contains("\"i\":11"));
    }

    #[test]
    fn disabled_sink_drops_events() {
        let path = tmp_sink_path();
        let sink = TelemetrySink::new_skeleton(false, Some(path.clone()));
        sink.emit(json!({"x": 1}));
        sink.attach();
        sink.emit(json!({"x": 2}));
        assert!(!path.exists(), "disabled sink must not write");
        assert!(sink.queue.lock().unwrap().is_empty());
    }

    #[test]
    fn pathless_sink_queues_pre_attach_and_noops_post_attach() {
        let sink = TelemetrySink::new_skeleton(true, None);
        sink.emit(json!({"y": 1}));
        assert_eq!(sink.queue.lock().unwrap().len(), 1);
        sink.attach();
        // attach clears the queue even with no path (no unbounded growth).
        assert!(sink.queue.lock().unwrap().is_empty());
        sink.emit(json!({"y": 2}));
        // Post-attach with no path: dropped, not queued.
        assert!(sink.queue.lock().unwrap().is_empty());
    }

    #[test]
    fn verified_analytics_metadata_requires_conscious_construction() {
        let v = VerifiedAnalyticsMetadata::verified("aggregate-count");
        assert_eq!(v.as_str(), "aggregate-count");
        assert_eq!(v, VerifiedAnalyticsMetadata::verified("aggregate-count"));
        // The newtype is transparent over String but has no From<&str>.
        assert_eq!(v.0, "aggregate-count");
    }

    #[test]
    fn set_enabled_toggles_emission_post_attach() {
        // Plan 06 / 6.2: the host re-applies the merged `telemetry` flag via
        // `set_enabled`; the Arc-shared flag is what the engine clone reads.
        let path = tmp_sink_path();
        let sink = TelemetrySink::new_skeleton(false, Some(path.clone()));
        sink.attach();
        // Disabled at construction: emit is a no-op (no file created).
        sink.emit(json!({"e": "dropped"}));
        assert!(!path.exists(), "disabled sink must not write");
        // Flip on: subsequent emit writes through.
        sink.set_enabled(true);
        sink.emit(json!({"e": "written"}));
        let contents = std::fs::read_to_string(&path).expect("file written after set_enabled");
        assert!(contents.contains("\"written\""), "got: {contents}");
    }

    #[test]
    fn redact_strips_paths_quotes_and_truncates() {
        // Plan 06 / 6.3: redact() is the type barrier for runtime
        // diagnostics (replay_outcome, persist error) that may leak paths
        // or code into telemetry.
        let r = RedactedAnalyticsMetadata::redact(
            "replay failed at /Users/camile/secret.txt: \"unexpected token\" in `parser`",
        );
        let s = r.as_str();
        assert!(!s.contains("/Users/camile/secret.txt"), "path leaked: {s}");
        assert!(!s.contains("unexpected token"), "quoted span leaked: {s}");
        assert!(!s.contains("parser"), "backtick span leaked: {s}");
        assert!(s.contains("<path>"), "expected <path> placeholder: {s}");
        assert!(
            s.contains("<redacted>"),
            "expected <redacted> placeholder: {s}"
        );

        // Truncation cap (redact caps at MAX_LEN chars + ellipsis).
        let long = "a".repeat(600);
        let r2 = RedactedAnalyticsMetadata::redact(&long);
        assert!(
            r2.as_str().chars().count() <= RedactedAnalyticsMetadata::MAX_LEN + 1,
            "redact must truncate to <= MAX_LEN+1 chars"
        );

        // Deterministic + Clone round-trip.
        let again = RedactedAnalyticsMetadata::redact(
            "replay failed at /Users/camile/secret.txt: \"unexpected token\" in `parser`",
        );
        assert_eq!(r, again);
        assert_eq!(r.as_str(), r.clone().as_str());
    }
}
