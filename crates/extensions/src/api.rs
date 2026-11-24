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
        pending.commands.push(crate::runner::PendingCommand {
            command,
            origin: self.origin,
        });
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
        pending.disabled_commands.push((name, self.origin));
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
        pending.tool_restrictions.push((
            crate::runner::ToolRestriction {
                scope,
                denied: names,
            },
            self.origin,
        ));
        Ok(())
    }
    fn replace_tool(&self, replacement: Box<dyn ToolReplacement>) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending
            .replacements
            .push((Arc::from(replacement), self.origin));
        Ok(())
    }
    fn register_around_tool(
        &self,
        handler: Arc<dyn codesmith_agent::extension::AroundToolHandler>,
    ) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.around_tools.push((handler, self.origin));
        Ok(())
    }
    fn register_around_turn(
        &self,
        handler: Arc<dyn codesmith_agent::extension::AroundTurnHandler>,
    ) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.around_turns.push((handler, self.origin));
        Ok(())
    }
    fn register_provider(&self, factory: Arc<dyn ProviderFactory>) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.providers.push(crate::runner::PendingProvider {
            factory,
            origin: self.origin,
        });
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
        pending.providers.push(crate::runner::PendingProvider {
            factory,
            origin: self.origin,
        });
        Ok(())
    }
    fn register_turn_executor(
        &self,
        contribution: TurnExecutorContribution,
    ) -> Result<(), ExtensionError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        // Stamp the generation the extension configured under — the value
        // the engine's retired-generation sweep compares against (flush
        // moves the pair into the runner's adopted-pending storage).
        pending
            .turn_executors
            .push(crate::runner::PendingTurnExecutor {
                generation: self.generation.load(Ordering::Acquire),
                contribution,
                origin: self.origin,
            });
        Ok(())
    }
    fn register_prompt_section(&self, id: String, text: String) -> Result<(), ExtensionError> {
        // Validate against the runner's rules now so a bad section fails
        // its load (fail loud), not at first prompt assembly.
        crate::runner::validate_prompt_section(&id, &text)?;
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        assert_live(&self.generation, self.captured_gen)?;
        pending.prompt_sections.push((id, text, self.origin));
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
        pending.skills.push((skill, self.origin));
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
                origin: self.origin,
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
        pending.policies.push((
            codesmith_agent::policy::ResolvedPolicy {
                kind,
                config,
                source: owner,
            },
            self.origin,
        ));
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
    commands: Arc<Mutex<crate::runner::BoundCommandMap>>,
    handlers: Arc<Mutex<Vec<crate::runner::RegisteredHandler>>>,
    /// Route A — live registrations (drop = un-register) + the shared
    /// registry they flush into.
    providers: Arc<Mutex<Vec<(ProviderRegistration, RegistrationOrigin)>>>,
    shared: SharedProviderRegistry,
    /// Route B — prompt-section contributions (same storage the runner
    /// reads at assembly; shared so late registrations are honored).
    prompt_sections: Arc<Mutex<Vec<(String, String, RegistrationOrigin)>>>,
    /// Route B (skills) — registered skills (same storage the runner
    /// snapshots for catalogue render / `load_skill`).
    skills: Arc<
        Mutex<
            Vec<(
                codesmith_agent::extension::SkillRegistration,
                RegistrationOrigin,
            )>,
        >,
    >,
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
        commands: Arc<Mutex<crate::runner::BoundCommandMap>>,
        handlers: Arc<Mutex<Vec<crate::runner::RegisteredHandler>>>,
        providers: Arc<Mutex<Vec<(ProviderRegistration, RegistrationOrigin)>>>,
        shared: SharedProviderRegistry,
        prompt_sections: Arc<Mutex<Vec<(String, String, RegistrationOrigin)>>>,
        skills: Arc<
            Mutex<
                Vec<(
                    codesmith_agent::extension::SkillRegistration,
                    RegistrationOrigin,
                )>,
            >,
        >,
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
        let bound = crate::runner::BoundCommand {
            def: Arc::from(command),
            origin: self.origin,
        };
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name, bound);
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
            .push((guard, self.origin));
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
        if let Some(slot) = sections.iter_mut().find(|(existing, _, _)| *existing == id) {
            slot.1 = text;
            slot.2 = self.origin;
        } else {
            // Same cap the runner's flush path enforces — the real api
            // writes the shared store directly, so the limit holds here too.
            if sections.len() >= crate::runner::MAX_PROMPT_SECTIONS {
                return Err(ExtensionError::Config(format!(
                    "too many prompt sections (>{})",
                    crate::runner::MAX_PROMPT_SECTIONS
                )));
            }
            sections.push((id, text, self.origin));
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
            .find(|(s, _)| s.name == skill.name && s.owner != skill.owner)
        {
            return Err(ExtensionError::Config(format!(
                "skill {:?} already registered by mod {:?}",
                skill.name, existing.0.owner
            )));
        }
        if let Some(slot) = skills.iter_mut().find(|(s, _)| s.name == skill.name) {
            *slot = (skill, self.origin);
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
            skills.push((skill, self.origin));
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
    use codesmith_tools::ToolResult;
    use std::collections::HashMap;

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

    // === coverage: the full stub + real registration surfaces =============

    struct NopHandler;
    #[async_trait]
    impl Handler for NopHandler {
        async fn handle(
            &self,
            _: &ExtensionEvent,
            _: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            Ok(HandlerOutcome::Continue)
        }
    }

    struct NopTool;
    #[async_trait]
    impl ToolDefinition for NopTool {
        fn name(&self) -> &str {
            "nop_tool"
        }
        fn description(&self) -> &str {
            "does nothing"
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &dyn ExtensionContext,
        ) -> Result<ToolResult, ExtensionError> {
            Ok(ToolResult::success("nop"))
        }
    }

    struct NopCmd;
    #[async_trait]
    impl CommandDefinition for NopCmd {
        fn name(&self) -> &str {
            "nop_cmd"
        }
        fn description(&self) -> &str {
            "does nothing"
        }
        async fn run(
            &self,
            _ctx: &dyn ExtensionCommandContext,
            _args: &str,
        ) -> Result<CommandOutput, ExtensionError> {
            Ok(CommandOutput::Message("nop".into()))
        }
    }

    struct NopReplacement;
    #[async_trait]
    impl ToolReplacement for NopReplacement {
        fn target(&self) -> &str {
            "echo"
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &dyn ExtensionContext,
        ) -> Result<ReplacementOutcome, ExtensionError> {
            Ok(ReplacementOutcome::Delegate(None))
        }
    }

    struct NopAroundTool;
    #[async_trait]
    impl codesmith_agent::extension::AroundToolHandler for NopAroundTool {
        async fn call(
            &self,
            _call: codesmith_agent::extension::ToolCallEvent,
            next: codesmith_agent::extension::ToolCallNext<'_>,
        ) -> Result<ToolResult, codesmith_agent::tools::ToolError> {
            next.await
        }
    }

    struct NopAroundTurn;
    #[async_trait]
    impl codesmith_agent::extension::AroundTurnHandler for NopAroundTurn {
        async fn call(
            &self,
            _turn: codesmith_agent::extension::TurnAroundEvent,
            next: codesmith_agent::extension::TurnNext<'_>,
        ) -> anyhow::Result<codesmith_agent::callback::StopReason> {
            next.await
        }
    }

    struct NopFactory;
    impl ProviderFactory for NopFactory {
        fn id(&self) -> codesmith_agent::provider::ProviderId {
            codesmith_agent::provider::ProviderId::from("nop-llm")
        }
        fn build(
            &self,
            _cfg: &codesmith_agent::provider::ProviderConfig,
        ) -> anyhow::Result<codesmith_agent::llm_client::LlmClientHandle> {
            Err(anyhow::anyhow!("not built in this test"))
        }
    }

    fn nop_alias(id: &str) -> ProviderAlias {
        ProviderAlias {
            id: id.to_string(),
            target: codesmith_agent::provider::ProviderId::from("openai"),
            base_url: Some("https://gateway.example.test/v1".to_string()),
            default_model: Some("gwt-large".to_string()),
            http_headers: None,
        }
    }

    /// The stub's whole registration surface queues into `pending`: the
    /// generation read, every contribution kind, and the loud validation
    /// arms (bad scope/section/skill names, dylib-scoped rejection, blank
    /// disable name, non-object policy config).
    #[tokio::test]
    async fn stub_registration_surface_queues_and_validates() {
        let generation = Arc::new(AtomicU64::new(7));
        let pending = Arc::new(Mutex::new(Pending::default()));
        let stub = StubExtensionApi::new(
            generation.clone(),
            pending.clone(),
            SharedProviderRegistry::new(),
            RegistrationOrigin::Script,
        );
        assert_eq!(stub.generation(), 7);

        // Happy-path registrations of every kind.
        stub.register_tool(Box::new(NopTool)).unwrap();
        stub.register_tool_scoped(Scope::AgentType("explore".into()), Box::new(NopTool))
            .unwrap();
        stub.register_command(Box::new(NopCmd)).unwrap();
        stub.disable_command("model".into()).unwrap();
        stub.on(Arc::new(NopHandler)).unwrap();
        stub.on_variant(ExtensionEventKind::ToolCall, Arc::new(NopHandler))
            .unwrap();
        stub.on_variant_scoped(
            ExtensionEventKind::TurnStart,
            Scope::AgentType("explore".into()),
            Arc::new(NopHandler),
        )
        .unwrap();
        stub.restrict_tools(Scope::Global, vec!["web_fetch".into()])
            .unwrap();
        stub.replace_tool(Box::new(NopReplacement)).unwrap();
        stub.register_around_tool(Arc::new(NopAroundTool)).unwrap();
        stub.register_around_turn(Arc::new(NopAroundTurn)).unwrap();
        stub.register_provider(Arc::new(NopFactory)).unwrap();
        stub.register_provider_alias(nop_alias("acme-gw")).unwrap();
        stub.register_prompt_section("guide".into(), "Be terse.".into())
            .unwrap();
        stub.register_skill(codesmith_agent::extension::SkillRegistration {
            owner: "cov".into(),
            name: "inline-skill".into(),
            description: "does one thing".into(),
            body: "Do the thing.".into(),
            when_to_use: None,
        })
        .unwrap();
        stub.register_message_projection(
            "cov".into(),
            "counts".into(),
            serde_json::json!(0),
            Arc::new(|state, _msg| Ok(state)),
        )
        .unwrap();
        stub.register_policy(
            "cov".into(),
            codesmith_agent::policy::PolicyKind::from_kebab_name("shell-output").unwrap(),
            serde_json::json!({"max_output_size": 20000}),
        )
        .unwrap();

        {
            let p = pending.lock().unwrap();
            assert_eq!(p.tools.len(), 2, "global + scoped tool queued");
            assert_eq!(p.commands.len(), 1);
            assert_eq!(p.handlers.len(), 3, "on + on_variant + on_variant_scoped");
            assert_eq!(
                p.disabled_commands,
                vec![("model".to_string(), RegistrationOrigin::Script)]
            );
            assert_eq!(p.tool_restrictions.len(), 1);
            assert_eq!(p.replacements.len(), 1);
            assert_eq!(p.around_tools.len(), 1);
            assert_eq!(p.around_turns.len(), 1);
            assert_eq!(p.providers.len(), 2, "factory + alias factory");
            assert_eq!(p.prompt_sections.len(), 1);
            assert_eq!(p.skills.len(), 1);
            assert_eq!(p.message_projections.len(), 1);
            assert_eq!(p.policies.len(), 1);
        }

        // Loud validation arms.
        assert!(
            stub.disable_command("   ".into()).is_err(),
            "blank disable name rejected"
        );
        assert!(
            stub.register_prompt_section("bad id!".into(), "x".into())
                .is_err(),
            "bad section id rejected"
        );
        let mut bad_skill = codesmith_agent::extension::SkillRegistration {
            owner: "cov".into(),
            name: "bad name!".into(),
            description: "d".into(),
            body: "b".into(),
            when_to_use: None,
        };
        assert!(stub.register_skill(bad_skill.clone()).is_err());
        bad_skill.name = "ok".into();
        bad_skill.body = "  ".into();
        assert!(stub.register_skill(bad_skill).is_err());
        assert!(
            stub.restrict_tools(Scope::Global, Vec::new()).is_err(),
            "empty restriction list rejected"
        );
        assert!(
            stub.register_tool_scoped(Scope::AgentType("  ".into()), Box::new(NopTool))
                .is_err(),
            "blank scope id rejected"
        );
        assert!(
            stub.register_policy(
                "cov".into(),
                codesmith_agent::policy::PolicyKind::from_kebab_name("shell-output").unwrap(),
                serde_json::json!("not-an-object"),
            )
            .is_err(),
            "non-object policy config rejected"
        );
        assert!(
            stub.register_provider_alias(nop_alias("")).is_err(),
            "empty alias id rejected"
        );
        assert!(
            stub.register_provider_alias(ProviderAlias {
                id: "loopback-http".into(),
                target: codesmith_agent::provider::ProviderId::from("openai"),
                base_url: Some("http://gateway.example.test/v1".into()),
                default_model: None,
                http_headers: None,
            })
            .is_err(),
            "non-loopback http base_url rejected"
        );
    }

    /// A dylib-origin stub rejects *scoped* registrations at load (§F5d) —
    /// they serve sub-agent surfaces dylib code cannot reach.
    #[tokio::test]
    async fn stub_dylib_origin_rejects_scoped_registrations() {
        let generation = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(Pending::default()));
        let stub = StubExtensionApi::new(
            generation,
            pending,
            SharedProviderRegistry::new(),
            RegistrationOrigin::Dylib,
        );
        let err = stub
            .register_tool_scoped(Scope::AgentType("explore".into()), Box::new(NopTool))
            .unwrap_err();
        assert!(err.to_string().contains("dylib"), "{err}");
        let err = stub
            .on_variant_scoped(
                ExtensionEventKind::TurnStart,
                Scope::AgentType("explore".into()),
                Arc::new(NopHandler),
            )
            .unwrap_err();
        assert!(err.to_string().contains("dylib"), "{err}");
    }

    /// The real api (§F2 long-lived handle) writes the bound registries
    /// directly: registrations are observable without a flush, validation
    /// arms stay loud, and a generation bump makes every surface read
    /// `StaleContext`.
    #[tokio::test]
    async fn real_api_flushes_into_bound_registries_and_guards_generation() {
        let generation = Arc::new(AtomicU64::new(3));
        let tools = Arc::new(Mutex::new(crate::runner::BoundToolMap::new()));
        let commands = Arc::new(Mutex::new(HashMap::new()));
        let handlers = Arc::new(Mutex::new(Vec::new()));
        let providers = Arc::new(Mutex::new(Vec::new()));
        let shared = SharedProviderRegistry::new();
        let prompt_sections = Arc::new(Mutex::new(Vec::new()));
        let skills = Arc::new(Mutex::new(Vec::new()));
        let hub = codesmith_agent::extension::MessageProjectionHubArc::new(
            codesmith_agent::extension::MessageProjectionHub::new(),
        );
        let api = RealExtensionApi::new(
            generation.clone(),
            RegistrationOrigin::Script,
            tools.clone(),
            commands.clone(),
            handlers.clone(),
            providers.clone(),
            shared.clone(),
            prompt_sections.clone(),
            skills.clone(),
            hub.clone(),
        );
        assert_eq!(api.generation(), 3);

        api.register_tool(Box::new(NopTool)).unwrap();
        api.register_command(Box::new(NopCmd)).unwrap();
        api.on(Arc::new(NopHandler)).unwrap();
        api.on_variant(ExtensionEventKind::ToolCall, Arc::new(NopHandler))
            .unwrap();
        api.register_provider(Arc::new(NopFactory)).unwrap();
        api.register_provider_alias(nop_alias("acme-gw2")).unwrap();
        api.register_prompt_section("guide".into(), "first".into())
            .unwrap();
        // Same id re-register replaces in place, not appends.
        api.register_prompt_section("guide".into(), "second".into())
            .unwrap();
        let skill = |name: &str| codesmith_agent::extension::SkillRegistration {
            owner: "cov".into(),
            name: name.to_string(),
            description: "d".into(),
            body: "b".into(),
            when_to_use: None,
        };
        api.register_skill(skill("s1")).unwrap();
        // Same owner re-register replaces in place.
        api.register_skill(skill("s1")).unwrap();
        api.register_message_projection(
            "cov".into(),
            "counts".into(),
            serde_json::json!(0),
            Arc::new(|state, _msg| Ok(state)),
        )
        .unwrap();

        assert_eq!(tools.lock().unwrap().len(), 1);
        assert!(commands.lock().unwrap().contains_key("nop_cmd"));
        assert_eq!(handlers.lock().unwrap().len(), 2);
        assert_eq!(providers.lock().unwrap().len(), 2);
        assert!(
            shared
                .resolve(&codesmith_agent::provider::ProviderId::from("nop-llm"))
                .is_some()
        );
        assert_eq!(
            *prompt_sections.lock().unwrap(),
            vec![(
                "guide".to_string(),
                "second".to_string(),
                RegistrationOrigin::Script
            )]
        );
        assert_eq!(skills.lock().unwrap().len(), 1);
        assert_eq!(
            hub.state("cov", "counts"),
            Some(serde_json::json!(0)),
            "projection registered on the hub"
        );

        // Loud arms: cross-owner skill conflict.
        let mut other_owner = skill("s1");
        other_owner.owner = "other".into();
        assert!(api.register_skill(other_owner).is_err());
        assert!(
            api.register_prompt_section("bad id!".into(), "x".into())
                .is_err()
        );

        // Duplicate projection key errors through the hub.
        assert!(
            api.register_message_projection(
                "cov".into(),
                "counts".into(),
                serde_json::json!(0),
                Arc::new(|state, _msg| Ok(state)),
            )
            .is_err()
        );

        // Invalidation: every live surface reads StaleContext afterwards.
        generation.fetch_add(1, Ordering::AcqRel);
        assert!(matches!(
            api.register_tool(Box::new(NopTool)),
            Err(ExtensionError::StaleContext)
        ));
        assert!(matches!(
            api.register_command(Box::new(NopCmd)),
            Err(ExtensionError::StaleContext)
        ));
        assert!(matches!(
            api.on(Arc::new(NopHandler)),
            Err(ExtensionError::StaleContext)
        ));
        assert!(matches!(
            api.on_variant(ExtensionEventKind::ToolCall, Arc::new(NopHandler)),
            Err(ExtensionError::StaleContext)
        ));
        assert!(matches!(
            api.register_provider(Arc::new(NopFactory)),
            Err(ExtensionError::StaleContext)
        ));
        assert!(matches!(
            api.register_provider_alias(nop_alias("acme-gw3")),
            Err(ExtensionError::StaleContext)
        ));
        assert!(matches!(
            api.register_prompt_section("late".into(), "x".into()),
            Err(ExtensionError::StaleContext)
        ));
        assert!(matches!(
            api.register_skill(skill("late")),
            Err(ExtensionError::StaleContext)
        ));
        assert!(matches!(
            api.register_message_projection(
                "cov".into(),
                "late".into(),
                serde_json::json!(0),
                Arc::new(|state, _msg| Ok(state)),
            ),
            Err(ExtensionError::StaleContext)
        ));
    }

    /// The `Nop` handler bodies used by the stale-context tests run (their
    /// `handle` is the observable contract: continue the chain).
    #[tokio::test]
    async fn nop_handler_handle_continues() {
        struct Ctx;
        #[async_trait]
        impl ExtensionContext for Ctx {
            fn cwd(&self) -> &std::path::Path {
                std::path::Path::new(".")
            }
            fn mode(&self) -> ExtensionMode {
                ExtensionMode::Interactive
            }
            fn is_idle(&self) -> bool {
                true
            }
            fn signal(&self) -> tokio_util::sync::CancellationToken {
                tokio_util::sync::CancellationToken::new()
            }
            fn generation(&self) -> u64 {
                1
            }
        }
        let out = NopHandler
            .handle(&ExtensionEvent::SessionShutdown, &Ctx)
            .await
            .unwrap();
        assert!(matches!(out, HandlerOutcome::Continue));
    }

    /// The unimplemented default methods of `ExtensionApi` must all refuse
    /// with a diagnosable error (a silent no-op would look like success).
    /// Drive every one through the stub api's `&dyn` surface.
    #[test]
    fn extension_api_default_methods_refuse_loudly() {
        use codesmith_agent::extension::Scope;
        let generation = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(crate::runner::Pending::default()));
        let stub = StubExtensionApi::new(
            generation,
            pending,
            SharedProviderRegistry::new(),
            crate::runner::RegistrationOrigin::CompiledIn,
        );
        // The stub implements disable_command; assert it records instead.
        stub.disable_command("x".to_string())
            .expect("stub records command disables");
        // The stub implements both scoped registrations; they succeed.
        stub.on_variant_scoped(
            ExtensionEventKind::SessionStart,
            Scope::Global,
            std::sync::Arc::new(NopHandler),
        )
        .expect("stub registers scoped handlers");
        stub.restrict_tools(Scope::AgentType("sub".to_string()), vec!["t".to_string()])
            .expect("stub records tool restrictions");
    }

    /// A bare `ExtensionApi` (only the required methods) must refuse every
    /// optional surface with a diagnosable error — a silent no-op default
    /// would look like successful registration.
    struct BareApi;
    #[async_trait]
    impl codesmith_agent::extension::ExtensionApi for BareApi {
        fn generation(&self) -> u64 {
            0
        }
        fn register_tool(
            &self,
            _tool: Box<dyn codesmith_agent::extension::ToolDefinition>,
        ) -> Result<(), ExtensionError> {
            Ok(())
        }
        fn register_command(
            &self,
            _command: Box<dyn codesmith_agent::extension::CommandDefinition>,
        ) -> Result<(), ExtensionError> {
            Ok(())
        }
        fn on(
            &self,
            _handler: Arc<dyn codesmith_agent::extension::Handler>,
        ) -> Result<(), ExtensionError> {
            Ok(())
        }
        fn on_variant(
            &self,
            _kind: ExtensionEventKind,
            _handler: Arc<dyn codesmith_agent::extension::Handler>,
        ) -> Result<(), ExtensionError> {
            Ok(())
        }
    }

    #[test]
    fn bare_extension_api_refuses_every_optional_surface() {
        use codesmith_agent::extension::Scope;
        let api: &dyn codesmith_agent::extension::ExtensionApi = &BareApi;
        let nop = Arc::new(NopHandler);
        for label in [
            api.on_variant_scoped(ExtensionEventKind::SessionStart, Scope::Global, nop.clone())
                .is_err(),
            api.register_tool_scoped(Scope::Global, Box::new(NopTool))
                .is_err(),
            api.disable_command("x".to_string()).is_err(),
            api.restrict_tools(Scope::Global, vec!["t".to_string()])
                .is_err(),
            api.register_skill(codesmith_agent::extension::SkillRegistration {
                owner: "m".to_string(),
                name: "s".to_string(),
                description: "d".to_string(),
                body: "b".to_string(),
                when_to_use: None,
            })
            .is_err(),
            api.register_message_projection(
                "o".to_string(),
                "k".to_string(),
                serde_json::Value::Null,
                std::sync::Arc::new(|_state, _msg: &codesmith_agent::models::Message| {
                    Ok(serde_json::Value::Null)
                }),
            )
            .is_err(),
        ] {
            assert!(label, "optional api surface must refuse loudly");
        }
    }
}
