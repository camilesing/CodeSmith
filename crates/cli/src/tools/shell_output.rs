//! Shim: shell output truncation/summarization lives in the runtime crate.
//!
//! The concrete `summarize_output` / `truncate_with_meta` / `TruncationMeta`
//! implementation was physically moved to
//! `codesmith_agent_runtime::tools::shell_output`. This file re-exports it so
//! historical `crate::tools::shell_output` paths keep resolving.
// Path-compatibility re-export: nothing inside the crate uses it yet.
#[allow(unused_imports)]
pub use codesmith_agent_runtime::tools::shell_output::{
    TruncationMeta, summarize_output, truncate_with_meta,
};
