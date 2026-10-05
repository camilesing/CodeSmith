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
use codesmith_agent::provider::{
    ProviderAlias, ProviderFactory, ProviderRegistration, SharedProviderRegistry,
};

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
    /// Route A — the runner's shared provider registry, for building
    /// aliasing factories (see `register_provider_alias`).
    shared: SharedProviderRegistry,
}

impl StubExtensionApi {
    /// Construct a stub tied to the runner's `generation` + `pending` queue.
    /// `captured_gen` is read once at construction; a later `invalidate()`
    /// makes subsequent `register_*`/`on` calls return `StaleContext`.
    pub(crate) fn new(
        generation: Arc<AtomicU64>,
        pending: Arc<Mutex<Pending>>,
        shared: SharedProviderRegistry,
    ) -> Self {
        let captured_gen = generation.load(Ordering::Acquire);
        Self {
            generation,
            captured_gen,
            pending,
            shared,
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
    fn register_provider(&self, factory: Arc<dyn ProviderFactory>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.pending
            .lock()
            .unwrap()
            .providers
            .push(crate::runner::PendingProvider { factory });
        Ok(())
    }
    fn register_provider_alias(&self, alias: ProviderAlias) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        // Build + validate here so a bad alias (shadowing id, non-builtin
        // target) fails its load — misconfiguration fails loud — instead of
        // surfacing at the first client build.
        let factory: Arc<dyn ProviderFactory> = Arc::new(
            alias
                .into_factory(self.shared.clone())
                .map_err(ExtensionError::Config)?,
        );
        self.pending
            .lock()
            .unwrap()
            .providers
            .push(crate::runner::PendingProvider { factory });
        Ok(())
    }
    fn register_prompt_section(&self, id: String, text: String) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        // Validate against the runner's rules now so a bad section fails
        // its load (fail loud), not at first prompt assembly.
        crate::runner::validate_prompt_section(&id, &text)?;
        self.pending
            .lock()
            .unwrap()
            .prompt_sections
            .push((id, text));
        Ok(())
    }

    fn register_message_projection(
        &self,
        owner: String,
        key: String,
        init: serde_json::Value,
        fold: MessageFoldFn,
    ) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.pending.lock().unwrap().message_projections.push(
            crate::runner::PendingMessageProjection {
                owner,
                key,
                init,
                fold,
            },
        );
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
    /// Route A — live registrations (drop = un-register) + the shared
    /// registry they flush into.
    providers: Arc<Mutex<Vec<ProviderRegistration>>>,
    shared: SharedProviderRegistry,
    /// Route B — prompt-section contributions (same storage the runner
    /// reads at assembly; shared so late registrations are honored).
    prompt_sections: Arc<Mutex<Vec<(String, String)>>>,
    /// Session log folds — the runner's shared hub (same `Arc` the engine
    /// folds through).
    message_projection_hub: codesmith_agent::extension::MessageProjectionHubArc,
}

#[allow(dead_code)]
impl RealExtensionApi {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        generation: Arc<AtomicU64>,
        tools: Arc<Mutex<HashMap<String, Arc<dyn ToolDefinition>>>>,
        commands: Arc<Mutex<HashMap<String, Arc<dyn CommandDefinition>>>>,
        handlers: Arc<Mutex<Vec<crate::runner::RegisteredHandler>>>,
        providers: Arc<Mutex<Vec<ProviderRegistration>>>,
        shared: SharedProviderRegistry,
        prompt_sections: Arc<Mutex<Vec<(String, String)>>>,
        message_projection_hub: codesmith_agent::extension::MessageProjectionHubArc,
    ) -> Self {
        let captured_gen = generation.load(Ordering::Acquire);
        Self {
            generation,
            captured_gen,
            tools,
            commands,
            handlers,
            providers,
            shared,
            prompt_sections,
            message_projection_hub,
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
        self.tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name, arc);
        Ok(())
    }
    fn register_command(&self, command: Box<dyn CommandDefinition>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        let name = command.name().to_string();
        let arc: Arc<dyn CommandDefinition> = Arc::from(command);
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name, arc);
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
    fn register_provider(&self, factory: Arc<dyn ProviderFactory>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        let guard = self.shared.register(factory);
        self.providers.lock().unwrap().push(guard);
        Ok(())
    }
    fn register_provider_alias(&self, alias: ProviderAlias) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        let factory: Arc<dyn ProviderFactory> = Arc::new(
            alias
                .into_factory(self.shared.clone())
                .map_err(ExtensionError::Config)?,
        );
        self.register_provider(factory)
    }
    fn register_prompt_section(&self, id: String, text: String) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        crate::runner::validate_prompt_section(&id, &text)?;
        let mut sections = self.prompt_sections.lock().unwrap();
        if let Some(slot) = sections.iter_mut().find(|(existing, _)| *existing == id) {
            slot.1 = text;
        } else {
            sections.push((id, text));
        }
        Ok(())
    }

    fn register_message_projection(
        &self,
        owner: String,
        key: String,
        init: serde_json::Value,
        fold: MessageFoldFn,
    ) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.message_projection_hub
            .register(&owner, &key, init, fold)
            .map_err(ExtensionError::Config)
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
        let stub =
            StubExtensionApi::new(generation.clone(), pending, SharedProviderRegistry::new());
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
        let stub = StubExtensionApi::new(
            generation.clone(),
            pending.clone(),
            SharedProviderRegistry::new(),
        );
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
