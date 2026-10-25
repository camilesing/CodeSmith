//! `ExtensionApi` stub + real impls (two-phase construction, spec §4).
//!
//! The stub (constructed at load time by `ExtensionRunner::load`) queues
//! registrations into the runner's `pending_*`; the runner drains `pending_*`
//! at `bind_core`. The real impl (flushes directly into the bound registries)
//! is defined here for the §F2 long-lived-`Arc<dyn ExtensionApi>` case
//! (extensions that retain the api for lazy registration); slice 1 does not
//! construct it.
//!
//! The generation guard (`assert_live`) is the stable contract: a captured
//! `ExtensionApi` whose `captured_gen` no longer matches the live
//! `generation.load()` returns [`ExtensionError::StaleContext`]. Every stub
//! enqueue checks the generation while holding the `pending` lock, and
//! [`ExtensionRunner::invalidate`](crate::ExtensionRunner::invalidate) bumps
//! the generation under the same lock — so a registration can never be
//! checked live and then queued after an invalidation (§7.3).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codesmith_agent::extension::*;
use codesmith_agent::provider::{
    ProviderAlias, ProviderFactory, ProviderRegistration, SharedProviderRegistry,
};

use crate::runner::{Pending, RegistrationOrigin};

/// Return `Ok(())` if the runtime generation still matches `captured`, else
/// `StaleContext`. The stable stale-context guard (spec §7.3). Callers must
/// hold the queue/registry lock the result will push into, so the check and
/// the enqueue are one atomic step against `invalidate()`'s generation bump.
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
    /// The load-path stamp (§F5d unload-safety tier) carried onto every
    /// queued registration.
    origin: RegistrationOrigin,
}

impl StubExtensionApi {
    /// Construct a stub tied to the runner's `generation` + `pending` queue.
    /// `captured_gen` is read once at construction; a later `invalidate()`
    /// makes subsequent `register_*`/`on` calls return `StaleContext`.
    pub(crate) fn new(
        generation: Arc<AtomicU64>,
        pending: Arc<Mutex<Pending>>,
        shared: SharedProviderRegistry,
        origin: RegistrationOrigin,
    ) -> Self {
        let captured_gen = generation.load(Ordering::Acquire);
        Self {
            generation,
            captured_gen,
            pending,
            shared,
            origin,
        }
    }
}

/// A dylib-origin extension cannot reach sub-agent surfaces (its
/// registrations are main-turn-only, §F5d unload safety), so a *scoped*
/// registration from one would never fire — reject it at load (fail loud).
fn reject_dylib_scoped(origin: RegistrationOrigin, what: &str) -> Result<(), ExtensionError> {
    if origin.is_dylib() {
        return Err(ExtensionError::Load(format!(
            "a dylib extension cannot register scoped {what}: scoped registrations serve \
             sub-agent surfaces, which dylib contributions cannot reach (unload safety)"
        )));
    }
    Ok(())
}

#[async_trait]
impl ExtensionApi for StubExtensionApi {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    fn register_tool(&self, tool: Box<dyn ToolDefinition>) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.tools.push(crate::runner::PendingTool {
            tool,
            scope: Scope::Global,
            origin: self.origin,
        });
        Ok(())
    }
    fn register_tool_scoped(
        &self,
        scope: Scope,
        tool: Box<dyn ToolDefinition>,
    ) -> Result<(), ExtensionError> {
        scope.validate()?;
        reject_dylib_scoped(self.origin, "tools")?;
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.tools.push(crate::runner::PendingTool {
            tool,
            scope,
            origin: self.origin,
        });
        Ok(())
    }
    fn register_command(&self, command: Box<dyn CommandDefinition>) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending
            .commands
            .push(crate::runner::PendingCommand { command });
        Ok(())
    }
    fn disable_command(&self, name: String) -> Result<(), ExtensionError> {
        // The framework stores the name opaquely; the host resolves it
        // against its builtin table (warn + no-op on an unknown name — the
        // script-bug posture, mirroring bad policy knobs). Only an empty
        // name fails the load here.
        if name.trim().is_empty() {
            return Err(ExtensionError::Config(
                "disable_command(): name must be a built-in command name".into(),
            ));
        }
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.disabled_commands.push(name);
        Ok(())
    }
    fn on(&self, handler: Arc<dyn Handler>) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.handlers.push(crate::runner::PendingHandler {
            handler,
            kind_filter: None,
            scope: Scope::Global,
            origin: self.origin,
        });
        Ok(())
    }
    fn on_variant(
        &self,
        kind: ExtensionEventKind,
        handler: Arc<dyn Handler>,
    ) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.handlers.push(crate::runner::PendingHandler {
            handler,
            kind_filter: Some(kind),
            scope: Scope::Global,
            origin: self.origin,
        });
        Ok(())
    }
    fn on_variant_scoped(
        &self,
        kind: ExtensionEventKind,
        scope: Scope,
        handler: Arc<dyn Handler>,
    ) -> Result<(), ExtensionError> {
        scope.validate()?;
        reject_dylib_scoped(self.origin, "handlers")?;
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.handlers.push(crate::runner::PendingHandler {
            handler,
            kind_filter: Some(kind),
            scope,
            origin: self.origin,
        });
        Ok(())
    }
    fn restrict_tools(&self, scope: Scope, names: Vec<String>) -> Result<(), ExtensionError> {
        // Validate against the runner's rules now so a bad restriction
        // fails its load (fail loud), not at the next catalog build.
        crate::runner::validate_tool_restriction(&scope, &names)?;
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending
            .tool_restrictions
            .push(crate::runner::ToolRestriction {
                scope,
                denied: names,
            });
        Ok(())
    }
    fn register_around_tool(
        &self,
        handler: Arc<dyn codesmith_agent::extension::AroundToolHandler>,
    ) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.around_tools.push(handler);
        Ok(())
    }
    fn register_provider(&self, factory: Arc<dyn ProviderFactory>) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending
            .providers
            .push(crate::runner::PendingProvider { factory });
        Ok(())
    }
    fn register_provider_alias(&self, alias: ProviderAlias) -> Result<(), ExtensionError> {
        // Build + validate here so a bad alias (shadowing id, non-builtin
        // target) fails its load — misconfiguration fails loud — instead of
        // surfacing at the first client build.
        let factory: Arc<dyn ProviderFactory> = Arc::new(
            alias
                .into_factory(self.shared.clone())
                .map_err(ExtensionError::Config)?,
        );
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending
            .providers
            .push(crate::runner::PendingProvider { factory });
        Ok(())
    }
    fn register_prompt_section(&self, id: String, text: String) -> Result<(), ExtensionError> {
        // Validate against the runner's rules now so a bad section fails
        // its load (fail loud), not at first prompt assembly.
        crate::runner::validate_prompt_section(&id, &text)?;
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.prompt_sections.push((id, text));
        Ok(())
    }

    fn register_skill(
        &self,
        skill: codesmith_agent::extension::SkillRegistration,
    ) -> Result<(), ExtensionError> {
        // Validate now so a bad skill fails its load (fail loud), not at
        // first catalogue render.
        crate::runner::validate_skill_registration(&skill.name, &skill.description, &skill.body)?;
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.skills.push(skill);
        Ok(())
    }

    fn register_message_projection(
        &self,
        owner: String,
        key: String,
        init: serde_json::Value,
        fold: MessageFoldFn,
    ) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending
            .message_projections
            .push(crate::runner::PendingMessageProjection {
                owner,
                key,
                init,
                fold,
            });
        Ok(())
    }

    fn register_policy(
        &self,
        owner: String,
        kind: codesmith_agent::policy::PolicyKind,
        config: serde_json::Value,
    ) -> Result<(), ExtensionError> {
        // A policy is data: require a JSON object so a malformed
        // registration fails its load (fail loud), not at first read.
        if !config.is_object() {
            return Err(ExtensionError::Config(format!(
                "policy {} config must be a JSON object, got {config}",
                kind.kebab_name()
            )));
        }
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending
            .policies
            .push(codesmith_agent::policy::ResolvedPolicy {
                kind,
                config,
                source: owner,
            });
        Ok(())
    }
}

/// Real api — live after `bind_core`; flushes registrations directly into
/// the bound runner registries. Slice 1: defined but not constructed (the
/// primary path is stub + flush). §F2 constructs it for extensions that
/// retain a long-lived `Arc<dyn ExtensionApi>` (lazy registration).
/// The scoped registration surface (`register_tool_scoped` /
/// `on_variant_scoped` / `restrict_tools`) is NOT implemented here — it
/// inherits the trait's loud default errors (zero consumers; same posture
/// as iteration 4's `register_around_tool`) until one shows up.
#[allow(dead_code)]
pub struct RealExtensionApi {
    generation: Arc<AtomicU64>,
    captured_gen: u64,
    /// The load-path stamp (§F5d unload-safety tier) carried onto every
    /// registration — supplied by the constructor like the stub's, never a
    /// baked-in default (a wrong tier would silently grant dylib
    /// registrations the most-privileged unload-safety level).
    origin: RegistrationOrigin,
    tools: Arc<Mutex<crate::runner::BoundToolMap>>,
    commands: Arc<Mutex<HashMap<String, Arc<dyn CommandDefinition>>>>,
    handlers: Arc<Mutex<Vec<crate::runner::RegisteredHandler>>>,
    /// Route A — live registrations (drop = un-register) + the shared
    /// registry they flush into.
    providers: Arc<Mutex<Vec<ProviderRegistration>>>,
    shared: SharedProviderRegistry,
    /// Route B — prompt-section contributions (same storage the runner
    /// reads at assembly; shared so late registrations are honored).
    prompt_sections: Arc<Mutex<Vec<(String, String)>>>,
    /// Route B (skills) — registered skills (same storage the runner
    /// snapshots for catalogue render / `load_skill`).
    skills: Arc<Mutex<Vec<codesmith_agent::extension::SkillRegistration>>>,
    /// Session log folds — the runner's shared hub (same `Arc` the engine
    /// folds through).
    message_projection_hub: codesmith_agent::extension::MessageProjectionHubArc,
}

#[allow(dead_code)]
impl RealExtensionApi {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        generation: Arc<AtomicU64>,
        origin: RegistrationOrigin,
        tools: Arc<Mutex<crate::runner::BoundToolMap>>,
        commands: Arc<Mutex<HashMap<String, Arc<dyn CommandDefinition>>>>,
        handlers: Arc<Mutex<Vec<crate::runner::RegisteredHandler>>>,
        providers: Arc<Mutex<Vec<ProviderRegistration>>>,
        shared: SharedProviderRegistry,
        prompt_sections: Arc<Mutex<Vec<(String, String)>>>,
        skills: Arc<Mutex<Vec<codesmith_agent::extension::SkillRegistration>>>,
        message_projection_hub: codesmith_agent::extension::MessageProjectionHubArc,
    ) -> Self {
        let captured_gen = generation.load(Ordering::Acquire);
        Self {
            generation,
            captured_gen,
            origin,
            tools,
            commands,
            handlers,
            providers,
            shared,
            prompt_sections,
            skills,
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
        let bound = crate::runner::BoundTool {
            def: Arc::from(tool),
            origin: self.origin,
        };
        self.tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((Scope::Global, name), bound);
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
            .unwrap_or_else(|e| e.into_inner())
            .push(crate::runner::RegisteredHandler {
                handler,
                kind_filter: None,
                scope: Scope::Global,
                origin: self.origin,
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
            .unwrap_or_else(|e| e.into_inner())
            .push(crate::runner::RegisteredHandler {
                handler,
                kind_filter: Some(kind),
                scope: Scope::Global,
                origin: self.origin,
            });
        Ok(())
    }
    fn register_provider(&self, factory: Arc<dyn ProviderFactory>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        let guard = self.shared.register(factory);
        self.providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(guard);
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
        let mut sections = self
            .prompt_sections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = sections.iter_mut().find(|(existing, _)| *existing == id) {
            slot.1 = text;
        } else {
            // Same cap the runner's flush path enforces — the real api
            // writes the shared store directly, so the limit holds here too.
            if sections.len() >= crate::runner::MAX_PROMPT_SECTIONS {
                return Err(ExtensionError::Config(format!(
                    "too many prompt sections (>{})",
                    crate::runner::MAX_PROMPT_SECTIONS
                )));
            }
            sections.push((id, text));
        }
        Ok(())
    }

    fn register_skill(
        &self,
        skill: codesmith_agent::extension::SkillRegistration,
    ) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        crate::runner::validate_skill_registration(&skill.name, &skill.description, &skill.body)?;
        let mut skills = self.skills.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = skills
            .iter()
            .find(|s| s.name == skill.name && s.owner != skill.owner)
        {
            return Err(ExtensionError::Config(format!(
                "skill {:?} already registered by mod {:?}",
                skill.name, existing.owner
            )));
        }
        if let Some(slot) = skills.iter_mut().find(|s| s.name == skill.name) {
            *slot = skill;
        } else {
            // Same cap the runner's flush path enforces (see above).
            if skills.len() >= codesmith_agent::extension::MAX_REGISTERED_SKILLS {
                return Err(ExtensionError::Config(format!(
                    "too many registered skills (>{}), rejecting {:?} from mod {:?}",
                    codesmith_agent::extension::MAX_REGISTERED_SKILLS,
                    skill.name,
                    skill.owner
                )));
            }
            skills.push(skill);
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
        let stub = StubExtensionApi::new(
            generation.clone(),
            pending,
            SharedProviderRegistry::new(),
            crate::runner::RegistrationOrigin::CompiledIn,
        );
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
            crate::runner::RegistrationOrigin::CompiledIn,
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
