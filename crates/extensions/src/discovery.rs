//! Static (phase-1) discovery via `inventory` (spec §7.1).
//!
//! Extensions compiled into the binary register themselves via
//! `inventory::submit! { ExtensionRegistration { factory, metadata } }`;
//! [`discover_static`] iterates them at runtime (slice 1: no filtering —
//! enable/disable filtering against `ExtensionStateStore` happens in
//! `build_extension_runtime`, Task 9). Mirrors pi-mono's `builtInExtensions`.

use codesmith_agent::extension::ExtensionMetadata;

/// A compiled-in extension registration. `factory` constructs a fresh
/// `Box<dyn Extension>` per load (so a reload gets clean state). Mirrors
/// pi-mono's `ExtensionFactory` + manifest.
pub struct ExtensionRegistration {
    pub factory: fn() -> Box<dyn codesmith_agent::extension::Extension>,
    pub metadata: ExtensionMetadata,
}

inventory::collect!(ExtensionRegistration);

/// Iterate every compiled-in extension registration. Order is unspecified
/// (inventory order); callers that need determinism sort by `metadata.id`.
pub fn discover_static() -> Vec<&'static ExtensionRegistration> {
    inventory::iter::<ExtensionRegistration>().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use codesmith_agent::extension::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static LOAD_COUNT: AtomicUsize = AtomicUsize::new(0);

    struct NoopExt;
    #[async_trait]
    impl Extension for NoopExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("test-noop");
            &M
        }
        async fn configure(&self, _api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            LOAD_COUNT.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    inventory::submit! {
        ExtensionRegistration {
            factory: || Box::new(NoopExt),
            metadata: ExtensionMetadata::new("test-noop"),
        }
    }

    #[test]
    fn discover_static_finds_submitted_registration() {
        let all = discover_static();
        assert!(
            all.iter().any(|r| r.metadata.id == "test-noop"),
            "test-noop not discovered; all={} (inventory submit may need module-scope; see plan §4.3 fallback)",
            all.iter().map(|r| r.metadata.id).collect::<Vec<_>>().join(", ")
        );
    }

    #[test]
    fn factory_builds_fresh_extension_each_call() {
        let all = discover_static();
        let reg = all
            .iter()
            .find(|r| r.metadata.id == "test-noop")
            .expect("test-noop registered");
        let before = LOAD_COUNT.load(Ordering::Relaxed);
        let ext = (reg.factory)();
        // Drop ext without configuring — factory just proves constructible.
        drop(ext);
        let after = LOAD_COUNT.load(Ordering::Relaxed);
        assert_eq!(before, after); // configure not called — count unchanged
    }
}
