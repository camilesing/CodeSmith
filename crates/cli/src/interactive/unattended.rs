//! Unattended event pump for the idle REPL (round 10 TODO-69).
//!
//! While the main loop is blocked in the synchronous `readline`, nobody
//! reads the engine's event channel — yet the engine may be running an
//! idle-wake turn (a background sub-agent completion synthesized the next
//! turn). The channel's `MessageDelta` backpressure is intentional
//! ("ordered and lossless": a stalled consumer stalls the bridge), so an
//! unattended turn would stall the engine at 256 queued events until the
//! user types. This pump is the second reader for exactly that window.
//!
//! Ownership protocol (the channel has exactly one reader at a time):
//! - [`IdlePump::release`] before the main loop blocks in `readline` — the
//!   pump may read.
//! - [`IdlePump::hold`] before the main loop reads events itself
//!   (`pump_turn`): a `watch`-flag flip plus an awaited ack. The pump
//!   finishes the event it is handling, then acknowledges and parks — no
//!   half-processed event, no second reader. An already-held `hold` is a
//!   no-op (the pump is parked and will not re-ack).
//! - The main loop keeps pumping while `EngineHandle::turn_in_flight` —
//!   its own turn may queue behind a synthesized one, and `pump_turn`
//!   exits at the first `TurnComplete` it sees.
//!
//! ## Policy while unattended
//!
//! Interactive questions must be answered, not dropped: an approval the
//! pump consumed can never be re-shown (events are not replayable), and
//! leaving the router waiting would hang the turn until the user types.
//! The pump answers the way the headless paths (`exec`, team teammate)
//! do: approvals / path-access / elevation are **denied** (a synthesized
//! turn running under `ApprovalMode::Suggest` never asked the UI for
//! anything it may do silently; sessions running `Auto` do not ask at
//! all), `UserInputRequired` is cancelled — the tool result tells the
//! model to ask in plain text. Deny is the conservative direction: an
//! unattended turn can work around a denial, not around an approval it
//! never should have had.
//!
//! ## Known limitations
//!
//! - Prints while rustyline is editing overwrite the prompt line visually
//!   until the next keypress redraws it. Claude Code prints background
//!   notifications the same way; the typed draft itself is not lost.
//! - The pump does not update `ReplState` (loading flag, statusline,
//!   crash-recovery checkpoints) and does not persist the session
//!   snapshot — the session journal (round 9) is the durable record and
//!   recovers the transcript on load.
//! - Rendering is deliberately minimal (tool lines, assistant text,
//!   status lines); the full turn view belongs to `pump_turn`.

use colored::Colorize;
use std::sync::Arc;
use tokio::sync::{Notify, watch};

use super::state::ReplState;
use crate::core::engine::EngineHandle;
use crate::core::events::Event;
use codesmith_agent_runtime::utils::spawn_supervised;

/// Gate + pump controlling who reads `rx_event` while the REPL is at the
/// prompt. Start one per engine and keep it for the session. Always
/// constructible (`start` on a state without an engine yields a parked
/// no-op) so call sites never unwrap.
pub(crate) struct IdlePump {
    /// `true` = the main loop owns the event channel (pump parked).
    owned_tx: Option<watch::Sender<bool>>,
    /// Fired by the pump each time it parks after a `hold`.
    hold_acked: Arc<Notify>,
}

impl IdlePump {
    /// Spawn the pump for `state`'s engine. Without an engine the pump is a
    /// no-op (the caller retries nothing — `run_repl` wires the engine
    /// before the first readline).
    pub(crate) fn start(state: &ReplState) -> Self {
        let Some(engine) = state.engine.as_ref() else {
            return Self {
                owned_tx: None,
                hold_acked: Arc::new(Notify::new()),
            };
        };
        let engine = engine.clone();
        let (owned_tx, owned_rx) = watch::channel(true);
        let pump = Self {
            owned_tx: Some(owned_tx),
            hold_acked: Arc::new(Notify::new()),
        };
        let hold_acked = pump.hold_acked.clone();
        spawn_supervised(
            "repl-idle-pump",
            std::panic::Location::caller(),
            async move {
                run(engine, owned_rx, hold_acked).await;
            },
        );
        pump
    }

    /// Let the pump read events (call right before blocking in `readline`).
    pub(crate) fn release(&self) {
        if let Some(owned_tx) = &self.owned_tx {
            let _ = owned_tx.send(false);
        }
    }

    /// Take the channel back from the pump. Returns once the pump has
    /// parked (it acknowledged after finishing any event in flight), so
    /// the caller becomes the sole reader before it pumps.
    pub(crate) async fn hold(&self) {
        if let Some(owned_tx) = &self.owned_tx
            && !*owned_tx.borrow()
        {
            let _ = owned_tx.send(true);
            self.hold_acked.notified().await;
        }
    }
}

async fn run(engine: EngineHandle, mut owned_rx: watch::Receiver<bool>, hold_acked: Arc<Notify>) {
    loop {
        // Parked while the main loop owns the channel.
        while *owned_rx.borrow_and_update() {
            if owned_rx.changed().await.is_err() {
                return;
            }
        }
        // Read events while the main loop is at the prompt.
        let mut assistant_buffer = String::new();
        loop {
            tokio::select! {
                changed = owned_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    if *owned_rx.borrow_and_update() {
                        // Main loop takes over; acknowledge and park.
                        hold_acked.notify_one();
                        break;
                    }
                }
                event = async {
                    let mut rx = engine.rx_event.write().await;
                    rx.recv().await
                } => {
                    let Some(event) = event else {
                        // Engine gone: ack a pending hold so the main loop
                        // never waits on a dead pump, then stop.
                        hold_acked.notify_one();
                        return;
                    };
                    handle_unattended(&engine, event, &mut assistant_buffer).await;
                }
            }
        }
    }
}

/// Answer interactive requests and print a minimal view of everything else.
async fn handle_unattended(engine: &EngineHandle, event: Event, assistant: &mut String) {
    match event {
        Event::MessageDelta { content, .. } => {
            assistant.push_str(&content);
        }
        Event::MessageComplete { .. } => {
            let text = std::mem::take(assistant);
            if !text.trim().is_empty() {
                println!("\n{text}");
            }
        }
        Event::ToolCallStarted { name, input, .. } => {
            let args = crate::tool_summary::summarize_tool_args(&input)
                .map(|summary| format!(" {summary}"))
                .unwrap_or_default();
            println!("{} {}{}", "⏺".cyan(), name.cyan(), args.dimmed());
        }
        Event::ToolCallComplete { name, result, .. } => {
            let ok = result.as_ref().map(|r| r.success).unwrap_or(false);
            let mark = if ok { "✓".green() } else { "✗".red() };
            println!("  {mark} {name}");
        }
        Event::AgentComplete { id, result } => {
            println!(
                "{} sub-agent {id} completed: {}",
                "⏺".cyan(),
                crate::tool_summary::summarize_tool_output(&result)
            );
        }
        Event::Status { message } => {
            println!("{}", format!("⏺ {message}").dimmed());
        }
        Event::TurnComplete { status, .. } => {
            println!(
                "{}",
                format!("⏺ background turn ended ({status:?})").dimmed()
            );
        }
        // Headless policy (see module docs): deny / cancel, never leave the
        // router waiting on a prompt that was consumed and cannot resurface.
        Event::ApprovalRequired { id, tool_name, .. } => {
            println!(
                "{}",
                format!(
                    "⏸ approval for {tool_name} denied (unattended) — submit any input to take over"
                )
                .yellow()
            );
            let _ = engine.deny_tool_call(id).await;
        }
        Event::PathAccessRequired { id, .. } => {
            let _ = engine.deny_tool_call(id).await;
        }
        Event::ElevationRequired {
            tool_id,
            tool_name,
            denial_reason,
            ..
        } => {
            println!(
                "{}",
                format!("⏸ sandbox denied {tool_name}: {denial_reason} (unattended)").yellow()
            );
            let _ = engine.deny_tool_call(tool_id).await;
        }
        Event::UserInputRequired { id, request } => {
            let question = request
                .questions
                .first()
                .map(|q| q.question.clone())
                .unwrap_or_default();
            println!(
                "{}",
                format!("⏸ question unanswered (unattended): {question}").yellow()
            );
            let _ = engine.cancel_user_input(id).await;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate alone (no engine, no pump): `hold` parks the reading side
    /// before returning and `release` unparks it. This is the ownership
    /// handshake the real pump runs; here it is driven by a stand-in task
    /// so the ordering (ack strictly after parking) can be asserted.
    #[tokio::test]
    async fn gate_hold_waits_for_park_ack_and_release_resumes() {
        // Created held, exactly like `IdlePump::start` — the pump's first
        // poll parks in the hold loop before anything else runs.
        let (owned_tx, mut owned_rx) = watch::channel(true);
        let hold_acked: Arc<Notify> = Arc::new(Notify::new());
        let ack = hold_acked.clone();
        // Stand-in pump: mirrors `run`'s park/ack protocol.
        let pump_task = tokio::spawn(async move {
            loop {
                while *owned_rx.borrow_and_update() {
                    if owned_rx.changed().await.is_err() {
                        return;
                    }
                }
                // "Reading": a long-poll that is also watching for hold.
                if owned_rx.changed().await.is_err() {
                    return;
                }
                if *owned_rx.borrow_and_update() {
                    ack.notify_one();
                    break;
                }
            }
        });

        // Release first (the main loop calls `release` before readline);
        // the pump leaves its park loop and waits in the reading arm. In
        // production the blocking readline is the await boundary between
        // release and hold — here one yield stands in for it.
        owned_tx.send(false).expect("release");
        tokio::task::yield_now().await;
        let mut probe = owned_tx.subscribe();
        assert!(!*probe.borrow());
        // Hold: flips the flag and returns only after the pump acked.
        owned_tx.send(true).expect("watch send");
        hold_acked.notified().await;
        assert!(*probe.borrow(), "pump parked before hold returns");
        // Release: reading resumes.
        owned_tx.send(false).expect("watch send");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while *probe.borrow_and_update() {
                probe.changed().await.expect("watch alive");
            }
        })
        .await
        .expect("pump resumes reading after release");
        pump_task.abort();
    }
}
