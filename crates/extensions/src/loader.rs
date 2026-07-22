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
    let register: Symbol<unsafe extern "C" fn() -> *mut dyn Extension> =
        unsafe { library.get(REGISTER_SYMBOL) }
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

    #[test]
    fn load_dylib_missing_file_is_load_error() {
        let path = std::path::PathBuf::from("/nonexistent/ext-does-not-exist.dylib");
        let r = load_dylib(&path);
        assert!(matches!(r, Err(ExtensionError::Load(_))), "expected ExtensionError::Load");
    }

    #[test]
    fn load_dylib_not_a_dylib_is_load_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not-a-dylib");
        std::fs::write(&path, b"not a dylib").expect("write");
        let r = load_dylib(&path);
        assert!(matches!(r, Err(ExtensionError::Load(_))), "expected ExtensionError::Load");
    }
}
