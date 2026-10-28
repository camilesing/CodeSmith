//! Model-writable `workflow` tool — CLI implementation group.
//!
//! - [`engine`]: `PythonWorkflowEngine` — a one-shot kernel on the existing
//!   RLM REPL machinery, plus the RPC bridge that services the injected
//!   `run_agent` / `agents` / `log` helpers through a
//!   [`codesmith_agent_runtime::workflow::SubAgentSpawner`].
//! - [`tool`]: the `workflow` [`ToolSpec`](crate::tools::spec::ToolSpec)
//!   and `CliSubAgentSpawner`, which wraps the same one-shot sub-agent run
//!   path (`agent_run` sync mode) the `agent_open`/`agent_eval`/
//!   `agent_close` family runs on.
//!
//! The seam vocabulary and known limitations live in
//! `codesmith_agent_runtime::workflow`.

pub mod engine;
pub mod tool;
