//! Compatibility shim: frontend-agnostic session types moved to
//! `crate::app_state` when the ratatui `App` was removed.

pub use crate::app_state::{
    AppMode, HuntState, HuntVerdict, InitialInput, ReasoningEffort, SessionState, TuiOptions,
    TurnCacheRecord,
};
