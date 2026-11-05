//! `ReplacementToolSpec` — mounts an extension's [`ToolReplacement`] (DSH
//! pattern d: wrap or substitute an existing tool's execution) onto the
//! host's [`ToolSpec`] contract, and the per-turn apply helper the main
//! turn and sub-agent registries both call after their extension-tool
//! mounts.
//!
//! Safety shape: every metadata method delegates to the **incumbent**
//! (the `Arc<dyn ToolSpec>` captured at apply time) — capabilities, the
//! approval ladder, destructiveness, path questions, concurrency — so a
//! replacement can never relax an approval gate or smuggle a tool into a
//! read-only surface. Only `description` / `input_schema` may be overridden
//! (model-visible catalog; the mod's load path validates object-rooted
//! schemas). `execute` runs the replacement; [`ReplacementOutcome::Delegate`]
//! resolves against the incumbent, so consecutive replacements for one
//! target compose as a chain (the later wraps the earlier), never a loop.
//!
//! Known limitations (v1):
//! - Main-turn MCP tools are catalog-merged, not registry-resident — a
//!   replacement cannot reach them there.
//! - Replacements are Global-scope only (no per-agent-type shadowing); the
//!   script tier has no scoped registration surface.
//! - The replacement callback is synchronous script code bounded by the
//!   mod engine's op cap; a `delegate` runs the incumbent under the turn's
//!   usual cancellation.

use std::sync::Arc;

use async_trait::async_trait;
use codesmith_agent::extension::{ExtensionContext, ReplacementOutcome, ToolReplacement};
use codesmith_extensions::ExtensionRunner;
use codesmith_tools::{ToolCapability, ToolError, ToolResult};
use serde_json::Value;
use std::path::PathBuf;

use super::registry::ToolRegistry;
use super::spec::{ApprovalRequirement, ToolContext, ToolSpec};

/// Wrap a [`ToolReplacement`] + the captured incumbent into a host
/// [`ToolSpec`]. The bound `ctx` is handed to `ToolReplacement::execute`
/// (extensions stay decoupled from `ToolContext`'s host-coupled fields,
/// same rule as [`super::extension::ExtensionToolSpecAdapter`]); the
/// incumbent's `execute` still receives the real per-turn `ToolContext`.
pub struct ReplacementToolSpec {
    original: Arc<dyn ToolSpec>,
    replacement: Arc<dyn ToolReplacement>,
    ctx: Arc<dyn ExtensionContext>,
}

impl ReplacementToolSpec {
    #[must_use]
    pub fn new(
        original: Arc<dyn ToolSpec>,
        replacement: Arc<dyn ToolReplacement>,
        ctx: Arc<dyn ExtensionContext>,
    ) -> Self {
        Self {
            original,
            replacement,
            ctx,
        }
    }

    /// Forward an extension error to the agent loop as a normal tool
    /// failure (same message-forwarding rule as
    /// [`super::extension::ExtensionToolSpecAdapter`]).
    fn extension_error(err: codesmith_agent::extension::ExtensionError) -> ToolError {
        let message = match err {
            codesmith_agent::extension::ExtensionError::Tool { message, .. } => message,
            other => other.to_string(),
        };
        ToolError::execution_failed(message)
    }
}

#[async_trait]
impl ToolSpec for ReplacementToolSpec {
    fn name(&self) -> &str {
        self.original.name()
    }
    fn description(&self) -> &str {
        // The one sanctioned model-visible override.
        self.replacement
            .description_override()
            .unwrap_or_else(|| self.original.description())
    }
    fn input_schema(&self) -> Value {
        // The other sanctioned model-visible override — defensive on the
        // object root even though the script load path already rejects
        // non-object schemas (the registry's fail-closed chokepoint would
        // swap the whole tool for a FailClosedTool otherwise).
        match self.replacement.input_schema_override() {
            Some(schema) if schema.get("type").and_then(|v| v.as_str()) == Some("object") => schema,
            Some(_) => {
                tracing::warn!(
                    "replacement for tool '{}' declared a non-object-rooted schema override; \
                     keeping the incumbent's schema",
                    self.original.name()
                );
                self.original.input_schema()
            }
            None => self.original.input_schema(),
        }
    }
    fn output_schema(&self) -> Value {
        self.original.output_schema()
    }
    fn validate_input(&self, input: &Value, context: &ToolContext) -> Result<(), ToolError> {
        // Without a schema override the model's input still matches the
        // incumbent's contract, so its semantic validation applies. With
        // one, the input speaks the replacement's dialect — the script
        // owns translating before `delegate`, and the incumbent's own
        // validation still guards the delegated input in `execute`.
        if self.replacement.input_schema_override().is_some() {
            Ok(())
        } else {
            self.original.validate_input(input, context)
        }
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        self.original.capabilities()
    }
    fn approval_requirement(&self) -> ApprovalRequirement {
        self.original.approval_requirement()
    }
    fn approval_requirement_for_input(
        &self,
        input: &Value,
        context: &ToolContext,
    ) -> ApprovalRequirement {
        self.original.approval_requirement_for_input(input, context)
    }
    fn escaped_path_for_input(&self, input: &Value, context: &ToolContext) -> Option<PathBuf> {
        self.original.escaped_path_for_input(input, context)
    }
    fn is_destructive(&self, input: &Value) -> bool {
        self.original.is_destructive(input)
    }
    fn is_interactive(&self, input: &Value) -> bool {
        self.original.is_interactive(input)
    }
    fn supports_parallel(&self) -> bool {
        self.original.supports_parallel()
    }
    fn defer_loading(&self) -> bool {
        self.original.defer_loading()
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        match self.replacement.execute(input.clone(), &*self.ctx).await {
            Ok(ReplacementOutcome::Complete(result)) => Ok(result),
            Ok(ReplacementOutcome::Delegate(None)) => self.original.execute(input, context).await,
            Ok(ReplacementOutcome::Delegate(Some(rewritten))) => {
                // The rewritten input speaks the incumbent's dialect — its
                // own semantic gate guards it before it runs.
                self.original.validate_input(&rewritten, context)?;
                self.original.execute(rewritten, context).await
            }
            Err(err) => Err(Self::extension_error(err)),
        }
    }
}

/// Apply every bound tool replacement (the runner's **main-turn** view —
/// [`ExtensionRunner::bound_replacements`]) onto `registry`: for each
/// replacement whose target is registered, capture the incumbent and
/// re-register the wrapped spec. Later replacements wrap earlier ones
/// (chain semantics); a target missing from the current surface is a
/// warning, not an error — surfaces are mode-dependent (a `replace_tool`
/// of `write_file` under Plan mode simply has nothing to wrap this turn).
///
/// Returns the names this call actually replaced (missing targets absent),
/// for origin bookkeeping and eager catalog visibility at the call site.
pub fn apply_extension_tool_replacements(
    registry: &mut ToolRegistry,
    runner: &ExtensionRunner,
) -> Vec<String> {
    let Some(ctx) = runner.bound_context() else {
        // No bound context yet (pre-bind_core) → nothing to mount.
        return Vec::new();
    };
    let mut written = Vec::new();
    for (target, replacement) in runner.bound_replacements() {
        let Some(incumbent) = registry.get(&target) else {
            tracing::warn!(
                target: "codesmith_mods",
                "a mod replaces tool '{target}' but no such tool is registered in this \
                 surface — replacement skipped here (surfaces are mode-dependent)"
            );
            continue;
        };
        registry.register(Arc::new(ReplacementToolSpec::new(
            incumbent,
            replacement,
            ctx.clone(),
        )));
        tracing::info!(
            target: "codesmith_mods",
            "tool '{target}' execution replaced by a mod (metadata preserved except \
             description/schema overrides)"
        );
        written.push(target);
    }
    written
}

/// The sub-agent twin of [`apply_extension_tool_replacements`]: mounts the
/// runner's sub-agent-safe replacement view
/// ([`ExtensionRunner::bound_replacements_for_subagents`] — dylib-origin
/// entries excluded, same §F5d unload-safety rule as scoped tool mounts)
/// onto a child's fresh registry, so parent and child see the same
/// implementation for a replaced tool. Called from
/// `SubAgentToolRegistry::new` after `register_extension_tools_scoped`,
/// before the capability-manifest removal and read-only narrowing.
pub fn apply_extension_tool_replacements_for_subagents(
    registry: &mut ToolRegistry,
    runner: &ExtensionRunner,
) -> Vec<String> {
    let Some(ctx) = runner.bound_context() else {
        return Vec::new();
    };
    let mut written = Vec::new();
    for (target, replacement) in runner.bound_replacements_for_subagents() {
        let Some(incumbent) = registry.get(&target) else {
            // A child's surface is narrower by design (read-only explore
            // children, capability masks) — a missing target is expected,
            // so this is debug-level noise, not a warning.
            tracing::debug!(
                target: "codesmith_mods",
                "mod replacement for tool '{target}' has no target in this sub-agent surface"
            );
            continue;
        };
        registry.register(Arc::new(ReplacementToolSpec::new(
            incumbent,
            replacement,
            ctx.clone(),
        )));
        written.push(target);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::ToolRegistry;
    use async_trait::async_trait;
    use codesmith_agent::extension::{
        Extension, ExtensionApi, ExtensionCommandContext, ExtensionContext, ExtensionError,
        ExtensionMetadata, ExtensionMode,
    };
    use codesmith_extensions::ExtensionRunner;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;
    use tokio_util::sync::CancellationToken;

    /// A builtin stand-in with a write capability (→ `Suggest` approval)
    /// and a semantic gate: `forbidden: true` input is rejected by
    /// `validate_input`, everything else echoes as `builtin:<arg>`.
    struct BuiltinTool {
        inputs: StdMutex<Vec<Value>>,
    }

    #[async_trait]
    impl ToolSpec for BuiltinTool {
        fn name(&self) -> &str {
            "fake_write"
        }
        fn description(&self) -> &str {
            "builtin description"
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object", "properties": {"arg": {"type": "string"}}})
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::WritesFiles]
        }
        fn validate_input(&self, input: &Value, _context: &ToolContext) -> Result<(), ToolError> {
            if input.get("forbidden").and_then(|v| v.as_bool()) == Some(true) {
                Err(ToolError::invalid_input("forbidden input"))
            } else {
                Ok(())
            }
        }
        async fn execute(
            &self,
            input: Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            self.inputs.lock().unwrap().push(input.clone());
            let arg = input.get("arg").and_then(|v| v.as_str()).unwrap_or("");
            Ok(ToolResult::success(format!("builtin:{arg}")))
        }
    }

    /// Scripted replacement behavior for one call.
    enum Behavior {
        Complete(&'static str),
        DelegatePass,
        DelegateRewrite(&'static str),
    }

    struct FakeReplacement {
        target: String,
        behavior: Behavior,
        description_override: Option<String>,
        schema_override: Option<Value>,
    }

    #[async_trait]
    impl ToolReplacement for FakeReplacement {
        fn target(&self) -> &str {
            &self.target
        }
        fn description_override(&self) -> Option<&str> {
            self.description_override.as_deref()
        }
        fn input_schema_override(&self) -> Option<Value> {
            self.schema_override.clone()
        }
        async fn execute(
            &self,
            _input: Value,
            _ctx: &dyn ExtensionContext,
        ) -> Result<ReplacementOutcome, ExtensionError> {
            Ok(match &self.behavior {
                Behavior::Complete(text) => {
                    ReplacementOutcome::Complete(ToolResult::success(*text))
                }
                Behavior::DelegatePass => ReplacementOutcome::Delegate(None),
                Behavior::DelegateRewrite(arg) => {
                    ReplacementOutcome::Delegate(Some(json!({"arg": arg})))
                }
            })
        }
    }

    fn ext_ctx() -> Arc<dyn ExtensionContext> {
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
            fn signal(&self) -> CancellationToken {
                CancellationToken::new()
            }
            fn generation(&self) -> u64 {
                1
            }
        }
        Arc::new(Ctx)
    }

    fn tool_context() -> ToolContext {
        ToolContext::new(".")
    }

    fn spec(behavior: Behavior) -> ReplacementToolSpec {
        let original: Arc<dyn ToolSpec> = Arc::new(BuiltinTool {
            inputs: StdMutex::new(Vec::new()),
        });
        ReplacementToolSpec::new(
            original,
            Arc::new(FakeReplacement {
                target: "fake_write".into(),
                behavior,
                description_override: None,
                schema_override: None,
            }),
            ext_ctx(),
        )
    }

    #[tokio::test]
    async fn metadata_delegates_and_overrides_only_description_schema() {
        // Overrides present: description + schema come from the
        // replacement…
        let with_overrides = ReplacementToolSpec::new(
            Arc::new(BuiltinTool {
                inputs: StdMutex::new(Vec::new()),
            }),
            Arc::new(FakeReplacement {
                target: "fake_write".into(),
                behavior: Behavior::Complete("x"),
                description_override: Some("replaced description".into()),
                schema_override: Some(
                    json!({"type": "object", "properties": {"x": {"type": "integer"}}}),
                ),
            }),
            ext_ctx(),
        );
        assert_eq!(with_overrides.name(), "fake_write");
        assert_eq!(with_overrides.description(), "replaced description");
        assert!(
            with_overrides.input_schema()["properties"]["x"].is_object(),
            "schema override visible"
        );
        // …while the safety metadata stays the incumbent's (WritesFiles →
        // Suggest; is_destructive true).
        assert_eq!(
            with_overrides.approval_requirement(),
            ApprovalRequirement::Suggest
        );
        assert!(with_overrides.is_destructive(&json!({})));

        // No overrides: description/schema also delegate.
        let plain = spec(Behavior::Complete("x"));
        assert_eq!(plain.description(), "builtin description");
        assert!(plain.input_schema()["properties"]["arg"].is_object());
    }

    #[tokio::test]
    async fn non_object_schema_override_falls_back_to_incumbent() {
        let s = ReplacementToolSpec::new(
            Arc::new(BuiltinTool {
                inputs: StdMutex::new(Vec::new()),
            }),
            Arc::new(FakeReplacement {
                target: "fake_write".into(),
                behavior: Behavior::Complete("x"),
                description_override: None,
                schema_override: Some(json!({"type": "string"})),
            }),
            ext_ctx(),
        );
        assert!(
            s.input_schema()["properties"]["arg"].is_object(),
            "incumbent schema kept on a non-object override"
        );
    }

    #[tokio::test]
    async fn complete_substitutes_and_delegate_paths_run_incumbent() {
        let tc = tool_context();
        // Complete: the incumbent never runs.
        let out = spec(Behavior::Complete("from-script"))
            .execute(json!({"arg": "a"}), &tc)
            .await
            .unwrap();
        assert_eq!(out.content, "from-script");

        // Delegate(None): incumbent runs with the original input.
        let out = spec(Behavior::DelegatePass)
            .execute(json!({"arg": "a"}), &tc)
            .await
            .unwrap();
        assert_eq!(out.content, "builtin:a");

        // Delegate(Some): incumbent runs with the rewritten input.
        let out = spec(Behavior::DelegateRewrite("rewritten"))
            .execute(json!({"arg": "a"}), &tc)
            .await
            .unwrap();
        assert_eq!(out.content, "builtin:rewritten");
    }

    #[tokio::test]
    async fn rewritten_delegate_input_passes_incumbent_validation() {
        // A rewritten input the incumbent's semantic gate rejects fails
        // before the incumbent runs — reached through execute by using a
        // replacement that rewrites to a forbidden input.
        struct ForbiddenRewrite;
        #[async_trait]
        impl ToolReplacement for ForbiddenRewrite {
            fn target(&self) -> &str {
                "fake_write"
            }
            async fn execute(
                &self,
                _input: Value,
                _ctx: &dyn ExtensionContext,
            ) -> Result<ReplacementOutcome, ExtensionError> {
                Ok(ReplacementOutcome::Delegate(Some(
                    json!({"forbidden": true}),
                )))
            }
        }
        let s = ReplacementToolSpec::new(
            Arc::new(BuiltinTool {
                inputs: StdMutex::new(Vec::new()),
            }),
            Arc::new(ForbiddenRewrite),
            ext_ctx(),
        );
        let out = s.execute(json!({"arg": "a"}), &tool_context()).await;
        assert!(
            out.is_err(),
            "forbidden rewrite must fail the incumbent's gate"
        );
    }

    #[tokio::test]
    async fn validate_input_delegates_without_schema_override_only() {
        let tc = tool_context();
        let plain = spec(Behavior::Complete("x"));
        assert!(
            plain
                .validate_input(&json!({"forbidden": true}), &tc)
                .is_err(),
            "incumbent's semantic gate applies without a schema override"
        );
        let overridden = ReplacementToolSpec::new(
            Arc::new(BuiltinTool {
                inputs: StdMutex::new(Vec::new()),
            }),
            Arc::new(FakeReplacement {
                target: "fake_write".into(),
                behavior: Behavior::Complete("x"),
                description_override: None,
                schema_override: Some(json!({"type": "object"})),
            }),
            ext_ctx(),
        );
        assert!(
            overridden
                .validate_input(&json!({"forbidden": true}), &tc)
                .is_ok(),
            "with a schema override the script owns the dialect at dispatch"
        );
    }

    // === apply_extension_tool_replacements =================================

    /// An in-process extension registering one replacement at configure —
    /// rides the same stub→flush path a Rhai mod's registration takes.
    struct ReplaceExt(Arc<FakeReplacement>);

    #[async_trait]
    impl Extension for ReplaceExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("replaceext");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.replace_tool(Box::new(CloneReplacement(self.0.clone())))
        }
    }

    /// `FakeReplacement` is not `Clone`-shaped for `Box`; wrap the Arc.
    struct CloneReplacement(Arc<FakeReplacement>);

    #[async_trait]
    impl ToolReplacement for CloneReplacement {
        fn target(&self) -> &str {
            self.0.target()
        }
        fn description_override(&self) -> Option<&str> {
            self.0.description_override()
        }
        fn input_schema_override(&self) -> Option<Value> {
            self.0.input_schema_override()
        }
        async fn execute(
            &self,
            input: Value,
            ctx: &dyn ExtensionContext,
        ) -> Result<ReplacementOutcome, ExtensionError> {
            ToolReplacement::execute(&*self.0, input, ctx).await
        }
    }

    fn cmd_ctx() -> Arc<dyn ExtensionCommandContext> {
        struct CmdCtx;
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
            fn signal(&self) -> CancellationToken {
                CancellationToken::new()
            }
            fn generation(&self) -> u64 {
                1
            }
        }
        impl ExtensionCommandContext for CmdCtx {}
        Arc::new(CmdCtx)
    }

    fn registry_with_builtin() -> ToolRegistry {
        let mut registry = ToolRegistry::new(tool_context());
        registry.register(Arc::new(BuiltinTool {
            inputs: StdMutex::new(Vec::new()),
        }));
        registry
    }

    #[tokio::test]
    async fn apply_wraps_registered_target_and_skips_missing() {
        let runner = ExtensionRunner::new();
        runner
            .load(&ReplaceExt(Arc::new(FakeReplacement {
                target: "fake_write".into(),
                behavior: Behavior::Complete("scripted"),
                description_override: None,
                schema_override: None,
            })))
            .await
            .expect("load ReplaceExt");
        runner.bind_core(cmd_ctx());

        let mut registry = registry_with_builtin();
        let written = apply_extension_tool_replacements(&mut registry, &runner);
        assert_eq!(written, vec!["fake_write".to_string()]);

        let tc = tool_context();
        let out = registry
            .get("fake_write")
            .unwrap()
            .execute(json!({"arg": "a"}), &tc)
            .await
            .unwrap();
        assert_eq!(out.content, "scripted");
        // Safety metadata survived the wrap.
        assert_eq!(
            registry.get("fake_write").unwrap().approval_requirement(),
            ApprovalRequirement::Suggest
        );

        // A replacement whose target is absent is skipped, not an error.
        let runner2 = ExtensionRunner::new();
        runner2
            .load(&ReplaceExt(Arc::new(FakeReplacement {
                target: "no_such_tool".into(),
                behavior: Behavior::Complete("x"),
                description_override: None,
                schema_override: None,
            })))
            .await
            .unwrap();
        runner2.bind_core(cmd_ctx());
        let mut registry2 = registry_with_builtin();
        let written2 = apply_extension_tool_replacements(&mut registry2, &runner2);
        assert!(written2.is_empty(), "missing target skipped: {written2:?}");
    }

    #[tokio::test]
    async fn two_replacements_chain_later_wraps_earlier() {
        let runner = ExtensionRunner::new();
        // Inner: always delegate to the incumbent.
        runner
            .load(&ReplaceExt(Arc::new(FakeReplacement {
                target: "fake_write".into(),
                behavior: Behavior::DelegatePass,
                description_override: None,
                schema_override: None,
            })))
            .await
            .unwrap();
        // Outer: rewrite the input, then delegate.
        runner
            .load(&ReplaceExt(Arc::new(FakeReplacement {
                target: "fake_write".into(),
                behavior: Behavior::DelegateRewrite("outer"),
                description_override: None,
                schema_override: None,
            })))
            .await
            .unwrap();
        runner.bind_core(cmd_ctx());

        let mut registry = registry_with_builtin();
        let written = apply_extension_tool_replacements(&mut registry, &runner);
        assert_eq!(written.len(), 2, "both replacements applied in order");

        // Outer (rewrites to arg="outer") → inner (delegates verbatim) →
        // builtin executes with the rewritten arg. No loop, chain order
        // holds.
        let out = registry
            .get("fake_write")
            .unwrap()
            .execute(json!({"arg": "a"}), &tool_context())
            .await
            .unwrap();
        assert_eq!(out.content, "builtin:outer");
    }
}
