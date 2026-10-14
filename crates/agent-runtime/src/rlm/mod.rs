//! Recursive Language Model (RLM) loop — paper-spec Algorithm 1.
//!
//! Implements Zhang, Kraska & Khattab (arXiv:2512.24601, §2 Algorithm 1):
//!
//! ```text
//! state ← InitREPL(prompt=P)
//! state ← AddFunction(state, sub_RLM)
//! hist ← [Metadata(state)]
//! while True:
//!     code ← LLM(hist)
//!     (state, stdout) ← REPL(state, code)
//!     hist ← hist ∥ code ∥ Metadata(stdout)
//!     if state[Final] is set:
//!         return state[Final]
//! ```
//!
//! Invariants:
//! - `P` is held only as a REPL variable (`context` / `ctx`); never
//!   appears in the root LLM's window.
//! - The root LLM receives small metadata messages — length, preview,
//!   helper list, prior-round summary.
//! - Code rounds and sub-LLM calls travel over a single stdin/stdout
//!   pipe to a long-lived Python subprocess. No HTTP sidecar.

use crate::models::Usage;

pub mod bridge;
pub mod prompt;
pub mod session;
pub mod turn;

pub use bridge::RlmBridge;
pub use prompt::rlm_system_prompt;
pub use turn::{RlmTermination, RlmTurnResult, run_rlm_turn, run_rlm_turn_with_root};

fn add_usage_with_prompt_cache(total: &mut Usage, delta: &Usage) {
    total.input_tokens = total.input_tokens.saturating_add(delta.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(delta.output_tokens);
    total.prompt_cache_hit_tokens =
        add_optional_usage(total.prompt_cache_hit_tokens, delta.prompt_cache_hit_tokens);
    total.prompt_cache_miss_tokens = add_optional_usage(
        total.prompt_cache_miss_tokens,
        delta.prompt_cache_miss_tokens,
    );
    total.reasoning_tokens = add_optional_usage(total.reasoning_tokens, delta.reasoning_tokens);
    total.reasoning_replay_tokens =
        add_optional_usage(total.reasoning_replay_tokens, delta.reasoning_replay_tokens);
    // `server_tool_use` is deliberately NOT accumulated: it is a per-response
    // struct (per server-side tool, e.g. web_search), not a scalar
    // Option<u32>, and summing its fields across heterogeneous responses
    // would invent totals the providers never reported. RLM turns surface
    // the final response's value instead.
}

fn add_optional_usage(total: Option<u32>, delta: Option<u32>) -> Option<u32> {
    match (total, delta) {
        (Some(total), Some(delta)) => Some(total.saturating_add(delta)),
        (None, Some(delta)) => Some(delta),
        (Some(total), None) => Some(total),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_usage_with_prompt_cache_preserves_cache_counts() {
        let mut total = Usage {
            input_tokens: 100,
            output_tokens: 10,
            prompt_cache_hit_tokens: Some(80),
            prompt_cache_miss_tokens: Some(20),
            reasoning_tokens: Some(30),
            reasoning_replay_tokens: Some(5),
            ..Usage::default()
        };
        let delta = Usage {
            input_tokens: 50,
            output_tokens: 5,
            prompt_cache_hit_tokens: Some(30),
            prompt_cache_miss_tokens: Some(20),
            reasoning_tokens: Some(12),
            reasoning_replay_tokens: Some(4),
            ..Usage::default()
        };

        add_usage_with_prompt_cache(&mut total, &delta);

        assert_eq!(total.input_tokens, 150);
        assert_eq!(total.output_tokens, 15);
        assert_eq!(total.prompt_cache_hit_tokens, Some(110));
        assert_eq!(total.prompt_cache_miss_tokens, Some(40));
        assert_eq!(total.reasoning_tokens, Some(42));
        assert_eq!(total.reasoning_replay_tokens, Some(9));
    }

    #[test]
    fn add_optional_usage_covers_asymmetric_arms() {
        assert_eq!(add_optional_usage(None, Some(5)), Some(5));
        assert_eq!(add_optional_usage(Some(5), None), Some(5));
        assert_eq!(add_optional_usage(None, None), None);
        assert_eq!(add_optional_usage(Some(u32::MAX), Some(1)), Some(u32::MAX));
    }
}
