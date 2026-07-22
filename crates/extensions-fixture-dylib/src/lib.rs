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

use std::sync::Arc;

use async_trait::async_trait;
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
