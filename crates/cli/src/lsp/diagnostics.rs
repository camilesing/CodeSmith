//! Re-export of the LSP diagnostic shape + renderer
//! (canonical home: `codesmith_agent_runtime::lsp_diagnostics`).
//!
//! The diagnostic value types (`Severity`, `Diagnostic`, `DiagnosticBlock`)
//! and the pure renderers (`DiagnosticBlock::render`, `render_blocks`) are
//! terminal-agnostic plain data, so they live in `codesmith-agent-runtime`.
//! The TUI's `LspManager` (which drives LSP server processes) stays in this
//! crate's `lsp` module (`lsp/mod.rs`) and is trait-erased for the engine
//! via `LspManagerApi`.
pub use codesmith_agent_runtime::lsp_diagnostics::{Diagnostic, DiagnosticBlock, Severity};
// Only `lsp::mod`'s test module consumes this re-export today; the cfg gate
// keeps the non-test bin pass from flagging it as an unused import.
#[cfg(test)]
pub use codesmith_agent_runtime::lsp_diagnostics::render_blocks;
