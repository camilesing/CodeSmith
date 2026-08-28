//! Re-export of the LSP diagnostic shape + renderer
//! (canonical home: `codesmith_agent_runtime::lsp_diagnostics`).
//!
//! The diagnostic value types (`Severity`, `Diagnostic`, `DiagnosticBlock`)
//! and the pure renderers (`DiagnosticBlock::render`, `render_blocks`) are
//! terminal-agnostic plain data, so they live in `codesmith-agent-runtime`.
//! The TUI's `LspManager` (which drives LSP server processes) stays here and
//! is trait-erased for the engine via `LspManagerApi`.
// `render_blocks` is consumed by the `lsp::mod` tests; keep it re-exported.
#[allow(unused_imports)]
pub use codesmith_agent_runtime::lsp_diagnostics::{
    Diagnostic, DiagnosticBlock, Severity, render_blocks,
};
