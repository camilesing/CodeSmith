//! `EventBus` — extension-to-extension pub/sub (spec §10.1 skeleton; full
//! impl is §F3).
//!
//! Slice 1 ships only the skeleton: the type exists so the sample extension
//! (Task 10) + `ExtensionApi` (§F2) can reference the shape. `subscribe` /
//! `publish` are stubbed to [`ExtensionError::Unimplemented`] — the §F1
//! ROADMAP entry records this.

use std::sync::Mutex;

use codesmith_agent::extension::ExtensionError;

/// A channel namespace. Slice 1: opaque string; §F3 adds typed channels.
pub type Channel = String;

/// Skeleton bus. `subscribe`/`publish` are no-ops returning `Unimplemented` —
/// §F3 fills the real impl (MPSC fan-out, namespace scoping, per-channel
/// history).
#[derive(Default)]
pub struct EventBus {
    _phantom: Mutex<()>,
}

impl EventBus {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// §F3: subscribe a callback to `channel`. Slice 1: unimplemented.
    pub fn subscribe(&self, _channel: &Channel) -> Result<(), ExtensionError> {
        Err(ExtensionError::Unimplemented("EventBus.subscribe (§F3)".into()))
    }

    /// §F3: publish `payload` to `channel`. Slice 1: unimplemented.
    pub fn publish(
        &self,
        _channel: &Channel,
        _payload: serde_json::Value,
    ) -> Result<(), ExtensionError> {
        Err(ExtensionError::Unimplemented("EventBus.publish (§F3)".into()))
    }
}
