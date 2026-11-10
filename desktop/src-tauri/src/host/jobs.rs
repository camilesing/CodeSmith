//! `job/list` · `job/follow` · `job/kill` — the vendored jobs popover,
//! bridged onto the runtime's background tasks (`/v1/tasks`).
//!
//! A CodeSmith task maps onto upstream's `JobView`: the task's prompt is the
//! label, `TaskStatus` onto the job lifecycle (`queued`/`running` → running,
//! `completed`, `failed`, `canceled` → killed), and the task's durable
//! timeline renders as the output ring — consecutive `message` entries
//! concatenate (they are one answer's deltas), every other kind renders one
//! `log` line. The render is deterministic over an append-only timeline, so
//! the ring's absolute byte offsets are stable across polls.
//!
//! Streams poll the runtime (2s for the roster, 1s while following): the
//! task's own thread already streams its events, but tailing that here would
//! duplicate the mapper's fold for a popover; a settled task's follow ends
//! with its `status` frame, exactly the anchor→output→status shape upstream
//! serves.
//!
//! Known limitations: a task with no linked thread (a foreign HTTP client's
//! task) belongs to no session, so no roster lists it; `progress` lines and
//! the byte `outputLimitBytes` cap have no runtime source and stay absent;
//! kill maps onto the runtime's cancel (a 60s grace interrupt), so a row may
//! read `running` briefly after its kill was `requested`.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::HostState;

/// One runtime task row (`GET /v1/tasks` / `GET /v1/tasks/{id}`).
async fn task(host: &HostState, id: &str) -> Result<Value> {
    host.backend
        .get(&format!("/v1/tasks/{id}"))
        .await
        .with_context(|| format!("task {id}"))
}

/// All runtime tasks; the roster filters by owning thread.
async fn tasks(host: &HostState) -> Vec<Value> {
    host.backend
        .get::<Vec<Value>>("/v1/tasks")
        .await
        .unwrap_or_default()
}

/// The task's first line, as the row's one-line label.
fn task_label(task: &Value) -> String {
    let prompt = task
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let line = prompt.lines().next().unwrap_or_default().trim();
    let mut label: String = line.chars().take(80).collect();
    if label.chars().count() == 80 {
        label.push('…');
    }
    label
}

/// `TaskStatus` onto the job lifecycle the client renders.
fn wire_status(status: &str) -> &'static str {
    match status {
        "completed" => "completed",
        "failed" => "failed",
        "canceled" | "cancelled" => "killed",
        // queued, running, and anything unknown read as live.
        _ => "running",
    }
}

/// The durable timeline as the job's output text: consecutive `message`
/// entries concatenate (one answer's deltas), every other kind is one line.
pub fn render_output(task: &Value) -> String {
    let mut out = String::new();
    let mut in_message = false;
    for entry in task
        .get("timeline")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = entry.get("kind").and_then(Value::as_str).unwrap_or_default();
        let summary = entry
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if kind == "message" {
            if !in_message && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(summary);
            in_message = true;
        } else {
            if in_message {
                out.push('\n');
                in_message = false;
            }
            if !summary.is_empty() {
                out.push_str(summary);
                out.push('\n');
            }
        }
    }
    if in_message {
        out.push('\n');
    }
    out
}

/// One runtime task as the client's `JobView`.
fn job_view(task: &Value) -> Value {
    let ms = |key: &str| {
        task.get(key)
            .and_then(Value::as_str)
            .and_then(super::mapper::epoch_ms)
            .unwrap_or(0)
    };
    let output = render_output(task);
    let mut started = ms("started_at");
    if started == 0 {
        started = ms("created_at");
    }
    let mut view = json!({
        "id": task.get("id").cloned().unwrap_or(Value::Null),
        "kind": "codesmith-task",
        "label": task_label(task),
        "status": wire_status(task.get("status").and_then(Value::as_str).unwrap_or("queued")),
        "startedAt": started,
        "output": { "total": output.len(), "earliest": 0 },
    });
    if let Some(thread) = task.get("thread_id").and_then(Value::as_str) {
        view["owner"] = json!(thread);
    }
    let finished = ms("ended_at");
    if finished > 0 {
        view["finishedAt"] = json!(finished);
    }
    // The terminal reason: the failure text, else the result summary.
    let detail = task
        .get("error")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| {
            task.get("result_summary")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        });
    if let Some(detail) = detail {
        view["detail"] = json!(detail);
    }
    view
}

/// The jobs remote's single `request` parameter: the gateway packs its args
/// as `{request: {…}}` (one object per wire parameter name), so every reader
/// here unwraps that member first — the shape `session/prompt` and friends
/// established.
fn inner_request(args: &Value) -> &Value {
    args.get("request").unwrap_or(args)
}

/// The byte offset `from` the client asked to resume at (default 0).
fn follow_from(request: &Value) -> usize {
    request
        .get("from")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(usize::MAX as u64) as usize
}

/// `job/list` — the session's task roster as whole-set `rows` frames, then a
/// 2s poll keeping the popover's rows current while it is open.
pub async fn list(
    host: &HostState,
    request: &Value,
    tx: &tokio::sync::mpsc::Sender<Value>,
) -> Result<()> {
    let request = inner_request(request);
    let session_id = request
        .get("sessionId")
        .or_else(|| request.get("session_id"))
        .and_then(Value::as_str)
        .context("job/list without sessionId")?;
    let rows = |all: &[Value]| {
        json!({
            "type": "rows",
            "jobs": all
                .iter()
                .filter(|task| task.get("thread_id").and_then(Value::as_str) == Some(session_id))
                .map(job_view)
                .collect::<Vec<_>>(),
        })
    };
    let mut last = Value::Null;
    loop {
        let all = tasks(host).await;
        let frame = rows(&all);
        if frame != last {
            if tx.send(frame.clone()).await.is_err() {
                return Ok(());
            }
            last = frame;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = tx.closed() => return Ok(()),
        }
    }
}

/// `job/follow` — the anchor (`opened`), then the output past `from` as it
/// grows (1s poll), ending with the terminal `status` frame.
pub async fn follow(
    host: &HostState,
    request: &Value,
    tx: &tokio::sync::mpsc::Sender<Value>,
) -> Result<(), Value> {
    let request = inner_request(request);
    let job_id = request
        .get("jobId")
        .or_else(|| request.get("job_id"))
        .and_then(Value::as_str)
        .context("job/follow without jobId")
        .map_err(|err| super::stream_failure("job/not-found", err.to_string()))?;
    let from = follow_from(request);
    let mut current = task(host, job_id)
        .await
        .map_err(|err| super::stream_failure("job/not-found", format!("{err:#}")))?;
    let mut served = from.min(render_output(&current).len());
    if tx.send(json!({ "type": "opened", "job": job_view(&current), "from": served })).await.is_err() {
        return Ok(());
    }
    loop {
        let status = current.get("status").and_then(Value::as_str).unwrap_or("");
        if !matches!(status, "queued" | "running" | "") {
            // Settled: the terminal status frame ends the stream, the shape
            // upstream's follow serves.
            let _ = tx
                .send(json!({ "type": "status", "job": job_view(&current) }))
                .await;
            return Ok(());
        }
        let output = render_output(&current);
        if output.len() > served {
            let text = output[served..].to_string();
            let at = served;
            served = output.len();
            let chunk = json!({ "at": at, "text": text, "channel": "log" });
            if tx
                .send(json!({ "type": "output", "chunks": [chunk], "next": served }))
                .await
                .is_err()
            {
                return Ok(());
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            _ = tx.closed() => return Ok(()),
        }
        current = match task(host, job_id).await {
            Ok(next) => next,
            // The task vanished (retention): end without a status frame —
            // the client's roster poll drops the row too.
            Err(_) => return Ok(()),
        };
    }
}

/// `job/kill` — the runtime's task cancel, in the registry's admission
/// vocabulary (`requested` / `already-finished`); an unknown job is the
/// client's `job/not-found` error.
pub async fn kill(host: &HostState, request: &Value) -> Value {
    let request = inner_request(request);
    let job_id = match request
        .get("jobId")
        .or_else(|| request.get("job_id"))
        .and_then(Value::as_str)
    {
        Some(id) => id.to_string(),
        None => {
            return super::rpc_error("gateway/arguments-invalid", "job/kill without jobId");
        }
    };
    let current = match task(host, &job_id).await {
        Ok(current) => current,
        Err(err) => return super::rpc_error("job/not-found", format!("{err:#}")),
    };
    if !matches!(
        current.get("status").and_then(Value::as_str),
        Some("queued") | Some("running")
    ) {
        return super::rpc_ok(json!({ "outcome": "already-finished" }));
    }
    match host
        .backend
        .post_empty(&format!("/v1/tasks/{job_id}/cancel"))
        .await
    {
        Ok(()) => super::rpc_ok(json!({ "outcome": "requested" })),
        Err(err) => super::rpc_error("gateway/internal", format!("job/kill: {err:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline_entry(kind: &str, summary: &str) -> Value {
        json!({ "timestamp": "2026-11-06T10:00:00Z", "kind": kind, "summary": summary })
    }

    #[test]
    fn output_rendering_concatenates_messages_and_lines_the_rest() {
        let task = json!({
            "id": "task_1",
            "prompt": "do the thing",
            "status": "completed",
            "thread_id": "thr_1",
            "created_at": "2026-11-06T10:00:00Z",
            "ended_at": "2026-11-06T10:01:00Z",
            "result_summary": "did it",
            "timeline": [
                timeline_entry("running", "Task started"),
                timeline_entry("message", "hello "),
                timeline_entry("message", "world"),
                timeline_entry("tool_started", "exec_shell started (ls)"),
                timeline_entry("tool_completed", "exec_shell: ok"),
            ],
        });
        assert_eq!(
            render_output(&task),
            "Task started\nhello world\nexec_shell started (ls)\nexec_shell: ok\n"
        );
        let view = job_view(&task);
        assert_eq!(view["kind"], "codesmith-task");
        assert_eq!(view["label"], "do the thing");
        assert_eq!(view["status"], "completed");
        assert_eq!(view["owner"], "thr_1");
        assert_eq!(view["detail"], "did it");
        assert_eq!(
            view["output"]["total"],
            json!("Task started\nhello world\nexec_shell started (ls)\nexec_shell: ok\n".len())
        );
    }

    #[test]
    fn statuses_map_and_labels_truncate() {
        for (runtime, wire) in [
            ("queued", "running"),
            ("running", "running"),
            ("completed", "completed"),
            ("failed", "failed"),
            ("canceled", "killed"),
        ] {
            let view = job_view(&json!({ "id": "t", "prompt": "p", "status": runtime }));
            assert_eq!(view["status"], wire, "{runtime}");
        }
        let long = "x".repeat(120);
        let view = job_view(&json!({ "id": "t", "prompt": long, "status": "running" }));
        let label = view["label"].as_str().unwrap();
        assert_eq!(label.chars().count(), 81, "80 chars plus the ellipsis");
        assert!(label.ends_with('…'));
        // A failure's error text is the terminal detail.
        let failed = job_view(&json!({
            "id": "t", "prompt": "p", "status": "failed", "error": "boom",
            "result_summary": "partial",
        }));
        assert_eq!(failed["detail"], "boom");
    }
}
