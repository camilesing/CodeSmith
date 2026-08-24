//! `HostExtensionContext` — the host-backed [`ExtensionContext`] impl.
//!
//! Constructed by `build_extension_runtime` (Task 9) from host state. Slice 1:
//! observation methods are real (backed by fields); action methods
//! (`abort`/`shutdown`/`compact`/`get_context_usage`) inherit the trait's
//! `Unimplemented` defaults — §F2 wires them to the host's
//! `EngineHandle`/`Session`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codesmith_agent::extension::*;
use tokio_util::sync::CancellationToken;

/// Host-backed context. `generation` is the `Arc<AtomicU64>` shared with
/// [`ExtensionRunner`](crate::ExtensionRunner) so `invalidate()` is visible
/// immediately. `signal` is the **shared** `Arc<Mutex<CancellationToken>>`
/// form (§F2c Layer 2): the engine swaps the inner token under this same `Arc`
/// on every turn reset (`Engine::reset_cancel_token`) + on cancel, so
/// `signal()` returns the *current* engine token at call time rather than a
/// stale build-time snapshot.
pub struct HostExtensionContext {
    cwd: PathBuf,
    mode: ExtensionMode,
    idle: Arc<Mutex<bool>>,
    signal: Arc<Mutex<CancellationToken>>,
    generation: Arc<AtomicU64>,
}

impl HostExtensionContext {
    /// Construct from host state. `generation` MUST be the same
    /// `Arc<AtomicU64>` held by the `ExtensionRunner` (obtained via
    /// [`ExtensionRunner::generation_arc`](crate::ExtensionRunner::generation_arc))
    /// so the stale-context guard is consistent. `signal` MUST be the engine's
    /// shared `Arc<Mutex<CancellationToken>>` (the same `Arc` the engine
    /// resets under) so `signal()` reflects per-turn resets.
    #[must_use]
    pub fn new(
        cwd: PathBuf,
        mode: ExtensionMode,
        idle: Arc<Mutex<bool>>,
        signal: Arc<Mutex<CancellationToken>>,
        generation: Arc<AtomicU64>,
    ) -> Self {
        Self {
            cwd,
            mode,
            idle,
            signal,
            generation,
        }
    }
}

#[async_trait]
impl ExtensionContext for HostExtensionContext {
    fn cwd(&self) -> &Path {
        &self.cwd
    }
    fn mode(&self) -> ExtensionMode {
        self.mode
    }
    fn is_idle(&self) -> bool {
        *self.idle.lock().unwrap()
    }
    /// Snapshot the **current** engine cancel token. Locks the shared `Arc`
    /// and clones the inner token (§F2c Layer 2) — so a handler calling this
    /// after a per-turn `reset_cancel_token` sees the new token, not the
    /// build-time one. The returned `CancellationToken` is itself a snapshot;
    /// a handler that needs always-live behavior should re-call `signal()`.
    fn signal(&self) -> CancellationToken {
        self.signal
            .lock()
            .expect("extension context signal mutex poisoned")
            .clone()
    }
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

// Marker impl: `ExtensionRunner` holds the context as
// `Arc<dyn ExtensionCommandContext>` so command handlers get the sub-trait;
// event handlers receive `&dyn ExtensionContext` via trait upcasting.
impl ExtensionCommandContext for HostExtensionContext {}

#[cfg(test)]
mod tests {
    use super::*;

    /// §F2c Layer 2 — `signal()` must reflect an engine reset (the inner token
    /// being swapped under the shared `Arc`), proving the shared-Arc storage
    /// beats a stale build-time snapshot. Mirrors `Engine::reset_cancel_token`,
    /// which does `*shared.lock() = CancellationToken::new()`.
    #[test]
    fn host_extension_context_signal_reflects_engine_reset() {
        let shared = Arc::new(Mutex::new(CancellationToken::new()));
        let ctx = HostExtensionContext::new(
            PathBuf::from("."),
            ExtensionMode::Tui,
            Arc::new(Mutex::new(true)),
            Arc::clone(&shared),
            Arc::new(AtomicU64::new(0)),
        );

        // Before reset: signal() snapshots the current inner token.
        let token_a = ctx.signal();
        assert!(!token_a.is_cancelled());

        // Engine resets the token (mirrors Engine::reset_cancel_token).
        *shared.lock().unwrap() = CancellationToken::new();

        // After reset: signal() must return the NEW inner, not the stale
        // build-time snapshot.
        let token_b = ctx.signal();
        assert!(!token_b.is_cancelled(), "new token is uncancelled");

        // Cancelling the pre-reset token must not cancel the post-reset one —
        // they are distinct tokens, proving signal() followed the swap.
        token_a.cancel();
        assert!(
            !token_b.is_cancelled(),
            "signal() after reset returned a distinct token (Layer 2 shared-Arc, not stale snapshot)"
        );
    }
}
