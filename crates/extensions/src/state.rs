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
/// immediately.
pub struct HostExtensionContext {
    cwd: PathBuf,
    mode: ExtensionMode,
    idle: Arc<Mutex<bool>>,
    signal: CancellationToken,
    generation: Arc<AtomicU64>,
}

impl HostExtensionContext {
    /// Construct from host state. `generation` MUST be the same
    /// `Arc<AtomicU64>` held by the `ExtensionRunner` (obtained via
    /// [`ExtensionRunner::generation_arc`](crate::ExtensionRunner::generation_arc))
    /// so the stale-context guard is consistent.
    #[must_use]
    pub fn new(
        cwd: PathBuf,
        mode: ExtensionMode,
        idle: Arc<Mutex<bool>>,
        signal: CancellationToken,
        generation: Arc<AtomicU64>,
    ) -> Self {
        Self { cwd, mode, idle, signal, generation }
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
    fn signal(&self) -> CancellationToken {
        self.signal.clone()
    }
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

// Marker impl: `ExtensionRunner` holds the context as
// `Arc<dyn ExtensionCommandContext>` so command handlers get the sub-trait;
// event handlers receive `&dyn ExtensionContext` via trait upcasting.
impl ExtensionCommandContext for HostExtensionContext {}
