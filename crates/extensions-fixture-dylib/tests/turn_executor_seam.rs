//! The turn-executor seam, exercised from *outside* `codesmith-agent-runtime`.
//!
//! This crate is the stand-in for an embedded host: it links the framework
//! crates the way any downstream crate would and proves the public
//! registration path end to end — implement
//! [`TurnExecutorFactory`](codesmith_agent_runtime::engine::TurnExecutorFactory),
//! register on the process-shared registry, pass startup validation,
//! resolve, and un-register with the RAII guard. The executor itself is a
//! no-op loop (returns `NoToolCalls` without touching the LLM); what is
//! under test is the seam, not a real loop.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use codesmith_agent::callback::StopReason;
use codesmith_agent::executor::AgentExecutor;
use codesmith_agent::memory::ChatHistory;
use codesmith_agent_runtime::engine::{
    BuiltTurn, TurnDeps, TurnExecutorFactory, shared_turn_executors, validate_turn_executor_id,
};

const FIXTURE_LOOP_ID: &str = "fixture-host-loop";

struct FixtureLoopFactory;

impl TurnExecutorFactory for FixtureLoopFactory {
    fn id(&self) -> &'static str {
        FIXTURE_LOOP_ID
    }

    fn build(&self, _deps: TurnDeps) -> BuiltTurn {
        // A minimal but *real* loop shape: an executor + a default-owing
        // harvest. Dropping `deps` unused is legal — a factory takes what it
        // needs.
        struct NoopLoop;
        impl AgentExecutor for NoopLoop {
            fn run<'a>(
                &'a self,
                _history: &'a mut dyn ChatHistory,
                _user_text: String,
            ) -> Pin<Box<dyn Future<Output = anyhow::Result<StopReason>> + Send + 'a>> {
                Box::pin(async { Ok(StopReason::NoToolCalls) })
            }
        }
        struct NoopHarvest;
        impl codesmith_agent_runtime::engine::TurnHarvest for NoopHarvest {}
        BuiltTurn {
            executor: Arc::new(NoopLoop),
            harvest: Arc::new(NoopHarvest),
        }
    }
}

#[test]
fn host_crate_registers_resolves_and_unregisters_a_turn_executor() {
    // Before registration the id fails startup validation loudly…
    let miss = validate_turn_executor_id(FIXTURE_LOOP_ID)
        .expect_err("unregistered id must fail validation");
    assert!(miss.contains(FIXTURE_LOOP_ID), "names the id: {miss}");

    // …registration is a plain call from this crate…
    let registry = shared_turn_executors();
    let guard = registry
        .register(Arc::new(FixtureLoopFactory))
        .expect("a first registration is never refused");

    // …and the id now resolves and validates.
    let factory = registry
        .resolve(FIXTURE_LOOP_ID)
        .expect("registered factory resolves");
    assert_eq!(factory.id(), FIXTURE_LOOP_ID);
    validate_turn_executor_id(FIXTURE_LOOP_ID).expect("registered id validates");

    // Dropping the guard un-registers (identity-checked)…
    drop(guard);
    assert!(
        registry.resolve(FIXTURE_LOOP_ID).is_none(),
        "guard drop un-registers"
    );
    assert!(
        validate_turn_executor_id(FIXTURE_LOOP_ID).is_err(),
        "validation flips back after un-registration"
    );
}
