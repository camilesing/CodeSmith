//! Knowledge On Demand (KoD) — directory-based memory system.
//!
//! Evolves the single-file `memory.md` MVP into a directory of frontmatter-parsed
//! `.md` files with an entrypoint (`MEMORY.md`).
//!
//! What is live: `memory::compose_kod_block` puts the `MEMORY.md` entrypoint
//! block in the system prompt, the `remember` tool writes typed memory files,
//! and `knowledge_recall` searches them when the model asks.
//!
//! Known limitation — not wired: the per-turn pipeline the `prefetch`,
//! `relevance`, `dedup`, `budget` and `curator` modules implement (scan and
//! rank in the background, collect after the tools, deduplicate against tool
//! result paths, inject surfaced memories as `<system-reminder>`) has no
//! caller in the turn loop. `Engine::kod_prefetch_spawn` / `collect` exist
//! but are never invoked, so nothing is surfaced automatically.

pub mod age;
pub mod budget;
pub mod curator;
pub mod dedup;
pub mod entrypoint;
pub mod paths;
pub mod prefetch;
pub mod relevance;
pub mod scan;
pub mod types;
