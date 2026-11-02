//! Telemetry seam (P1-2): two channels, independently switchable, behind
//! the [`TelemetrySink`] trait.
//!
//! - **Events channel** — low-volume product events (capacity decisions,
//!   interventions, persistence failures). Existing emission points mirror
//!   their events into the sink; telemetry is a consumer, never a new event
//!   vocabulary.
//! - **Session-log channel** — high-volume per-turn session summaries. Every
//!   provider must enforce [`SESSION_LOG_BYTE_BUDGET`]: once the budget is
//!   spent the record is dropped, never truncated mid-record, so a runaway
//!   session cannot push unbounded traffic at a collector.
//!
//! Providers: [`LocalJsonlSink`] (this module — local-only jsonl, gated by
//! the `telemetry` config flag and the workspace-trust `attach` discipline)
//! and the CLI's OTLP/HTTP exporter (`crates/cli/src/telemetry_otlp.rs` —
//! opt-in via `[telemetry_otlp]` in config.toml, default off, zero network
//! while off). Hosts fan out to several sinks with [`TelemetryFanout`].
//!
//! ## Trust-timed attach
//!
//! Sinks are constructed *pre-trust*: [`LocalJsonlSink::new_skeleton`] (and
//! the OTLP exporter's constructor) register an in-memory queue but emit
//! nothing. Only after the workspace trust check passes does the host call
//! [`TelemetrySink::attach`], which flips the sink live and drains the
//! queue. An untrusted workspace never attaches, so nothing is ever emitted
//! — locally or over the network.
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
//!
//! # Known limitations
//!
//! - The session-log channel's producer mirrors only the normal turn-completion
//!   harvest (`engine/mod.rs`, post-executor `take_usage`); early-failure
//!   `TurnComplete`s that never ran the executor emit no session-log record
//!   (capacity events still capture those failures).
//! - No batching, retry, or backoff: a failed OTLP export is dropped, not
//!   retried — telemetry must never block or break the engine.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Total byte budget every provider enforces on the session-log channel.
/// Records beyond it are dropped whole — the channel is capped by contract,
/// not by hope.
pub const SESSION_LOG_BYTE_BUDGET: u64 = 256 * 1024;

/// The telemetry seam (P1-2): where product events and byte-capped session
/// summaries go. Definition lives here (framework); providers are
/// [`LocalJsonlSink`] (local jsonl) and the CLI's OTLP/HTTP exporter;
/// consumers are the engine's event-emission mirror sites. All methods must
/// be non-blocking and must never fail loudly — telemetry must not break the
/// engine (providers swallow IO/export errors internally).
pub trait TelemetrySink: Send + Sync + std::fmt::Debug {
    /// One product event (low volume): capacity decisions and kin. `Value`
    /// is the mirrored form of an existing event — no new vocabulary.
    fn emit_event(&self, event: serde_json::Value);
    /// One session-log record (high volume, byte-capped to
    /// [`SESSION_LOG_BYTE_BUDGET`] by the provider). Beyond the budget the
    /// provider drops records silently.
    fn emit_session_log(&self, record: serde_json::Value);
    /// Whether the events channel is on. Hosts consult this to skip
    /// serializing events nobody will take.
    fn events_enabled(&self) -> bool;
    /// Whether the session-log channel is on.
    fn session_log_enabled(&self) -> bool;
    /// Post-trust activation. Providers that gate on workspace trust
    /// override this; the default is a no-op (always-live providers).
    fn attach(&self) {}
}

/// Fan several sinks onto one trait object — the host wires local jsonl +
/// OTLP behind a single `Arc<dyn TelemetrySink>`.
#[derive(Debug, Clone)]
pub struct TelemetryFanout(pub Vec<Arc<dyn TelemetrySink>>);

impl TelemetrySink for TelemetryFanout {
    fn emit_event(&self, event: serde_json::Value) {
        for sink in &self.0 {
            sink.emit_event(event.clone());
        }
    }
    fn emit_session_log(&self, record: serde_json::Value) {
        for sink in &self.0 {
            sink.emit_session_log(record.clone());
        }
    }
    fn events_enabled(&self) -> bool {
        self.0.iter().any(|sink| sink.events_enabled())
    }
    fn session_log_enabled(&self) -> bool {
        self.0.iter().any(|sink| sink.session_log_enabled())
    }
    fn attach(&self) {
        for sink in &self.0 {
            sink.attach();
        }
    }
}

/// A local-only jsonl telemetry sink (the `telemetry = true` local mode).
///
/// Gated by the existing `telemetry: bool` config (`enabled`). The events
/// channel writes `~/.codesmith/telemetry/events.jsonl`; the session-log
/// channel writes a sibling `session-log.jsonl`, byte-capped to
/// [`SESSION_LOG_BYTE_BUDGET`]. Pre-`attach`, emissions enqueue in memory;
/// after `attach`, the queue drains to the files and new events are written
/// directly. IO errors are swallowed — the sink must never break the engine.
///
/// All fields are `Arc`-shared so cheap clones can be handed to long-lived
/// tasks (the engine, sub-agent runtimes).
#[derive(Clone, Debug)]
pub struct LocalJsonlSink {
    enabled: Arc<AtomicBool>,
    sink_path: Option<PathBuf>,
    session_log_path: Option<PathBuf>,
    session_log_bytes: Arc<AtomicU64>,
    queue: Arc<Mutex<VecDeque<serde_json::Value>>>,
    attached: Arc<AtomicBool>,
}

impl LocalJsonlSink {
    /// Pre-trust constructor: register the queue, do NOT emit. `enabled`
    /// mirrors the resolved `telemetry` config flag — when `false`, both
    /// channels drop events immediately (the sink is opt-in). `sink_path`
    /// is the events jsonl target; the session-log channel writes a sibling
    /// `session-log.jsonl`. `None` disables writing entirely (queue only,
    /// useful in tests or when the home dir is unavailable).
    #[must_use]
    pub fn new_skeleton(enabled: bool, sink_path: Option<PathBuf>) -> Self {
        let session_log_path = sink_path
            .as_ref()
            .map(|path| path.with_file_name("session-log.jsonl"));
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            sink_path,
            session_log_path,
            session_log_bytes: Arc::new(AtomicU64::new(0)),
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

    /// Whether [`attach`](TelemetrySink::attach) has been called
    /// (post-trust).
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.attached.load(Ordering::Relaxed)
    }
}

impl TelemetrySink for LocalJsonlSink {
    fn events_enabled(&self) -> bool {
        self.is_enabled() && self.sink_path.is_some()
    }

    fn session_log_enabled(&self) -> bool {
        // Same local flag gates both channels: the local sink is one config
        // (`telemetry = true`), not two.
        self.is_enabled() && self.session_log_path.is_some()
    }

    fn attach(&self) {
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

    fn emit_event(&self, event: serde_json::Value) {
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

    fn emit_session_log(&self, record: serde_json::Value) {
        if !self.session_log_enabled() || !self.is_attached() {
            return;
        }
        let Some(path) = self.session_log_path.as_ref() else {
            return;
        };
        // Byte-cap the channel: estimate the serialized size, claim it
        // atomically, drop whole records once the budget is spent.
        let Ok(line) = serde_json::to_string(&record) else {
            return;
        };
        let cost = line.len() as u64 + 1;
        let mut current = self.session_log_bytes.load(Ordering::Relaxed);
        loop {
            if current + cost > SESSION_LOG_BYTE_BUDGET {
                return; // over budget: drop the record, never truncate
            }
            match self.session_log_bytes.compare_exchange_weak(
                current,
                current + cost,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        let _ = write_event(path, &record);
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
    /// consumers (e.g. `format!("{action}")` in the REPL) render a value that
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
    /// `MAX_LEN` chars. The result is *not* guaranteed
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
        let sink = LocalJsonlSink::new_skeleton(true, Some(path.clone()));
        assert!(!sink.is_attached());
        sink.emit_event(json!({"e": 1}));
        sink.emit_event(json!({"e": 2}));
        // Not attached → nothing on disk.
        assert!(!path.exists(), "no file should exist before attach");
        // Both events queued.
        let q = sink.queue.lock().unwrap();
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn attach_drains_queue_to_jsonl() {
        let path = tmp_sink_path();
        let sink = LocalJsonlSink::new_skeleton(true, Some(path.clone()));
        sink.emit_event(json!({"i": 1}));
        sink.emit_event(json!({"i": 2}));
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
        let sink = LocalJsonlSink::new_skeleton(true, Some(path.clone()));
        sink.attach();
        sink.emit_event(json!({"i": 10}));
        sink.emit_event(json!({"i": 11}));
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
        let sink = LocalJsonlSink::new_skeleton(false, Some(path.clone()));
        sink.emit_event(json!({"x": 1}));
        sink.attach();
        sink.emit_event(json!({"x": 2}));
        assert!(!path.exists(), "disabled sink must not write");
        assert!(sink.queue.lock().unwrap().is_empty());
    }

    #[test]
    fn pathless_sink_queues_pre_attach_and_noops_post_attach() {
        let sink = LocalJsonlSink::new_skeleton(true, None);
        sink.emit_event(json!({"y": 1}));
        assert_eq!(sink.queue.lock().unwrap().len(), 1);
        sink.attach();
        // attach clears the queue even with no path (no unbounded growth).
        assert!(sink.queue.lock().unwrap().is_empty());
        sink.emit_event(json!({"y": 2}));
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
        let sink = LocalJsonlSink::new_skeleton(false, Some(path.clone()));
        sink.attach();
        // Disabled at construction: emit is a no-op (no file created).
        sink.emit_event(json!({"e": "dropped"}));
        assert!(!path.exists(), "disabled sink must not write");
        // Flip on: subsequent emit writes through.
        sink.set_enabled(true);
        sink.emit_event(json!({"e": "written"}));
        let contents = std::fs::read_to_string(&path).expect("file written after set_enabled");
        assert!(contents.contains("\"written\""), "got: {contents}");
    }

    #[test]
    fn session_log_channel_writes_a_capped_side_file() {
        let path = tmp_sink_path();
        let session_log = path.with_file_name("session-log.jsonl");
        let sink = LocalJsonlSink::new_skeleton(true, Some(path));
        sink.attach();
        assert!(sink.session_log_enabled());
        sink.emit_session_log(json!({"turn": 1, "n": "aaaa"}));
        sink.emit_session_log(json!({"turn": 2, "n": "bbbb"}));
        let contents = std::fs::read_to_string(&session_log).expect("side file written");
        assert_eq!(contents.lines().count(), 2);
        assert!(contents.contains("\"turn\":1"));

        // Byte budget: records past SESSION_LOG_BYTE_BUDGET are dropped
        // whole — the file never exceeds the budget by more than one record
        // already claimed under it.
        let big = "x".repeat(1024);
        let mut written = 2usize;
        for _ in 0..(SESSION_LOG_BYTE_BUDGET / 1024 + 8) {
            let before = sink.session_log_bytes.load(Ordering::Relaxed);
            sink.emit_session_log(json!({"n": big}));
            if sink.session_log_bytes.load(Ordering::Relaxed) > before {
                written += 1;
            }
        }
        let final_lines = std::fs::read_to_string(&session_log)
            .unwrap()
            .lines()
            .count();
        assert_eq!(final_lines, written, "claimed budget == written records");
        assert!(
            sink.session_log_bytes.load(Ordering::Relaxed) <= SESSION_LOG_BYTE_BUDGET,
            "budget is never exceeded"
        );
        assert!(final_lines < (SESSION_LOG_BYTE_BUDGET / 1024 + 16) as usize);
    }

    #[test]
    fn session_log_channel_follows_the_disabled_flag_and_pre_attach_gate() {
        let path = tmp_sink_path();
        let session_log = path.with_file_name("session-log.jsonl");
        // Disabled: no session-log writes even after attach.
        let sink = LocalJsonlSink::new_skeleton(false, Some(path.clone()));
        sink.attach();
        sink.emit_session_log(json!({"turn": 1}));
        assert!(!session_log.exists());

        // Enabled but pre-attach: dropped (the local session-log channel
        // does not queue — unlike events, these are high-volume records).
        let sink = LocalJsonlSink::new_skeleton(true, Some(path.clone()));
        sink.emit_session_log(json!({"turn": 1}));
        assert!(!session_log.exists());
    }

    #[test]
    fn fanout_routes_both_channels_to_every_sink() {
        let a = tmp_sink_path();
        let b_dir =
            std::env::temp_dir().join(format!("codesmith-telemetry-test-{}", uuid::Uuid::new_v4()));
        let b = b_dir.join("events.jsonl");
        let sink_a = Arc::new(LocalJsonlSink::new_skeleton(true, Some(a.clone())));
        let sink_b = Arc::new(LocalJsonlSink::new_skeleton(true, Some(b.clone())));
        let fanout = TelemetryFanout(vec![
            sink_a.clone() as Arc<dyn TelemetrySink>,
            sink_b.clone() as Arc<dyn TelemetrySink>,
        ]);
        assert!(fanout.events_enabled());
        fanout.attach();
        fanout.emit_event(json!({"e": 1}));
        fanout.emit_session_log(json!({"turn": 1}));
        for (sink, path) in [(sink_a, a), (sink_b, b)] {
            let events = std::fs::read_to_string(&path).unwrap();
            assert!(events.contains("\"e\":1"));
            let logs = std::fs::read_to_string(path.with_file_name("session-log.jsonl")).unwrap();
            assert!(logs.contains("\"turn\":1"));
            assert!(sink.is_attached(), "fanout attach reaches every sink");
        }
    }

    #[test]
    fn redact_strips_paths_quotes_and_truncates() {
        // Plan 06 / 6.3: redact() is the type barrier for runtime
        // diagnostics (replay_outcome, persist error) that may leak paths
        // or code into telemetry.
        let r = RedactedAnalyticsMetadata::redact(
            "replay failed at /Users/alice/secret.txt: \"unexpected token\" in `parser`",
        );
        let s = r.as_str();
        assert!(!s.contains("/Users/alice/secret.txt"), "path leaked: {s}");
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
            "replay failed at /Users/alice/secret.txt: \"unexpected token\" in `parser`",
        );
        assert_eq!(r, again);
        assert_eq!(r.as_str(), r.clone().as_str());
    }
}
