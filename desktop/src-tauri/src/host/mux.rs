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
//! Uplink: `item`/`end` frames route to the open stream's uplink channel.
//! Only `terminal/attach` reads them today (the sidebar terminal's
//! keystrokes and resizes); every other stream drops its uplink receiver,
//! and items for such a stream are swallowed — upstream's gateway parks them
//! in an unread per-stream inbox the same way. An uplink channel that fills
//! (a reading handler that stopped consuming) fails its stream with
//! `gateway/uplink-overflow`, the upstream inbox-overflow outcome.
//!
//! Known limitations:
//! - the uplink inbox is frame-counted (16), not byte-counted like
//!   upstream's 256 KiB inbox, so `terminal/attach` carries no
//!   `inputWindow` (the wire types omit it when the carrier sets no
//!   encoded-frame byte budget);
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

/// One uplink item an open stream's handler may read (`item` value or the
/// `end` half-close).
#[derive(Debug)]
pub enum UplinkFrame {
    Item(Value),
    End,
}

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
    /// Uplink item and half-close, routed to the open stream's uplink
    /// channel (`UplinkFrame`).
    Item {
        #[serde(rename = "streamId")]
        stream_id: String,
        #[serde(default)]
        value: Value,
    },
    End {
        #[serde(rename = "streamId")]
        stream_id: String,
    },
}

/// WebSockets bypass CORS, so the upgrade checks the page origin itself —
/// otherwise any web page could open the mux and drive the bridge. An absent
/// Origin is a non-browser local client, which reaches the HTTP routes too
/// (and an `<img>` load carries no Origin header at all — the icon route
/// shares this gate).
pub(super) fn origin_allowed(origin: Option<&HeaderValue>) -> bool {
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
    // Uplink senders of open streams. An entry outliving its stream (the
    // handler returned) fails on the next send and is dropped then.
    let mut uplinks: HashMap<String, mpsc::Sender<UplinkFrame>> = HashMap::new();
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
                uplinks.retain(|_, tx| !tx.is_closed());
                if streams.contains_key(&stream_id) {
                    let _ = out_tx.send(close(1008, "duplicate Remote stream id")).await;
                    break;
                }
                let (up_tx, up_rx) = mpsc::channel(16);
                let task = tokio::spawn(pump(
                    host.clone(),
                    stream_id.clone(),
                    endpoint,
                    payload,
                    out_tx.clone(),
                    up_rx,
                ));
                streams.insert(stream_id.clone(), task);
                uplinks.insert(stream_id, up_tx);
            }
            Ok(ClientMessage::Cancel { stream_id }) => {
                uplinks.remove(&stream_id);
                if let Some(task) = streams.remove(&stream_id) {
                    task.abort();
                }
            }
            Ok(ClientMessage::Item { stream_id, value }) => {
                if let Some(tx) = uplinks.get(&stream_id) {
                    route_uplink(&out_tx, &streams, &stream_id, tx, UplinkFrame::Item(value)).await;
                }
            }
            Ok(ClientMessage::End { stream_id }) => {
                // Half-close: the handler reads End once; a later item finds
                // no sender and is dropped (upstream fails it as a protocol
                // error, which no bridged endpoint can hit).
                if let Some(tx) = uplinks.remove(&stream_id) {
                    route_uplink(&out_tx, &streams, &stream_id, &tx, UplinkFrame::End).await;
                }
            }
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

/// Deliver one uplink frame to an open stream. A channel whose handler
/// dropped the receiver (a downlink-only stream) swallows the frame; a full
/// channel is the carrier's inbox overflow and fails the stream.
async fn route_uplink(
    out: &mpsc::Sender<Message>,
    streams: &HashMap<String, JoinHandle<()>>,
    stream_id: &str,
    tx: &mpsc::Sender<UplinkFrame>,
    frame: UplinkFrame,
) {
    match tx.try_send(frame) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Closed(_)) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            let _ = out
                .send(text(json!({
                    "type": "error",
                    "streamId": stream_id,
                    "error": { "code": "gateway/uplink-overflow", "message": "stream uplink inbox is full", "details": {} }
                })))
                .await;
            if let Some(task) = streams.get(stream_id) {
                task.abort();
            }
        }
    }
}

/// Run one logical stream: each value its producer yields goes out as an
/// `item`; the producer's outcome becomes the `end` or `error` frame. The
/// uplink receiver is handed to the handler; a handler that never reads it
/// (every downlink-only endpoint) leaves it dropped.
async fn pump(
    host: SharedHost,
    stream_id: String,
    endpoint: String,
    payload: Value,
    out: mpsc::Sender<Message>,
    uplink: mpsc::Receiver<UplinkFrame>,
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
    let (outcome, open) = tokio::join!(
        super::open_stream(&host, &endpoint, &payload, tx, uplink),
        forward
    );
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
            serde_json::from_str(r#"{"type":"item","streamId":"a","value":{"type":"write"}}"#)
                .unwrap();
        assert!(
            matches!(item, ClientMessage::Item { ref stream_id, ref value } if stream_id == "a"
                && value == &json!({"type": "write"}))
        );
        let end: ClientMessage = serde_json::from_str(r#"{"type":"end","streamId":"a"}"#).unwrap();
        assert!(matches!(end, ClientMessage::End { ref stream_id } if stream_id == "a"));
        assert!(
            serde_json::from_str::<ClientMessage>(r#"{"type":"open","streamId":"a"}"#).is_err()
        );
        assert!(serde_json::from_str::<ClientMessage>(r#"{"type":"bogus"}"#).is_err());
    }

    #[tokio::test]
    async fn uplink_routes_items_and_swallows_dropped_receivers() {
        let (out_tx, mut out_rx) = mpsc::channel::<Message>(4);
        let (up_tx, mut up_rx) = mpsc::channel::<UplinkFrame>(16);
        let task = tokio::spawn(std::future::pending::<()>());
        let mut streams = HashMap::from([("s1".to_string(), task)]);
        // A live stream takes the item.
        route_uplink(
            &out_tx,
            &streams,
            "s1",
            &up_tx,
            UplinkFrame::Item(json!({"type": "write", "sequence": 1, "data": "hi"})),
        )
        .await;
        assert!(out_rx.try_recv().is_err());
        assert!(matches!(
            up_rx.recv().await,
            Some(UplinkFrame::Item(value)) if value["data"] == "hi"
        ));
        // A stream whose handler dropped the receiver swallows the frame.
        drop(up_rx);
        route_uplink(&out_tx, &streams, "s1", &up_tx, UplinkFrame::End).await;
        assert!(out_rx.try_recv().is_err());
        streams.remove("s1").unwrap().abort();
    }
}
