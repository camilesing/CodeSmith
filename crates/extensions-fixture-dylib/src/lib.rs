//! §F5b test fixture: a cdylib exporting `codesmith_register_extension`
//! returning a `Box<dyn Extension>` that registers a tool + a `TurnStart`
//! handler. Loaded by `codesmith-extensions` tests to prove the dylib load
//! path (lockstep §8.2: same workspace + toolchain → vtable + global-allocator
//! match; the host reclaims the Box via `Box::from_raw`).
//!
//! `crate-type = ["cdylib","rlib"]` — the cdylib is the on-disk artifact the
//! loader reads; the rlib lets `codesmith-extensions` dev-depend on it so
//! `cargo test --lib` builds the cdylib into `<target>/<profile>/` (build.rs
//! computes that path; no cargo subprocess, no target-dir lock).
//!
//! Handler dispatch is observed **host-side** via the runner's `EmitOutcome`
//! (the `TurnStart` handler transforms `turn_id` → `fixture:<id>`). This avoids
//! the cdylib/rlib static-duplication trap: a `pub static` here would live at a
//! different address in the dlopen'd cdylib than in the test binary's rlib
//! copy, so the test would read 0. The transform is returned to the host
//! through the runner's handler chain, so no shared memory is needed.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use codesmith_agent::callback::StopReason;
use codesmith_agent::executor::AgentExecutor;
use codesmith_agent::extension::*;
use codesmith_tools::{ToolCapability, ToolResult};
use serde_json::Value;

pub struct FixtureExtension;

#[async_trait]
impl Extension for FixtureExtension {
    fn metadata(&self) -> &ExtensionMetadata {
        static M: ExtensionMetadata = ExtensionMetadata::new("fixture-dylib");
        &M
    }
    async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
        api.register_tool(Box::new(FixtureEchoTool))?;
        api.on(Arc::new(FixtureTurnStartHandler))?;
        // The dylib-tier turn-executor seam: a from-scratch loop, handed to
        // the host through the api object (a dylib cannot write host-side
        // registry statics — its copy of any crate's statics is private to
        // the dlopen'd image, so the contribution must flow through host
        // memory). The engine adopts it onto the process-shared registry at
        // the next turn's assembly and sweeps it when this extension's
        // generation retires, before the library unmaps.
        api.register_turn_executor(codesmith_agent_runtime::engine::contribute_turn_executor(
            Arc::new(FixtureLoopFactory),
        ))?;
        Ok(())
    }
}

pub struct FixtureEchoTool;

#[async_trait]
impl ToolDefinition for FixtureEchoTool {
    fn name(&self) -> &str {
        "fixture_echo"
    }
    fn description(&self) -> &str {
        "Fixture echo tool."
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }
    async fn execute(
        &self,
        input: Value,
        _ctx: &dyn ExtensionContext,
    ) -> Result<ToolResult, ExtensionError> {
        let text = input
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(ToolResult::success(format!("fixture:{text}")))
    }
}

pub struct FixtureTurnStartHandler;

#[async_trait]
impl Handler for FixtureTurnStartHandler {
    async fn handle(
        &self,
        event: &ExtensionEvent,
        _ctx: &dyn ExtensionContext,
    ) -> Result<HandlerOutcome, ExtensionError> {
        if let ExtensionEvent::TurnStart { turn_id } = event {
            Ok(HandlerOutcome::Transform(ExtensionEvent::TurnStart {
                turn_id: format!("fixture:{turn_id}"),
            }))
        } else {
            Ok(HandlerOutcome::Continue)
        }
    }
}

/// The loop the fixture contributes through the dylib-tier executor seam:
/// a no-op executor (answers `NoToolCalls` without an LLM call) + a
/// defaults-only harvest. Never selected by config in tests — it exists to
/// prove the seam's registration and generation-swept lifetime.
pub struct FixtureLoopFactory;

/// The fixture's registered executor id (also asserted by
/// `codesmith-extensions` tests).
pub const FIXTURE_LOOP_ID: &str = "fixture-loop";

impl codesmith_agent_runtime::engine::TurnExecutorFactory for FixtureLoopFactory {
    fn id(&self) -> &'static str {
        FIXTURE_LOOP_ID
    }
    fn build(
        &self,
        _deps: codesmith_agent_runtime::engine::TurnDeps,
    ) -> codesmith_agent_runtime::engine::BuiltTurn {
        struct NeverRuns;
        impl AgentExecutor for NeverRuns {
            fn run<'a>(
                &'a self,
                _history: &'a mut dyn codesmith_agent::memory::ChatHistory,
                _user_text: String,
            ) -> Pin<Box<dyn Future<Output = anyhow::Result<StopReason>> + Send + 'a>> {
                Box::pin(async { Ok(StopReason::NoToolCalls) })
            }
        }
        struct NoHarvest;
        impl codesmith_agent_runtime::engine::TurnHarvest for NoHarvest {}
        codesmith_agent_runtime::engine::BuiltTurn {
            executor: Arc::new(NeverRuns),
            harvest: Arc::new(NoHarvest),
        }
    }
}

/// C-ABI entry the host loader looks up. Returns a `Box<dyn Extension>` the
/// host reclaims via `Box::from_raw` (lockstep: same global allocator). The
/// `*mut FixtureExtension` → `*mut dyn Extension` unsizing coercion happens at
/// the return coercion site.
/// `*mut dyn Extension` is a fat pointer (data + vtable) crossing the C
/// boundary — `improper_ctypes_definitions` flags it because trait objects
/// have no stable C ABI. This is the §8.2 **lockstep** tradeoff (same
/// compiler + `codesmith-agent` version on both sides → fat-pointer repr +
/// vtable match); no `abi_stable` (§2.4 — same trait, no ABI churn).
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn codesmith_register_extension() -> *mut dyn Extension {
    Box::into_raw(Box::new(FixtureExtension))
}
