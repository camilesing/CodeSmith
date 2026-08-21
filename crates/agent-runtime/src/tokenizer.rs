//! Central token counting.
//!
//! Every budget heuristic in the engine (compaction triggers, capacity
//! preflight, large-output routing, truncation thresholds) needs a token
//! count. Historically each site divided characters by 3 (or bytes by 4),
//! which is a poor approximation for CJK-heavy text and drifts badly on
//! JSON-heavy tool output. This module provides one [`TokenCounter`] with
//! two implementations:
//!
//! - [`TokenCounter::Heuristic`] — the historical `chars.div_ceil(3)`
//!   estimate. Always available; the default. Conservative by design.
//! - `TokenCounter::Hf` (feature `hf-tokenizer`) — an exact count from a
//!   HuggingFace `tokenizer.json` (BPE/Unigram) loaded from
//!   `[context].tokenizer_path`.
//!
//! The counter is process-global ([`set_default`] / [`default_counter`]):
//! the estimate helpers in `compaction` and `tools::large_output_router`
//! are free functions without a config handle, and one tokenizer per
//! process matches how the binary actually runs (one user, one provider
//! config). The host sets it once at startup; the first set wins and later
//! attempts log a warning instead of swapping counters mid-session, which
//! would make budgets inconsistent across a conversation.

use std::sync::{Arc, OnceLock};

/// Token counter with pluggable backends.
#[derive(Clone)]
pub enum TokenCounter {
    /// `chars.div_ceil(3)` — the historical conservative estimate.
    Heuristic,
    /// Exact counts from a loaded HuggingFace tokenizer (feature
    /// `hf-tokenizer`).
    #[cfg(feature = "hf-tokenizer")]
    Hf(Arc<tokenizers::Tokenizer>),
}

impl TokenCounter {
    /// Count tokens in `text`.
    pub fn count_text(&self, text: &str) -> usize {
        match self {
            Self::Heuristic => text.chars().count().div_ceil(3),
            #[cfg(feature = "hf-tokenizer")]
            Self::Hf(tokenizer) => tokenizer
                .encode(text, false)
                .map(|encoding| encoding.get_ids().len())
                .unwrap_or_else(|err| {
                    tracing::warn!(
                        "tokenizer encode failed ({err}); falling back to heuristic count"
                    );
                    text.chars().count().div_ceil(3)
                }),
        }
    }

    /// Load a HuggingFace `tokenizer.json` from `path`.
    ///
    /// Returns `Err` with a human-readable message when the feature is
    /// disabled at compile time or the file fails to load, so hosts can
    /// surface a clear diagnostic and keep the heuristic fallback.
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        #[cfg(not(feature = "hf-tokenizer"))]
        {
            let _ = path;
            Err("hf-tokenizer feature is not compiled in; rebuild with the \
                 `hf-tokenizer` feature to use tokenizer_path"
                .to_string())
        }
        #[cfg(feature = "hf-tokenizer")]
        {
            let tokenizer = tokenizers::Tokenizer::from_file(path)
                .map_err(|e| format!("failed to load tokenizer {}: {e}", path.display()))?;
            Ok(Self::Hf(Arc::new(tokenizer)))
        }
    }
}

impl std::fmt::Debug for TokenCounter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Heuristic => f.write_str("TokenCounter::Heuristic"),
            #[cfg(feature = "hf-tokenizer")]
            Self::Hf(_) => f.write_str("TokenCounter::Hf(..)"),
        }
    }
}

static DEFAULT: OnceLock<TokenCounter> = OnceLock::new();

/// The process-wide default counter (heuristic until [`set_default`]).
pub fn default_counter() -> TokenCounter {
    DEFAULT.get().cloned().unwrap_or(TokenCounter::Heuristic)
}

/// Install the process-wide counter. The first call wins; later calls warn
/// and are ignored so budgets stay consistent across a session.
pub fn set_default(counter: TokenCounter) {
    if DEFAULT.set(counter).is_err() {
        tracing::warn!("token counter already installed; ignoring re-install");
    }
}

/// Convenience for hosts: load from `path` when `Some`, erroring (and
/// keeping the heuristic default) on failure.
pub fn init_from_path(path: Option<&std::path::Path>) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    match TokenCounter::from_file(path) {
        Ok(counter) => {
            set_default(counter);
            Ok(())
        }
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_matches_div_ceil_three() {
        let counter = TokenCounter::Heuristic;
        assert_eq!(counter.count_text(""), 0);
        assert_eq!(counter.count_text("123456789"), 3);
        assert_eq!(counter.count_text("1234567890"), 4);
        // CJK chars count as chars, not bytes.
        assert_eq!(counter.count_text("冰糖葫芦"), 2);
    }

    #[test]
    fn default_counter_is_heuristic_in_tests() {
        // Tests must never observe a globally-installed HF tokenizer:
        // budget math all over the suite assumes the heuristic.
        assert!(matches!(default_counter(), TokenCounter::Heuristic));
    }

    #[test]
    fn from_file_missing_file_is_an_error() {
        let result = TokenCounter::from_file(std::path::Path::new(
            "/nonexistent/tokenizer-does-not-exist.json",
        ));
        // Without the feature this is the feature-disabled error; with it,
        // a load failure. Either way it must NOT panic.
        assert!(result.is_err());
    }

    #[test]
    fn init_from_path_none_is_ok() {
        assert!(init_from_path(None).is_ok());
    }

    /// Real-file smoke test. Set `CODESMITH_TEST_TOKENIZER` to a
    /// tokenizer.json path to run:
    /// `cargo test -p codesmith-agent-runtime --features hf-tokenizer hf_counter -- --ignored`
    #[test]
    #[ignore = "opt-in: requires a local tokenizer.json (CODESMITH_TEST_TOKENIZER)"]
    fn hf_counter_encodes_a_real_tokenizer_file() {
        let Ok(path) = std::env::var("CODESMITH_TEST_TOKENIZER") else {
            return;
        };
        let counter = TokenCounter::from_file(std::path::Path::new(&path)).expect("load");
        let tokens = counter.count_text("hello world");
        assert!(tokens > 0 && tokens < 20, "unexpected count: {tokens}");
    }
}
