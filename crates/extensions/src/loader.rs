//! Phase-2 dylib loader (spec §F5b / §7.2 / §8.2). Loads a `cdylib` from
//! disk, looks up the `codesmith_register_extension` symbol, and returns
//! the `Library` + a `Box<dyn Extension>` constructed by the dylib.
//!
//! # Safety / lockstep (§8.2)
//!
//! `*mut dyn Extension` is a fat pointer (data + vtable) returned across
//! an `extern "C"` boundary. Its representation is stable **under
//! lockstep** — same compiler + same `codesmith-agent` version (same
//! `std`/allocator) on both sides — which the build enforces. The host
//! reclaims ownership via `Box::from_raw`; dropping the `Box` after
//! `configure` is sound because registered contributions are
//! self-contained owned trait objects whose vtables live in the
//! (kept-alive) `Library`. **No `abi_stable`** (§2.4 — same trait, no ABI
//! churn).

use std::path::Path;

use codesmith_agent::extension::{Extension, ExtensionError};
use libloading::{Library, Symbol};

/// The symbol a dylib must export:
/// `#[no_mangle] pub extern "C" fn codesmith_register_extension() -> *mut dyn Extension`.
pub const REGISTER_SYMBOL: &[u8] = b"codesmith_register_extension";

/// Load a dylib + construct its `Extension`. Returns the `Library` (which
/// the caller MUST keep alive for as long as any registered contribution's
/// vtable is reachable) and the `Box<dyn Extension>` (consumed by
/// `ExtensionRunner::load` during `configure`, then dropped). Errors →
/// [`ExtensionError::Load`] (open / symbol lookup / null return).
pub fn load_dylib(path: &Path) -> Result<(Library, Box<dyn Extension>), ExtensionError> {
    let library = unsafe { Library::new(path) }
        .map_err(|e| ExtensionError::Load(format!("open dylib {path:?}: {e}")))?;
    let register: Symbol<unsafe extern "C" fn() -> *mut dyn Extension> = unsafe {
        library.get(REGISTER_SYMBOL)
    }
    .map_err(|e| ExtensionError::Load(format!("symbol {path:?}::{REGISTER_SYMBOL:?}: {e}")))?;
    let ptr = unsafe { register() };
    if ptr.is_null() {
        return Err(ExtensionError::Load(format!(
            "{path:?}::{REGISTER_SYMBOL:?} returned null"
        )));
    }
    // SAFETY: lockstep (§8.2) — the dylib allocated this `Box` with the
    // same global allocator as the host (same compiler + codesmith-agent
    // version). Fat-pointer return representation matches under lockstep.
    let extension = unsafe { Box::from_raw(ptr) };
    Ok((library, extension))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use codesmith_agent::extension::*;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    // `bind_core` holds `Arc<dyn ExtensionCommandContext>`; the test Ctx must
    // impl the sub-trait (a marker in slice 1) for the coercion to fire —
    // mirrors `crates/extensions/src/runner.rs:370-384`.
    struct Ctx {
        generation: u64,
    }
    #[async_trait]
    impl ExtensionContext for Ctx {
        fn cwd(&self) -> &Path {
            Path::new(".")
        }
        fn mode(&self) -> ExtensionMode {
            ExtensionMode::Tui
        }
        fn is_idle(&self) -> bool {
            true
        }
        fn signal(&self) -> CancellationToken {
            CancellationToken::new()
        }
        fn generation(&self) -> u64 {
            self.generation
        }
    }
    impl ExtensionCommandContext for Ctx {}

    #[test]
    fn load_dylib_missing_file_is_load_error() {
        let path = std::path::PathBuf::from("/nonexistent/ext-does-not-exist.dylib");
        let r = load_dylib(&path);
        assert!(
            matches!(r, Err(ExtensionError::Load(_))),
            "expected ExtensionError::Load"
        );
    }

    #[test]
    fn load_dylib_not_a_dylib_is_load_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not-a-dylib");
        std::fs::write(&path, b"not a dylib").expect("write");
        let r = load_dylib(&path);
        assert!(
            matches!(r, Err(ExtensionError::Load(_))),
            "expected ExtensionError::Load"
        );
    }

    /// §F5b — the fixture cdylib is built as a dev-dep; `build.rs` emits its
    /// path. Proves the full dylib load path: `load_dylib` → `configure`
    /// (registers `fixture_echo` tool + `TurnStart` handler) → `bind_core` →
    /// the tool is bound + the handler dispatches through the runner. The
    /// handler transforms `turn_id` → `fixture:<id>`, observed host-side via
    /// `EmitOutcome` (no shared static — see the fixture `lib.rs` header for
    /// the cdylib/rlib static-duplication reason). Lockstep holds (same
    /// workspace + toolchain).
    #[test]
    fn load_dylib_fixture_contributes_tool_and_handler() {
        let path = env!("CODESMITH_FIXTURE_DYLIB");
        let runner = crate::ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load_dylib(Path::new(path)))
            .expect("load fixture");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let tools: Vec<String> = runner.bound_tools().into_iter().map(|(n, _)| n).collect();
        assert!(
            tools.iter().any(|n| n == "fixture_echo"),
            "fixture tool bound: {tools:?}"
        );
        let out = rt.block_on(runner.emit(ExtensionEvent::TurnStart {
            turn_id: "t1".into(),
        }));
        match out.event {
            ExtensionEvent::TurnStart { turn_id } => {
                assert_eq!(
                    turn_id, "fixture:t1",
                    "fixture handler dispatched (transform proof)"
                );
            }
            other => panic!("expected TurnStart, got {other:?}"),
        }
    }
}
