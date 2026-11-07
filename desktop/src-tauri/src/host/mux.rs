//! mux: the client's stream carrier — one WebSocket per client at
//! `/api/remote.mux` multiplexing every logical stream, the protocol of the
//! vendored api-gateway `RemoteStreamMuxClient` (`stream-protocol.ts`).
//! Replaces the NDJSON `/api/stream/*` route, which held one HTTP connection
//! per open stream: Chromium caps a host at six connections and keeps an
//! unread streaming response open past page teardown, so reloads wedged the
//! whole pool.
//!
//! Client → bridge: `open {streamId, endpoint, payload}`, `item`, `end`,
//! `cancel`. Bridge → client: `item {streamId, value}`, `end`,
//! `error {streamId, error: {code, message, details}}`. A cancel, or the
//! socket closing, aborts the stream's task and sends nothing for it (as
//! upstream does); a malformed frame, a binary frame, or a duplicate stream
//! id closes the socket.
//!
//! Known limitations:
//! - no uplink: no bridged endpoint reads client items (upstream's only one,
//!   the sidebar terminal, is not simulated), so `item`/`end` are dropped;
//! - no ping heartbeat: the bridge is loopback-only, where a vanished page
//!   closes its TCP socket and that ends every stream the socket carried;
//! - unary calls stay HTTP POSTs on `/api/<endpoint>`, as upstream's do.

use std::collections::HashMap;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{SharedHost, PAGE_ORIGINS};

/// One browser-to-bridge frame (`RemoteStreamClientMessage`).
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientMessage {
    Open {
        #[serde(rename = "streamId")]
        stream_id: String,
        endpoint: String,
        payload: Value,
    },
    Cancel {
        #[serde(rename = "streamId")]
        stream_id: String,
    },
    /// Uplink item and half-close: no bridged endpoint reads uplink.
    Item {},
    End {},
}

/// WebSockets bypass CORS, so the upgrade checks the page origin itself —
/// otherwise any web page could open the mux and drive the bridge. An absent
/// Origin is a non-browser local client, which reaches the HTTP routes too.
fn origin_allowed(origin: Option<&HeaderValue>) -> bool {
    origin.is_none_or(|origin| {
        PAGE_ORIGINS
            .iter()
            .any(|allowed| origin.as_bytes() == allowed.as_bytes())
    })
}

/// `GET /api/remote.mux`: admit the page origin, then serve the socket.
pub async fn upgrade(
    State(host): State<SharedHost>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !origin_allowed(headers.get(header::ORIGIN)) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(move |socket| serve(host, socket))
}

fn text(frame: Value) -> Message {
    Message::Text(frame.to_string().into())
}

fn close(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}

/// Serve one client socket until it closes, then abort every stream it
/// still carries.
async fn serve(host: SharedHost, socket: WebSocket) {
    let (mut sink, mut source) = socket.split();
    // One writer owns the sink; stream tasks queue whole frames.
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });
    let mut streams: HashMap<String, JoinHandle<()>> = HashMap::new();
    while let Some(Ok(message)) = source.next().await {
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(_) => {
                let _ = out_tx.send(close(1003, "text messages required")).await;
                break;
            }
            Message::Close(_) => break,
            // The socket answers pings itself.
            Message::Ping(_) | Message::Pong(_) => continue,
        };
        match serde_json::from_str::<ClientMessage>(text.as_str()) {
            Ok(ClientMessage::Open {
                stream_id,
                endpoint,
                payload,
            }) if !stream_id.is_empty() && !endpoint.is_empty() => {
                streams.retain(|_, task| !task.is_finished());
                if streams.contains_key(&stream_id) {
                    let _ = out_tx.send(close(1008, "duplicate Remote stream id")).await;
                    break;
                }
                let task = tokio::spawn(pump(
                    host.clone(),
                    stream_id.clone(),
                    endpoint,
                    payload,
                    out_tx.clone(),
                ));
                streams.insert(stream_id, task);
            }
            Ok(ClientMessage::Cancel { stream_id }) => {
                if let Some(task) = streams.remove(&stream_id) {
                    task.abort();
                }
            }
            Ok(ClientMessage::Item {} | ClientMessage::End {}) => {}
            Ok(ClientMessage::Open { .. }) | Err(_) => {
                let _ = out_tx
                    .send(close(1008, "invalid Remote stream request"))
                    .await;
                break;
            }
        }
    }
    for task in streams.into_values() {
        task.abort();
    }
    // The writer drains (a queued close frame included) once the aborted
    // tasks have dropped their senders.
    drop(out_tx);
    let _ = writer.await;
}

/// Run one logical stream: each value its producer yields goes out as an
/// `item`; the producer's outcome becomes the `end` or `error` frame.
async fn pump(
    host: SharedHost,
    stream_id: String,
    endpoint: String,
    payload: Value,
    out: mpsc::Sender<Message>,
) {
    let (tx, mut rx) = mpsc::channel::<Value>(16);
    let forward = {
        let out = out.clone();
        let stream_id = stream_id.clone();
        async move {
            while let Some(value) = rx.recv().await {
                let item = json!({ "type": "item", "streamId": stream_id, "value": value });
                if out.send(text(item)).await.is_err() {
                    return false;
                }
            }
            true
        }
    };
    let (outcome, open) = tokio::join!(super::open_stream(&host, &endpoint, &payload, tx), forward);
    if !open {
        return;
    }
    let _ = out.send(text(terminal_frame(&stream_id, outcome))).await;
}

/// The frame that ends a stream: `end`, or `error` with the client-visible
/// `{code, message, details}`.
fn terminal_frame(stream_id: &str, outcome: Result<(), Value>) -> Value {
    match outcome {
        Ok(()) => json!({ "type": "end", "streamId": stream_id }),
        Err(error) => json!({ "type": "error", "streamId": stream_id, "error": error }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrade_admits_page_origins_and_originless_clients_only() {
        assert!(origin_allowed(None));
        for origin in PAGE_ORIGINS {
            assert!(origin_allowed(Some(&HeaderValue::from_static(origin))));
        }
        for foreign in ["https://example.com", "http://localhost:1420.evil", "null"] {
            assert!(!origin_allowed(Some(&HeaderValue::from_static(foreign))));
        }
    }

    #[test]
    fn terminal_frames_carry_the_exact_client_keys() {
        assert_eq!(
            terminal_frame("s1", Ok(())),
            json!({ "type": "end", "streamId": "s1" })
        );
        let failure = super::super::stream_failure("gateway/internal", "boom");
        assert_eq!(
            terminal_frame("s1", Err(failure)),
            json!({
                "type": "error",
                "streamId": "s1",
                "error": { "code": "gateway/internal", "message": "boom", "details": {} }
            })
        );
    }

    #[test]
    fn client_frames_parse_like_the_vendored_protocol() {
        let open: ClientMessage = serde_json::from_str(
            r#"{"type":"open","streamId":"a","endpoint":"$events","payload":{"args":{}}}"#,
        )
        .unwrap();
        assert!(matches!(open, ClientMessage::Open { ref endpoint, .. } if endpoint == "$events"));
        let cancel: ClientMessage =
            serde_json::from_str(r#"{"type":"cancel","streamId":"a"}"#).unwrap();
        assert!(matches!(cancel, ClientMessage::Cancel { ref stream_id } if stream_id == "a"));
        let item: ClientMessage =
            serde_json::from_str(r#"{"type":"item","streamId":"a","value":1}"#).unwrap();
        assert!(matches!(item, ClientMessage::Item {}));
        assert!(
            serde_json::from_str::<ClientMessage>(r#"{"type":"open","streamId":"a"}"#).is_err()
        );
        assert!(serde_json::from_str::<ClientMessage>(r#"{"type":"bogus"}"#).is_err());
    }
}
