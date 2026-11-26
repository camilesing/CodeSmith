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
use tokio::sync::watch;

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
    /// `true` = the pump is parked. A state, not an event — see [`hold`].
    parked_rx: watch::Receiver<bool>,
}

impl IdlePump {
    /// Spawn the pump for `state`'s engine. Without an engine the pump is a
    /// no-op (the caller retries nothing — `run_repl` wires the engine
    /// before the first readline).
    pub(crate) fn start(state: &ReplState) -> Self {
        let Some(engine) = state.engine.as_ref() else {
            // No engine, no pump: the gate reads "parked", so `hold` is a
            // no-op.
            let (_owned_tx, parked_rx) = watch::channel(true);
            return Self {
                owned_tx: None,
                parked_rx,
            };
        };
        let engine = engine.clone();
        let (owned_tx, owned_rx) = watch::channel(true);
        let (parked_tx, parked_rx) = watch::channel(true);
        spawn_supervised(
            "repl-idle-pump",
            std::panic::Location::caller(),
            async move {
                run(engine, owned_rx, parked_tx).await;
            },
        );
        Self {
            owned_tx: Some(owned_tx),
            parked_rx,
        }
    }

    /// Let the pump read events (call right before blocking in `readline`).
    pub(crate) fn release(&self) {
        if let Some(owned_tx) = &self.owned_tx {
            let _ = owned_tx.send(false);
        }
    }

    /// Take the channel back from the pump: returns once the pump is
    /// parked, so the caller is the sole reader before it pumps.
    pub(crate) async fn hold(&self) {
        if let Some(owned_tx) = &self.owned_tx {
            hold(owned_tx, &self.parked_rx).await;
        }
    }
}

/// Park while the main loop owns the channel; publish `false` ("reading")
/// on the way out.
///
/// "Parked" is published on every entry. It has to be a state, not an
/// event: a release and the next `hold` that both land before the pump is
/// polled again leave it in this loop, where an ack sent only from the read
/// loop never comes — `hold` waited forever and the REPL froze at its
/// prompt. Stored as a state, the pump's parked flag releases that `hold`
/// at once.
async fn park_while_owned(
    owned_rx: &mut watch::Receiver<bool>,
    parked_tx: &watch::Sender<bool>,
) -> Result<(), watch::error::RecvError> {
    let _ = parked_tx.send(true);
    while *owned_rx.borrow_and_update() {
        owned_rx.changed().await?;
    }
    let _ = parked_tx.send(false);
    Ok(())
}

/// Ask the pump to park and return once it has.
async fn hold(owned_tx: &watch::Sender<bool>, parked_rx: &watch::Receiver<bool>) {
    if !*owned_tx.borrow() {
        let _ = owned_tx.send(true);
        let mut parked = parked_rx.clone();
        let _ = parked.wait_for(|parked| *parked).await;
    }
}

async fn run(
    engine: EngineHandle,
    mut owned_rx: watch::Receiver<bool>,
    parked_tx: watch::Sender<bool>,
) {
    loop {
        if park_while_owned(&mut owned_rx, &parked_tx).await.is_err() {
            return;
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
                        // Main loop takes over: publish "parked" and park.
                        let _ = parked_tx.send(true);
                        break;
                    }
                }
                event = async {
                    let mut rx = engine.rx_event.write().await;
                    rx.recv().await
                } => {
                    let Some(event) = event else {
                        // Engine gone: publish "parked" so a pending hold
                        // never waits on a dead pump, then stop.
                        let _ = parked_tx.send(true);
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
    use codesmith_agent_runtime::events::TurnOutcomeStatus;

    /// P1-1 — the REPL pump-path invariant: a synthesized (idle-wake) turn's
    /// full event sequence flows through `handle_unattended` like any other
    /// turn — streamed text accumulates and prints, the turn end renders,
    /// nothing special-cases (or drops) `synthesized` turns at the prompt.
    /// The ACP side of the invariant (a synthesized turn never completes a
    /// pending client prompt) is pinned in `acp_server`'s tests.
    #[tokio::test]
    async fn synthesized_turn_sequence_flows_through_the_unattended_handler() {
        use crate::config::Config;
        use crate::core::engine::{EngineConstruct, HeadlessEngineOptions, headless_engine_config};
        use codesmith_agent_runtime::engine::Engine;

        // A real (idle) engine so the deny paths have a live handle; the
        // synthesized-turn sequence below never asks for one.
        let engine_config = headless_engine_config(
            &Config::default(),
            &crate::settings::Settings::default(),
            HeadlessEngineOptions {
                model: "pump-test".to_string(),
                workspace: std::env::temp_dir(),
                auto_approve: true,
                trust_mode: true,
                max_subagents: 1,
                override_system_prompt: None,
                append_system_prompts: Vec::new(),
                cache_breaker: None,
                allowed_tools: None,
                team_context: None,
            },
        )
        .expect("engine config");
        let (engine, handle) = <Engine as EngineConstruct>::new_with_client(
            engine_config,
            &Config::default(),
            std::sync::Arc::new(
                codesmith_agent_runtime::test_support::mock_llm::MockLlmClient::new(Vec::new()),
            ),
        );
        tokio::spawn(async move {
            engine.run().await;
        });

        let mut assistant = String::new();
        // The exact event shape an idle-wake synthesized turn emits.
        for event in [
            Event::TurnStarted {
                turn_id: "t1".to_string(),
                synthesized: true,
            },
            Event::MessageDelta {
                index: 0,
                content: "coordinated ".to_string(),
            },
            Event::MessageDelta {
                index: 0,
                content: "after wake".to_string(),
            },
            Event::MessageComplete { index: 0 },
            Event::TurnComplete {
                usage: Default::default(),
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
                reported_model: None,
                failed_attempts: Vec::new(),
            },
        ] {
            handle_unattended(&handle, event, &mut assistant).await;
        }
        // The deltas accumulated (MessageComplete printed and took them).
        assert!(assistant.is_empty(), "complete flushed the buffer");
        // The engine is still alive — the handler never kills the pump.
        assert!(
            handle
                .send(codesmith_agent_runtime::ops::Op::Shutdown)
                .await
                .is_ok()
        );
    }

    /// The gate alone (no engine, no pump): `hold` parks the reading side
    /// before returning and `release` unparks it. This is the ownership
    /// handshake the real pump runs; here it is driven by a stand-in task
    /// so the ordering (ack strictly after parking) can be asserted.
    /// A stand-in for `run`: the real park loop and the real park-on-hold
    /// publish, with nothing to read in between — the reading arm just
    /// waits for the next ownership change.
    fn stand_in_pump() -> (
        watch::Sender<bool>,
        watch::Receiver<bool>,
        tokio::task::JoinHandle<()>,
    ) {
        let (owned_tx, mut owned_rx) = watch::channel(true);
        let (parked_tx, parked_rx) = watch::channel(true);
        let task = tokio::spawn(async move {
            loop {
                if park_while_owned(&mut owned_rx, &parked_tx).await.is_err() {
                    return;
                }
                if owned_rx.changed().await.is_err() {
                    return;
                }
                if *owned_rx.borrow_and_update() {
                    let _ = parked_tx.send(true);
                    break;
                }
            }
        });
        (owned_tx, parked_rx, task)
    }

    /// The gate alone (no engine, no pump): `hold` returns only once the
    /// pump is parked and `release` unparks it — the ownership handshake
    /// the real pump runs, driven by a stand-in task so the ordering can be
    /// asserted.
    #[tokio::test]
    async fn gate_hold_waits_for_the_pump_to_park_and_release_resumes() {
        let (owned_tx, parked_rx, pump_task) = stand_in_pump();

        // Release first (the main loop calls `release` before readline);
        // the pump leaves its park loop and reads. In production the
        // blocking readline is the await boundary between release and hold
        // — here one yield stands in for it.
        owned_tx.send(false).expect("release");
        tokio::task::yield_now().await;
        let probe = owned_tx.subscribe();
        assert!(!*probe.borrow());
        assert!(!*parked_rx.borrow(), "the pump published 'reading'");

        // Hold: flips the flag and returns only once the pump is parked.
        hold(&owned_tx, &parked_rx).await;
        assert!(
            *parked_rx.borrow(),
            "the pump is parked before hold returns"
        );

        // Release: reading resumes.
        owned_tx.send(false).expect("release");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let mut parked = parked_rx.clone();
            let _ = parked.wait_for(|parked| !*parked).await;
        })
        .await
        .expect("the pump resumes reading after release");
        pump_task.abort();
    }

    /// Found in the review: "parked" was an event — a `Notify` ack sent
    /// only from the read loop. A release and the next hold that both land
    /// before the pump is polled again (a buffered Enter: `readline`
    /// returns at once) leave it in the park loop, where no ack is ever
    /// sent: `hold` never returned, no prompt was drawn, and the session
    /// had to be killed. Parked is a state now.
    #[tokio::test]
    async fn hold_returns_for_a_release_the_pump_never_saw() {
        let (owned_tx, parked_rx, pump_task) = stand_in_pump();

        // No await between the two: the pump is not polled in between, so
        // it never leaves its park loop and never publishes "reading".
        owned_tx.send(false).expect("release");
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            hold(&owned_tx, &parked_rx),
        )
        .await
        .expect("a release the pump never saw must not hang the next hold");

        pump_task.abort();
    }
}
