//! AST-based semantic facts for shell commands (tree-sitter-bash).
//!
//! The command-safety gate ("fourth wall") parses every shell command into a
//! bash syntax tree instead of hand-rolled string scanning. This module owns
//! that translation: it reconstructs per-segment argv, redirections, heredocs,
//! and substitution/expansion sites from the tree.
//!
//! Parsing is **fail-closed**: tree-sitter is error-tolerant and will happily
//! produce a partial tree for malformed input. A tree containing `ERROR` or
//! `MISSING` nodes yields [`AstParseFailure`], which callers must treat as
//! "unparseable → require approval", never as "looks safe".

pub mod facts;
pub mod walker;

pub use facts::{AstParseFailure, BashFacts, CommandSegment, Redirect, SegmentJoin};
