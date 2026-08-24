//! `ExtensionApi` stub + real impls (two-phase construction, spec §4).
//!
//! The stub (constructed at load time by `ExtensionRunner::load`) queues
//! registrations into the runner's `pending_*`; the runner drains `pending_*`
//! at `bind_core`. The real impl (flushes directly into the bound
//! registries) is defined here for the §F2 long-lived-`Arc<dyn
//! ExtensionApi>` case (extensions that retain the api for lazy
//! registration); slice 1 does not construct it.
//!
//! The generation guard (`assert_live`) is the stable contract: a captured
//! `ExtensionApi` whose `captured_gen` no longer matches the live
//! `generation.load()` returns [`ExtensionError::StaleContext`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codesmith_agent::extension::*;

use crate::runner::Pending;

/// Return `Ok(())` if the runtime generation still matches `captured`, else
/// `StaleContext`. The stable stale-context guard (spec §7.3).
fn assert_live(generation: &Arc<AtomicU64>, captured: u64) -> Result<(), ExtensionError> {
    if generation.load(Ordering::Acquire) == captured {
        Ok(())
    } else {
        Err(ExtensionError::StaleContext)
    }
}

/// Stub api — queues registrations into a shared `pending` that the runner
/// drains at `bind_core`. Lifetime: the duration of `Extension::configure`.
pub struct StubExtensionApi {
    generation: Arc<AtomicU64>,
    captured_gen: u64,
    pending: Arc<Mutex<Pending>>,
}

impl StubExtensionApi {
    /// Construct a stub tied to the runner's `generation` + `pending` queue.
    /// `captured_gen` is read once at construction; a later `invalidate()`
    /// makes subsequent `register_*`/`on` calls return `StaleContext`.
    pub(crate) fn new(generation: Arc<AtomicU64>, pending: Arc<Mutex<Pending>>) -> Self {
        let captured_gen = generation.load(Ordering::Acquire);
        Self {
            generation,
            captured_gen,
            pending,
        }
    }
}

#[async_trait]
impl ExtensionApi for StubExtensionApi {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    fn register_tool(&self, tool: Box<dyn ToolDefinition>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.pending
            .lock()
            .unwrap()
            .tools
            .push(crate::runner::PendingTool { tool });
        Ok(())
    }
    fn register_command(&self, command: Box<dyn CommandDefinition>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.pending
            .lock()
            .unwrap()
            .commands
            .push(crate::runner::PendingCommand { command });
        Ok(())
    }
    fn on(&self, handler: Arc<dyn Handler>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.pending
            .lock()
            .unwrap()
            .handlers
            .push(crate::runner::PendingHandler {
                handler,
                kind_filter: None,
            });
        Ok(())
    }
    fn on_variant(
        &self,
        kind: ExtensionEventKind,
        handler: Arc<dyn Handler>,
    ) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.pending
            .lock()
            .unwrap()
            .handlers
            .push(crate::runner::PendingHandler {
                handler,
                kind_filter: Some(kind),
            });
        Ok(())
    }
}

/// Real api — live after `bind_core`; flushes registrations directly into
/// the bound runner registries. Slice 1: defined but not constructed (the
/// primary path is stub + flush). §F2 constructs it for extensions that
/// retain a long-lived `Arc<dyn ExtensionApi>` (lazy registration).
#[allow(dead_code)]
pub struct RealExtensionApi {
    generation: Arc<AtomicU64>,
    captured_gen: u64,
    tools: Arc<Mutex<HashMap<String, Arc<dyn ToolDefinition>>>>,
    commands: Arc<Mutex<HashMap<String, Arc<dyn CommandDefinition>>>>,
    handlers: Arc<Mutex<Vec<crate::runner::RegisteredHandler>>>,
}

#[allow(dead_code)]
impl RealExtensionApi {
    pub(crate) fn new(
        generation: Arc<AtomicU64>,
        tools: Arc<Mutex<HashMap<String, Arc<dyn ToolDefinition>>>>,
        commands: Arc<Mutex<HashMap<String, Arc<dyn CommandDefinition>>>>,
        handlers: Arc<Mutex<Vec<crate::runner::RegisteredHandler>>>,
    ) -> Self {
        let captured_gen = generation.load(Ordering::Acquire);
        Self {
            generation,
            captured_gen,
            tools,
            commands,
            handlers,
        }
    }
}

#[async_trait]
impl ExtensionApi for RealExtensionApi {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    fn register_tool(&self, tool: Box<dyn ToolDefinition>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        let name = tool.name().to_string();
        let arc: Arc<dyn ToolDefinition> = Arc::from(tool);
        self.tools.lock().unwrap().insert(name, arc);
        Ok(())
    }
    fn register_command(&self, command: Box<dyn CommandDefinition>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        let name = command.name().to_string();
        let arc: Arc<dyn CommandDefinition> = Arc::from(command);
        self.commands.lock().unwrap().insert(name, arc);
        Ok(())
    }
    fn on(&self, handler: Arc<dyn Handler>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.handlers
            .lock()
            .unwrap()
            .push(crate::runner::RegisteredHandler {
                handler,
                kind_filter: None,
            });
        Ok(())
    }
    fn on_variant(
        &self,
        kind: ExtensionEventKind,
        handler: Arc<dyn Handler>,
    ) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.handlers
            .lock()
            .unwrap()
            .push(crate::runner::RegisteredHandler {
                handler,
                kind_filter: Some(kind),
            });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Pending;

    #[tokio::test]
    async fn stub_after_invalidate_returns_stale_context() {
        let generation = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(Pending::default()));
        let stub = StubExtensionApi::new(generation.clone(), pending);
        generation.fetch_add(1, Ordering::AcqRel);
        struct Nop;
        #[async_trait]
        impl Handler for Nop {
            async fn handle(
                &self,
                _: &ExtensionEvent,
                _: &dyn ExtensionContext,
            ) -> Result<HandlerOutcome, ExtensionError> {
                Ok(HandlerOutcome::Continue)
            }
        }
        let err = stub.on(Arc::new(Nop)).unwrap_err();
        assert!(matches!(err, ExtensionError::StaleContext));
    }

    #[tokio::test]
    async fn f2a_stub_on_variant_queues_with_kind_filter() {
        use codesmith_agent::extension::ExtensionEventKind;
        let generation = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(Pending::default()));
        let stub = StubExtensionApi::new(generation.clone(), pending.clone());
        struct Nop;
        #[async_trait]
        impl Handler for Nop {
            async fn handle(
                &self,
                _: &ExtensionEvent,
                _: &dyn ExtensionContext,
            ) -> Result<HandlerOutcome, ExtensionError> {
                Ok(HandlerOutcome::Continue)
            }
        }
        stub.on_variant(ExtensionEventKind::ToolCall, Arc::new(Nop))
            .unwrap();
        let p = pending.lock().unwrap();
        assert_eq!(p.handlers.len(), 1);
        assert_eq!(
            p.handlers[0].kind_filter,
            Some(ExtensionEventKind::ToolCall)
        );
    }
}
