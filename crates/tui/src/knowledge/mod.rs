//! Knowledge On Demand (KoD) — directory-based memory system.
//!
//! Evolves the single-file `memory.md` MVP into a directory of frontmatter-parsed
//! `.md` files with an entrypoint (`MEMORY.md`) and asynchronous prefetch that
//! surfaces relevant memories into the agent's context each turn.
//!
//! Core data flow:
//! 1. User turn starts → spawn async prefetch (scan + rank via side-query)
//! 2. Turn loop proceeds (streaming, tool execution)
//! 3. After tools complete → collect prefetch results
//! 4. Deduplicate against tool result file paths
//! 5. Enforce budget (max memories per turn, line/byte limits)
//! 6. Inject surfaced memories as `<system-reminder>` before next API call

pub mod age;
pub mod budget;
pub mod dedup;
pub mod entrypoint;
pub mod paths;
pub mod prefetch;
pub mod relevance;
pub mod scan;
pub mod types;
