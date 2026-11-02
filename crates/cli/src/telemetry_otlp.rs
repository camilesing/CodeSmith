//! OTLP/HTTP telemetry exporter (P1-2) — the networked provider of the
//! `TelemetrySink` seam, wired from `[telemetry_otlp]` in config.toml.
//!
//! Opt-in and default-off: no `[telemetry_otlp]` table (or no `url`) means
//! this sink is never constructed, and a sink whose channels are both off
//! spawns no requests — zero network while off is a tested contract, not an
//! accident. Both channels POST OTLP/HTTP-JSON logs records
//! (`<url>/v1/logs`, `application/json`); the session-log channel is
//! byte-capped by the same [`SESSION_LOG_BYTE_BUDGET`] every provider
//! honours.
//!
//! Exports are fire-and-forget: `tokio::spawn`ed off the caller's runtime
//! when one exists (engine threads always have one), dropped when there is
//! no runtime or the POST fails — telemetry never blocks or breaks the
//! engine. Pre-`attach` (workspace trust not yet established) records queue
//! in memory, bounded; the queue is dropped, never flushed, if the sink is
//! never attached.
//!
//! # Anonymous id
//!
//! Records carry a stable anonymous id (`~/.codesmith/anonymous-id`, minted
//! once, a random UUID — not derived from anything about the user or
//! machine). It exists so a collector can group records from one install
//! without any identity: it is read **only** here, enters **only** the OTLP
//! resource attributes, and is never placed on a model request, a session
//! file, or the prompt. The ephemeral `telemetry_session_id` on engine
//! records stays per-session; the anonymous id spans sessions.
//!
//! # Known limitations
//!
//! - No batching, retry, or backoff; one record per POST. A collector
//!   outage drops records (never queues unbounded).
//! - No TLS or endpoint validation beyond what reqwest does by default;
//!   the URL is user config, treated as trusted input.
//! - The pre-attach queue holds at most [`PRE_ATTACH_QUEUE_LIMIT`] records;
//!   overflow drops oldest-first.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use codesmith_agent_runtime::telemetry::{SESSION_LOG_BYTE_BUDGET, TelemetrySink};

/// Records held while awaiting workspace trust; overflow drops oldest.
const PRE_ATTACH_QUEUE_LIMIT: usize = 256;

/// OTLP/HTTP-JSON logs exporter — one POST per record to `<url>/v1/logs`.
#[derive(Clone, Debug)]
pub struct OtlpHttpSink {
    url: String,
    client: reqwest::Client,
    events: bool,
    session_log: bool,
    attached: Arc<AtomicBool>,
    session_log_bytes: Arc<AtomicU64>,
    anonymous_id: Arc<str>,
    queue: Arc<Mutex<VecDeque<(Channel, serde_json::Value)>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channel {
    Events,
    SessionLog,
}

impl Channel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Events => "events",
            Self::SessionLog => "session_log",
        }
    }
}

impl OtlpHttpSink {
    /// Build the exporter. `url` is the collector base (e.g.
    /// `https://collector:4318`); `events` / `session_log` are the two
    /// channel switches from `[telemetry_otlp]`. Constructing the sink
    /// performs **no** network I/O; sending starts only after
    /// [`attach`](TelemetrySink::attach) (workspace trust).
    #[must_use]
    pub fn new(url: String, events: bool, session_log: bool) -> Self {
        Self {
            url,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            events,
            session_log,
            attached: Arc::new(AtomicBool::new(false)),
            session_log_bytes: Arc::new(AtomicU64::new(0)),
            anonymous_id: Arc::from(anonymous_id()),
            queue: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Whether the exporter has been attached (post-trust).
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.attached.load(Ordering::Relaxed)
    }

    fn channel_enabled(&self, channel: Channel) -> bool {
        match channel {
            Channel::Events => self.events,
            Channel::SessionLog => self.session_log,
        }
    }

    fn dispatch(&self, channel: Channel, record: serde_json::Value) {
        if !self.channel_enabled(channel) {
            return; // channel off: no serialization, no spawn, no network
        }
        if !self.is_attached() {
            // Pre-trust: bounded memory queue, oldest dropped on overflow.
            if let Ok(mut q) = self.queue.lock() {
                q.push_back((channel, record));
                while q.len() > PRE_ATTACH_QUEUE_LIMIT {
                    q.pop_front();
                }
            }
            return;
        }
        self.post(channel, record);
    }

    fn post(&self, channel: Channel, record: serde_json::Value) {
        let body = otlp_logs_body(&self.anonymous_id, channel, record);
        let url = format!("{}/v1/logs", self.url.trim_end_matches('/'));
        let client = self.client.clone();
        let send = async move {
            // Fire-and-forget: the response body is not read beyond the
            // status line; any failure is silently dropped.
            let _ = client.post(url).json(&body).send().await;
        };
        // Only spawn when a Tokio runtime exists (engine paths always do);
        // with no runtime the record is dropped rather than panicking.
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(send);
        }
    }
}

impl TelemetrySink for OtlpHttpSink {
    fn emit_event(&self, event: serde_json::Value) {
        self.dispatch(Channel::Events, event);
    }

    fn emit_session_log(&self, record: serde_json::Value) {
        if !self.session_log {
            return;
        }
        // Same byte-cap discipline as the local sink: claim the serialized
        // size atomically, drop whole records past the budget.
        let Ok(line) = serde_json::to_string(&record) else {
            return;
        };
        let cost = line.len() as u64 + 1;
        let mut current = self.session_log_bytes.load(Ordering::Relaxed);
        loop {
            if current + cost > SESSION_LOG_BYTE_BUDGET {
                return;
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
        self.dispatch(Channel::SessionLog, record);
    }

    fn events_enabled(&self) -> bool {
        self.events
    }

    fn session_log_enabled(&self) -> bool {
        self.session_log
    }

    fn attach(&self) {
        self.attached.store(true, Ordering::Relaxed);
        let drained: Vec<(Channel, serde_json::Value)> = match self.queue.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => return,
        };
        for (channel, record) in drained {
            self.post(channel, record);
        }
    }
}

/// One OTLP/HTTP-JSON `ExportLogsServiceRequest` carrying a single record.
fn otlp_logs_body(
    anonymous_id: &str,
    channel: Channel,
    record: serde_json::Value,
) -> serde_json::Value {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    serde_json::json!({
        "resourceLogs": [{
            "resource": {
                "attributes": [
                    { "key": "service.name", "value": { "stringValue": "codesmith" } },
                    { "key": "anonymous_id", "value": { "stringValue": anonymous_id } },
                    { "key": "channel", "value": { "stringValue": channel.as_str() } },
                ],
            },
            "scopeLogs": [{
                "scope": { "name": "codesmith-telemetry" },
                "logRecords": [{
                    "timeUnixNano": now_nanos.to_string(),
                    "severityText": "INFO",
                    "body": { "stringValue": serde_json::to_string(&record).unwrap_or_default() },
                }],
            }],
        }],
    })
}

/// The stable anonymous install id (`~/.codesmith/anonymous-id`): minted
/// once as a random UUID v4, read only by this module, carried only in OTLP
/// resource attributes. Never enters a model request, session file, or the
/// prompt.
fn anonymous_id() -> String {
    // `codesmith_home()` is already `~/.codesmith` (honours CODESMITH_HOME).
    let Ok(root) = codesmith_config::codesmith_home() else {
        return uuid::Uuid::new_v4().to_string();
    };
    let path = root.join("anonymous-id");
    match std::fs::read_to_string(&path) {
        Ok(existing) if !existing.trim().is_empty() => existing.trim().to_string(),
        _ => {
            let id = uuid::Uuid::new_v4().to_string();
            // Best-effort: an unwritable home falls back to a per-call id
            // (records still export; grouping degrades for this run).
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            let _ = std::fs::write(&path, &id);
            id
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::post;
    use std::sync::atomic::AtomicUsize;

    /// A local OTLP mock collector: counts POSTs, keeps the last body.
    async fn spawn_collector() -> (
        std::net::SocketAddr,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let count = Arc::new(AtomicUsize::new(0));
        let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let state = (count.clone(), bodies.clone());
        let app = Router::new().route(
            "/v1/logs",
            post(move |body: String| {
                let (count, bodies) = state.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    bodies.lock().unwrap().push(body);
                    axum::Json(serde_json::json!({}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, count, bodies)
    }

    async fn wait_for(count: &AtomicUsize, target: usize) {
        for _ in 0..100 {
            if count.load(Ordering::SeqCst) >= target {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    fn record(n: u64) -> serde_json::Value {
        serde_json::json!({ "type": "capacity_decision", "n": n })
    }

    #[tokio::test]
    async fn disabled_channels_make_zero_network_calls() {
        let (addr, count, _bodies) = spawn_collector().await;
        let sink = OtlpHttpSink::new(format!("http://{addr}"), false, false);
        sink.attach();
        sink.emit_event(record(1));
        sink.emit_session_log(record(2));
        // Give any (wrongly) spawned request time to land.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "both channels off must mean zero network calls"
        );
    }

    #[tokio::test]
    async fn pre_attach_records_never_hit_the_network() {
        let (addr, count, _bodies) = spawn_collector().await;
        let sink = OtlpHttpSink::new(format!("http://{addr}"), true, true);
        sink.emit_event(record(1));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "no export before the trust-time attach"
        );
        // Attach drains the queue to the collector.
        sink.attach();
        wait_for(&count, 1).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn enabled_exporter_posts_otlp_records_with_the_anonymous_id() {
        let (addr, count, bodies) = spawn_collector().await;
        let sink = OtlpHttpSink::new(format!("http://{addr}"), true, false);
        sink.attach();
        sink.emit_event(record(7));
        wait_for(&count, 1).await;

        let body = bodies.lock().unwrap()[0].clone();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let attrs = &parsed["resourceLogs"][0]["resource"]["attributes"];
        let id = attrs[1]["value"]["stringValue"].as_str().unwrap();
        assert!(!id.is_empty(), "anonymous id rides the resource attrs");
        assert_eq!(attrs[2]["value"]["stringValue"], "events");
        let body_str =
            parsed["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["body"]["stringValue"]
                .as_str()
                .unwrap();
        assert!(body_str.contains("capacity_decision"), "{body_str}");
        // The anonymous id never appears inside the record itself.
        assert!(!body_str.contains(id));
    }

    #[tokio::test]
    async fn session_log_channel_is_byte_capped() {
        let (addr, count, _bodies) = spawn_collector().await;
        let sink = OtlpHttpSink::new(format!("http://{addr}"), false, true);
        sink.attach();
        let big = "x".repeat(1024);
        let mut budgeted = 0usize;
        let mut claimed = 0u64;
        for _ in 0..(SESSION_LOG_BYTE_BUDGET / 1024 + 8) {
            let before = sink.session_log_bytes.load(Ordering::Relaxed);
            sink.emit_session_log(serde_json::json!({ "n": big }));
            let after = sink.session_log_bytes.load(Ordering::Relaxed);
            if after > before {
                budgeted += 1;
                claimed = after;
            }
        }
        assert!(
            claimed <= SESSION_LOG_BYTE_BUDGET,
            "budget never exceeded (claimed {claimed})"
        );
        // Fire-and-forget POSTs may lag; wait for every budgeted record.
        for _ in 0..500 {
            if count.load(Ordering::SeqCst) >= budgeted {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            budgeted,
            "one POST per budgeted record"
        );
        // An over-budget record adds nothing.
        sink.emit_session_log(serde_json::json!({ "n": big }));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            budgeted,
            "records past the budget are dropped before any POST"
        );
    }

    #[test]
    fn anonymous_id_is_stable_across_calls() {
        // No CODESMITH_HOME scoping here: the function reads the user home.
        // Stability is the property under test — two calls agree and are
        // UUID-shaped, whatever the first call minted or read.
        let a = anonymous_id();
        let b = anonymous_id();
        assert_eq!(a, b, "second call reads the same durable id");
        assert!(uuid::Uuid::parse_str(&a).is_ok(), "uuid-shaped: {a}");
    }
}
