//! `ExtensionToolSpecAdapter` — bridges an extension's `ToolDefinition`
//! (framework contract, `codesmith_agent::extension`) onto the host's
//! production `ToolSpec` trait. Mirrors `ToolSpecAdapter`
//! (`framework_adapter.rs:42-87`) which bridges the core `Tool` onto
//! `ToolSpec`.
//!
//! The agent loop sees only `ToolSpec` — it never names `ToolDefinition`
//! or `ExtensionContext`. An `Arc<ExtensionToolSpecAdapter>` is inserted
//! into the host `ToolRegistry` via `ToolRegistry::register` (which funnels
//! through the `build_tool` fail-closed chokepoint — so the adapter's
//! `input_schema()` MUST be object-rooted + `name()` MUST match
//! `^[a-zA-Z0-9_-]{1,64}$`, or the tool is swapped for `FailClosedTool`).

use std::sync::Arc;

use async_trait::async_trait;
use codesmith_agent::extension::{ExtensionContext, ToolDefinition};
use codesmith_extensions::ExtensionRunner;
use codesmith_tools::{ToolCapability, ToolError, ToolResult};
use serde_json::Value;

use super::registry::ToolRegistry;
use super::spec::{ToolContext, ToolSpec};

/// Wrap an extension `ToolDefinition` into a host `ToolSpec`. The bound
/// `ctx` is handed to `ToolDefinition::execute` on each call; the host's
/// `ToolContext` is ignored (extensions stay decoupled from `ToolContext`'s
/// ~30 host-coupled fields — spec §5.1.1).
pub struct ExtensionToolSpecAdapter {
    tool: Arc<dyn ToolDefinition>,
    ctx: Arc<dyn ExtensionContext>,
}

impl ExtensionToolSpecAdapter {
    #[must_use]
    pub fn new(tool: Arc<dyn ToolDefinition>, ctx: Arc<dyn ExtensionContext>) -> Self {
        Self { tool, ctx }
    }
}

#[async_trait]
impl ToolSpec for ExtensionToolSpecAdapter {
    fn name(&self) -> &str {
        self.tool.name()
    }
    fn description(&self) -> &str {
        self.tool.description()
    }
    fn input_schema(&self) -> Value {
        // MUST be object-rooted — `build_tool` rejects non-object roots.
        let schema = self.tool.input_schema();
        if schema.get("type").and_then(|v| v.as_str()) == Some("object") {
            schema
        } else {
            tracing::warn!(
                "extension tool '{}' declared a non-object-rooted input schema; \
                 substituting a bare object schema (declared properties/required dropped)",
                self.tool.name()
            );
            serde_json::json!({ "type": "object" })
        }
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        self.tool.capabilities()
    }
    async fn execute(&self, input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
        match self.tool.execute(input, &*self.ctx).await {
            Ok(result) => Ok(result),
            Err(err) => {
                // Forward the extension error to the agent loop as a normal tool
                // failure. `Tool { message, .. }` forwards the inner message verbatim —
                // `From<ToolError>` leaves `tool` empty, so rendering the wrapper would
                // add "extension tool '' failed:" noise on top of the original text.
                // `StaleContext` renders via its own Display, which tells the model the
                // runtime was invalidated (not worth retrying).
                let message = match err {
                    codesmith_agent::extension::ExtensionError::Tool { message, .. } => message,
                    other => other.to_string(),
                };
                Err(ToolError::execution_failed(message))
            }
        }
    }
}

/// §F5d — register every bound extension tool into the host `ToolRegistry`,
/// each wrapped in an [`ExtensionToolSpecAdapter`]. Called from
/// `EngineHost::build_turn_dispatcher` after plugin-tools are configured.
///
/// Registration scopes (DSH alignment): this resolves the runner's
/// **Global** view ([`ExtensionRunner::bound_tools`]) — an
/// agent-type-scoped tool registration never enters the main-turn
/// catalog (it shadows its global namesake only in that agent type's
/// view, [`ExtensionRunner::bound_tools_scoped`]), and a `Global`
/// `restrict_tools` entry prunes its names here. A sub-agent registry
/// resolves its own view via
/// [`register_extension_tools_scoped`] instead (5b).
///
/// Per-turn rebuild → no persistent host holder; clearing `runner.tools[id]`
/// before the next turn's call suffices. Ext tools mounted here are
/// **main-turn** contributions; which origins may reach a sub-agent
/// registry is the §F5d unload-safety tier's business (see
/// [`register_extension_tools_scoped`]).
///
/// Torn-read note: `bound_context()` and `bound_tools()` lock the runner's
/// context and tools registries separately (no atomic snapshot —
/// `bind_core` swaps the context *before* flushing pending tools). Callers
/// must serialize this call against `bind_core` / reload / re-register;
/// the host's per-turn dispatcher build satisfies this by running on the
/// engine task, off the bind path.
pub fn register_extension_tools(registry: &mut ToolRegistry, runner: &ExtensionRunner) {
    let Some(ctx) = runner.bound_context() else {
        // No bound context yet (pre-bind_core) → nothing to adapt.
        return;
    };
    for (_name, tool) in runner.bound_tools() {
        let adapter = ExtensionToolSpecAdapter::new(tool, ctx.clone());
        registry.register(Arc::new(adapter));
    }
}

/// 5b — mount the **sub-agent view** of extension tools into a child's
/// fresh `ToolRegistry`: [`ExtensionRunner::mountable_tools_for_subagents`]
/// for `scope` (the child's agent type) — the scope's view minus
/// dylib-origin tools (§F5d unload safety: a detached background child
/// can outlive a reload's two-phase `Library` drop, so dylib
/// contributions stay main-turn-only). Called from
/// `SubAgentToolRegistry::new` right after the built-in surface builds,
/// BEFORE the capability-manifest removal and the read-only narrowing —
/// so a disabled name prunes ext tools too (origin-agnostic removal),
/// and an extension tool declaring the `ReadOnly` capability joins an
/// explore child's read-only surface automatically.
///
/// ToolsChange note: the main-turn baseline/diff seam never sees child
/// catalogs (they build on this separate path), so a scoped tool that
/// only exists in agent-type views does not flap the main-turn diff.
pub fn register_extension_tools_scoped(
    registry: &mut ToolRegistry,
    runner: &ExtensionRunner,
    scope: &codesmith_agent::extension::Scope,
) {
    let Some(ctx) = runner.bound_context() else {
        return;
    };
    for (_name, tool) in runner.mountable_tools_for_subagents(scope) {
        let adapter = ExtensionToolSpecAdapter::new(tool, ctx.clone());
        registry.register(Arc::new(adapter));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::ToolRegistry;
    use async_trait::async_trait;
    use codesmith_agent::extension::{
        Extension, ExtensionApi, ExtensionCommandContext, ExtensionContext, ExtensionError,
        ExtensionMetadata, ExtensionMode, ToolDefinition,
    };
    use codesmith_extensions::{ExtensionRunner, HostExtensionContext};
    use codesmith_tools::ToolCapability;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use tokio_util::sync::CancellationToken;

    struct EchoTool;
    #[async_trait]
    impl ToolDefinition for EchoTool {
        fn name(&self) -> &str {
            "echo_ext"
        }
        fn description(&self) -> &str {
            "Echoes the input text."
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: Value,
            _ctx: &dyn ExtensionContext,
        ) -> Result<ToolResult, codesmith_agent::extension::ExtensionError> {
            let text = input
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ToolResult::success(format!("echo:{text}")))
        }
    }

    fn ctx() -> Arc<dyn ExtensionContext> {
        Arc::new(HostExtensionContext::new(
            PathBuf::from("."),
            ExtensionMode::Interactive,
            Arc::new(std::sync::Mutex::new(true)),
            Arc::new(std::sync::Mutex::new(CancellationToken::new())),
            Arc::new(AtomicU64::new(0)),
        ))
    }

    #[tokio::test]
    async fn adapter_executes_extension_tool() {
        let adapter = ExtensionToolSpecAdapter::new(Arc::new(EchoTool), ctx());
        let tc = ToolContext::new(".");
        let out = adapter.execute(json!({"text":"hi"}), &tc).await.unwrap();
        assert!(out.success);
        assert_eq!(out.content, "echo:hi");
    }

    #[test]
    fn adapter_name_and_schema_pass_fail_closed_chokepoint() {
        let adapter = ExtensionToolSpecAdapter::new(Arc::new(EchoTool), ctx());
        assert_eq!(adapter.name(), "echo_ext");
        assert_eq!(
            adapter.input_schema().get("type").and_then(|v| v.as_str()),
            Some("object")
        );
    }

    // === §F5d T1 — register_extension_tools helper =========================

    /// A static (in-process) extension that registers `EchoTool` (defined above
    /// in this test module) so T1's helper test does not require a built dylib.
    struct ToolExt;
    #[async_trait]
    impl Extension for ToolExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("toolext");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.register_tool(Box::new(EchoTool))?;
            Ok(())
        }
    }

    /// `ExtensionCommandContext`-typed ctx for `bind_core` (the runner stores
    /// `Arc<dyn ExtensionCommandContext>`; `bound_context()` upcasts it back to
    /// `Arc<dyn ExtensionContext>` for the adapter). A self-contained test mock
    /// mirroring `Ctx` (runner.rs:370-384) — avoids depending on
    /// `HostExtensionContext::new`'s ctor signature (a cross-crate guess).
    fn ctx_cmd() -> Arc<dyn ExtensionCommandContext> {
        struct CmdCtx {
            generation: u64,
        }
        #[async_trait]
        impl ExtensionContext for CmdCtx {
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
                self.generation
            }
        }
        impl ExtensionCommandContext for CmdCtx {}
        Arc::new(CmdCtx { generation: 1 })
    }

    #[tokio::test]
    async fn register_extension_tools_adapts_bound_tools_into_registry() {
        let runner = ExtensionRunner::new();
        runner.load(&ToolExt).await.expect("load ToolExt");
        runner.bind_core(ctx_cmd());

        let mut registry = ToolRegistry::new(ToolContext::new("."));
        register_extension_tools(&mut registry, &runner);

        assert!(registry.contains("echo_ext"), "adapter registered echo_ext");
        let tool = registry.get("echo_ext").expect("echo_ext present");
        let out = tool
            .execute(serde_json::json!({"text":"hi"}), &ToolContext::new("."))
            .await
            .expect("execute via ToolSpec path");
        // EchoTool (extension.rs:119) returns ToolResult::success("echo:hi");
        // the adapter forwards it as the ToolSpec execute result — mirrors
        // `adapter_executes_extension_tool` at extension.rs:155-162.
        assert!(out.success, "ToolSpec execute succeeds");
        assert_eq!(out.content, "echo:hi");
    }

    #[tokio::test]
    async fn register_extension_tools_before_bind_core_is_noop() {
        let runner = ExtensionRunner::new();
        runner.load(&ToolExt).await.unwrap();
        let mut registry = ToolRegistry::new(ToolContext::new("."));
        register_extension_tools(&mut registry, &runner);
        assert!(!registry.contains("echo_ext"));
    }

    // === DSH registration scope — main-turn catalog resolves the Global view

    /// A fixture tool carrying `(name, tag)` so same-name registrations
    /// under different scopes are distinguishable by `description()`.
    struct NamedTool(&'static str, &'static str);
    #[async_trait]
    impl ToolDefinition for NamedTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            self.1
        }
        async fn execute(
            &self,
            _input: Value,
            _ctx: &dyn ExtensionContext,
        ) -> Result<ToolResult, codesmith_agent::extension::ExtensionError> {
            Ok(ToolResult::success(self.1))
        }
    }

    struct ScopedExt;
    #[async_trait]
    impl Extension for ScopedExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("scopedext");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            use codesmith_agent::extension::Scope;
            // Global + explore-scoped registrations of the SAME name — the
            // scoped one must shadow only the explore view, never the
            // main-turn Global view.
            api.register_tool(Box::new(NamedTool("shared_name", "global-variant")))?;
            api.register_tool_scoped(
                Scope::AgentType("explore".into()),
                Box::new(NamedTool("shared_name", "explore-variant")),
            )?;
            // Explore-only tool — invisible to the main turn.
            api.register_tool_scoped(
                Scope::AgentType("explore".into()),
                Box::new(NamedTool("explore_only", "explore fixture")),
            )?;
            // Global tool, then globally denied — pruned from the main
            // catalog at build time (the model never sees it).
            api.register_tool(Box::new(NamedTool("denied_global", "denied fixture")))?;
            api.restrict_tools(Scope::Global, vec!["denied_global".into()])?;
            // Global tool denied for explore only — stays in the main turn.
            api.register_tool(Box::new(NamedTool(
                "explore_denied",
                "explore-denied fixture",
            )))?;
            api.restrict_tools(
                Scope::AgentType("explore".into()),
                vec!["explore_denied".into()],
            )?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn register_extension_tools_resolves_the_global_view() {
        let runner = ExtensionRunner::new();
        runner.load(&ScopedExt).await.expect("load ScopedExt");
        runner.bind_core(ctx_cmd());

        let mut registry = ToolRegistry::new(ToolContext::new("."));
        register_extension_tools(&mut registry, &runner);

        // The GLOBAL registration of shared_name is what mounts (the
        // explore-scoped shadow stays out of the main turn)…
        assert!(
            registry.contains("shared_name"),
            "global shared_name mounts"
        );
        // …explore-only tools never enter the main catalog…
        assert!(
            !registry.contains("explore_only"),
            "agent-type-scoped tools stay out of the main-turn catalog"
        );
        // …a Global restriction prunes its names from the main catalog…
        assert!(
            !registry.contains("denied_global"),
            "globally restricted tools are pruned at catalog build"
        );
        // …and an agent-type restriction does not reach the main turn.
        assert!(
            registry.contains("explore_denied"),
            "an AgentType-scoped restriction never prunes the Global view"
        );
    }

    #[tokio::test]
    async fn scoped_tool_view_shadows_global_for_that_agent_type() {
        let runner = ExtensionRunner::new();
        runner.load(&ScopedExt).await.expect("load ScopedExt");
        runner.bind_core(ctx_cmd());

        // The explore view: explore_only present, explore_denied pruned
        // (agent-scoped restriction), shared_name resolved to the SCOPED
        // registration (description pins which variant surfaced) and
        // denied_global pruned there too (Global restrictions cover every
        // view).
        let explore = runner.bound_tools_scoped(&codesmith_agent::extension::Scope::AgentType(
            "explore".into(),
        ));
        let names: Vec<&str> = explore.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"explore_only"), "explore view: {names:?}");
        assert!(
            !names.contains(&"explore_denied"),
            "agent-scoped restriction prunes the explore view: {names:?}"
        );
        assert!(
            !names.contains(&"denied_global"),
            "global restriction covers the explore view too: {names:?}"
        );
        let shared = explore
            .iter()
            .find(|(n, _)| n == "shared_name")
            .expect("shared_name present in the explore view");
        assert_eq!(
            shared.1.description(),
            "explore-variant",
            "the scoped registration shadows the global one in its own view"
        );
        // …and the Global view keeps the global variant (no leak in the
        // other direction).
        let global_view = runner.bound_tools();
        let global_shared = global_view
            .iter()
            .find(|(n, _)| n == "shared_name")
            .expect("shared_name present in the global view");
        assert_eq!(global_shared.1.description(), "global-variant");
    }
}
