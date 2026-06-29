//! Shim: shell output truncation/summarization lives in the runtime crate.
//!
//! The concrete `summarize_output` / `truncate_with_meta` / `TruncationMeta`
//! implementation was physically moved to
//! `codesmith_agent_runtime::tools::shell_output`. This file re-exports it so
//! historical `crate::tools::shell_output` paths keep resolving.
#![allow(dead_code)]

pub use codesmith_agent_runtime::tools::shell_output::{
    TruncationMeta, summarize_output, truncate_with_meta,
};
