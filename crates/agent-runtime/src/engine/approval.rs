//! Approval + user-input handshake for the agent loop.
//!
//! Extracted from `core/engine.rs` (P1.3). The agent loop blocks on these
//! two futures whenever a tool requires explicit approval (`await_tool_approval`)
//! or whenever a tool requests live user input (`await_user_input`). Channels
//! and engine state stay private to the parent module.
//!
//! [`ApprovalRouter`] is the receiving half of the decision channel. Its
//! reason to exist is concurrency: the parent turn's gate and every running
//! sub-agent wait on one channel, and the host answers in the order it
//! prompted — which need not be the order the waiters arrived. A decision
//! addressed to another waiter is parked for it rather than dropped.

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::user_input::UserInputResponse;

#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    Approved {
        id: String,
    },
    Denied {
        id: String,
    },
    /// Retry a tool with an elevated sandbox policy.
    RetryWithPolicy {
        id: String,
        policy: crate::sandbox::SandboxPolicy,
    },
}

impl ApprovalDecision {
    /// The tool-call id this decision answers (the wire id the ask carried).
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Approved { id } | Self::Denied { id } | Self::RetryWithPolicy { id, .. } => id,
        }
    }
}

/// Why a waiter stopped waiting without a decision for its own id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalWaitError {
    /// The turn (or the waiting sub-agent) was cancelled — the caller feeds
    /// back an interrupted/permission-denied error.
    Cancelled,
    /// The decision channel closed; no answer can arrive.
    Closed,
}

/// One decision channel, many waiters.
///
/// Waiting holds the receive lock across `recv()` — the single-waiter case
/// (a turn with no sub-agents) behaves exactly like the raw mutex+receiver it
/// replaces. With several waiters, a decision for a different id is parked in
/// `parked` and picked up by its owner on its next loop iteration, so an
/// answer is never consumed by the wrong waiter (which would leave it waiting
/// until cancellation).
#[derive(Debug)]
pub struct ApprovalRouter {
    rx: tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>,
    parked: std::sync::Mutex<Vec<ApprovalDecision>>,
}

impl ApprovalRouter {
    #[must_use]
    pub fn new(rx: mpsc::Receiver<ApprovalDecision>) -> Self {
        Self {
            rx: tokio::sync::Mutex::new(rx),
            parked: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Await the decision addressed to `id`.
    pub async fn wait(
        &self,
        id: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<ApprovalDecision, ApprovalWaitError> {
        loop {
            if let Some(decision) = self.take_parked(id) {
                return Ok(decision);
            }
            let received = {
                let mut rx = self.rx.lock().await;
                // Re-check under the lock. Our own answer can be parked by
                // another waiter while we sit queued behind its receive — the
                // loop-top check above ran before that, so without this the
                // answer would be found only after the channel delivers
                // something else (i.e. possibly never).
                if let Some(decision) = self.take_parked(id) {
                    return Ok(decision);
                }
                let cancelled = async {
                    match cancel {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::select! {
                    biased;
                    _ = cancelled => return Err(ApprovalWaitError::Cancelled),
                    decision = rx.recv() => decision,
                }
            };
            match received {
                None => return Err(ApprovalWaitError::Closed),
                Some(decision) if decision.id() == id => return Ok(decision),
                // Another waiter's answer: park it, never drop it — dropping
                // wedges that waiter until its turn is cancelled.
                Some(foreign) => self.park(foreign),
            }
        }
    }

    /// Await the decision addressed to `id` with a deadline, for waiters whose
    /// host may never answer (a sub-agent asking while the REPL sits idle at
    /// its prompt). Returns `None` when the deadline passes first.
    pub async fn wait_with_timeout(
        &self,
        id: &str,
        cancel: Option<&CancellationToken>,
        timeout: std::time::Duration,
    ) -> Result<Option<ApprovalDecision>, ApprovalWaitError> {
        match tokio::time::timeout(timeout, self.wait(id, cancel)).await {
            Ok(Ok(decision)) => Ok(Some(decision)),
            Ok(Err(err)) => Err(err),
            Err(_) => Ok(None),
        }
    }

    fn take_parked(&self, id: &str) -> Option<ApprovalDecision> {
        let mut parked = self.parked.lock().ok()?;
        let index = parked.iter().position(|decision| decision.id() == id)?;
        Some(parked.remove(index))
    }

    fn park(&self, decision: ApprovalDecision) {
        if let Ok(mut parked) = self.parked.lock() {
            parked.push(decision);
        }
    }
}

#[derive(Debug, Clone)]
pub enum UserInputDecision {
    Submitted {
        id: String,
        response: UserInputResponse,
    },
    Cancelled {
        id: String,
    },
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::{ApprovalDecision, ApprovalRouter, ApprovalWaitError};

    /// Answers are routed by id, not consumed in arrival order. Two waiters —
    /// a turn's gate and a sub-agent — ask concurrently and the host answers
    /// the second one first; each must still receive its own decision. The
    /// timeout is the assertion: the drop-on-mismatch loop this router
    /// replaced left the waiter that lost the receive race waiting forever.
    #[tokio::test]
    async fn decisions_route_to_their_own_waiter_out_of_order() {
        let (tx, rx) = mpsc::channel(8);
        let router = Arc::new(ApprovalRouter::new(rx));

        let parent = {
            let router = Arc::clone(&router);
            tokio::spawn(async move { router.wait("parent_turn", None).await })
        };
        let sub_agent = {
            let router = Arc::clone(&router);
            tokio::spawn(async move { router.wait("sub_agent", None).await })
        };
        // Let both waiters reach the channel, then answer out of order.
        tokio::time::sleep(Duration::from_millis(50)).await;

        tx.send(ApprovalDecision::Denied {
            id: "sub_agent".to_string(),
        })
        .await
        .expect("send sub-agent decision");
        tx.send(ApprovalDecision::Approved {
            id: "parent_turn".to_string(),
        })
        .await
        .expect("send parent decision");

        let (parent, sub_agent) = tokio::time::timeout(Duration::from_secs(5), async {
            (
                parent.await.expect("parent waiter joined"),
                sub_agent.await.expect("sub-agent waiter joined"),
            )
        })
        .await
        .expect("every waiter must receive its own decision");

        let parent = parent.expect("parent decision");
        assert_eq!(parent.id(), "parent_turn");
        assert!(matches!(parent, ApprovalDecision::Approved { .. }));
        let sub_agent = sub_agent.expect("sub-agent decision");
        assert_eq!(sub_agent.id(), "sub_agent");
        assert!(matches!(sub_agent, ApprovalDecision::Denied { .. }));
    }

    /// A sub-agent can ask while the REPL sits idle at its prompt, where
    /// nothing is pumping events. The ask must then give up (and the caller
    /// refuse the path) instead of wedging the child forever.
    #[tokio::test]
    async fn wait_with_timeout_gives_up_when_nobody_answers() {
        let (_tx, rx) = mpsc::channel(8);
        let router = ApprovalRouter::new(rx);

        let outcome = router
            .wait_with_timeout("unanswered", None, Duration::from_millis(20))
            .await
            .expect("no channel error");

        assert!(
            outcome.is_none(),
            "an unanswered ask must time out, not hang"
        );
    }

    #[tokio::test]
    async fn wait_reports_cancellation_instead_of_denying() {
        let (_tx, rx) = mpsc::channel(8);
        let router = ApprovalRouter::new(rx);
        let token = CancellationToken::new();
        token.cancel();

        let err = router
            .wait("call_a", Some(&token))
            .await
            .expect_err("a cancelled wait must not report a decision");

        assert_eq!(err, ApprovalWaitError::Cancelled);
    }
}
