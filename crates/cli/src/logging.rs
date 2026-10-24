//! Lightweight verbose logging helpers for the CLI.

use std::sync::atomic::{AtomicBool, Ordering};

use colored::Colorize;

/// Brand sky color (moved from the retired ratatui palette).
const CODESMITH_SKY_RGB: (u8, u8, u8) = (106, 174, 242);

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Enable or disable verbose logging output.
pub fn set_verbose(enabled: bool) {
    VERBOSE.store(enabled, Ordering::SeqCst);
}

/// Return true when `CODESMITH_LOG_LEVEL` requests verbose output.
///
/// Note: `RUST_LOG` is intentionally NOT checked here — the CLI has no
/// `tracing` subscriber; `RUST_LOG` only passes through to spawned child
/// processes and should not gate CLI verbose output.
#[must_use]
pub fn env_requests_verbose_logging() -> bool {
    codesmith_config::codesmith_env("LOG_LEVEL")
        .is_some_and(|value| log_value_enables_verbose(&value))
}

fn log_value_enables_verbose(value: &str) -> bool {
    value.split(',').any(|directive| {
        let directive = directive.trim();
        match directive.rsplit_once('=') {
            Some((_target, level)) => {
                matches!(
                    level.trim().to_ascii_lowercase().as_str(),
                    "trace" | "debug" | "info"
                )
            }
            // A bare directive is either a level (trace/debug/info turn
            // verbose on; warn/error/off do not) or a TARGET name —
            // standard RUST_LOG semantics read `codesmith_cli` as
            // `codesmith_cli=trace`, which enables verbose output.
            None => !matches!(
                directive.to_ascii_lowercase().as_str(),
                "" | "off" | "error" | "warn"
            ),
        }
    })
}

/// Check whether verbose logging is enabled.
#[must_use]
pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::SeqCst)
}

/// Emit a verbose info message (no-op when verbosity is disabled).
pub fn info(message: impl AsRef<str>) {
    if is_verbose() {
        let (r, g, b) = CODESMITH_SKY_RGB;
        eprintln!("{} {}", "info".truecolor(r, g, b).bold(), message.as_ref());
    }
}

/// Emit a verbose warning message (no-op when verbosity is disabled).
pub fn warn(message: impl AsRef<str>) {
    if is_verbose() {
        let (r, g, b) = CODESMITH_SKY_RGB;
        eprintln!("{} {}", "warn".truecolor(r, g, b).bold(), message.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_value_parser_accepts_common_rust_log_directives() {
        assert!(log_value_enables_verbose("debug"));
        assert!(log_value_enables_verbose("codesmith_cli=debug"));
        assert!(log_value_enables_verbose(
            "warn,codesmith_cli::llm_client=trace"
        ));
        assert!(!log_value_enables_verbose("warn"));
        // `codesmith_cli::off` is a bare TARGET path (RUST_LOG needs `=`
        // for a level), so it reads as `codesmith_cli::off=trace` and
        // enables verbose — the old parser silently no-op'd bare targets.
        assert!(log_value_enables_verbose("codesmith_cli::off"));
        // A bare TARGET directive is `target=trace` under RUST_LOG
        // semantics — it must enable verbose output, not silently no-op.
        assert!(log_value_enables_verbose("codesmith_cli"));
        assert!(log_value_enables_verbose("warn,codesmith_cli"));
        assert!(!log_value_enables_verbose("off"));
        assert!(!log_value_enables_verbose(""));
    }
}
