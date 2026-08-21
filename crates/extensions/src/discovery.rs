//! Static (phase-1) discovery via `inventory` (spec §7.1).
//!
//! Extensions compiled into the binary register themselves via
//! `inventory::submit! { ExtensionRegistration { factory, metadata } }`;
//! [`discover_static`] iterates them at runtime (slice 1: no filtering —
//! enable/disable filtering against `ExtensionStateStore` happens in
//! `build_extension_runtime`, Task 9). Mirrors pi-mono's `builtInExtensions`.

use crate::manifest::ExtensionManifest;
use codesmith_agent::extension::ExtensionMetadata;
use std::path::{Path, PathBuf};

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

// === Phase-2 dylib discovery (spec §7.2 / §F5b) =============================

/// A discovered dylib source (phase 2). `config_path` is the
/// `extension.toml` location (`None` for a bare dylib file with no manifest);
/// `dylib_path` is the resolved cdylib to hand to [`load_dylib`]. `global`
/// distinguishes a shared install root (e.g. `~/.codesmith/extensions`) from a
/// project-local one (`.codesmith/extensions`), which the trust gate (§F5
/// FirstLoad / §F2c) treats differently.
#[derive(Debug, Clone)]
pub struct DiscoveredSource {
    pub id: String,
    pub version: String,
    pub config_path: Option<PathBuf>,
    pub dylib_path: PathBuf,
    pub global: bool,
}

/// Default dylib filename for an id when `extension.toml` carries no `entry`:
/// `<DLL_PREFIX><id>.<DLL_EXTENSION>` (e.g. `libdemo.dylib` on macOS). Mirrors
/// std's `env::consts::{DLL_PREFIX, DLL_EXTENSION}`; keep in sync with the
/// fixture crate's `crate_type = ["cdylib"]` name (§8.2).
pub(crate) fn default_dylib_filename(id: &str) -> String {
    let prefix = std::env::consts::DLL_PREFIX;
    let ext = std::env::consts::DLL_EXTENSION;
    if prefix.is_empty() {
        format!("{id}.{ext}")
    } else {
        format!("{prefix}{id}.{ext}")
    }
}

/// Walk global + project roots and discover all dylib sources (phase 2, §F5b).
/// Each root may be a container directory of extension subdirectories, a single
/// manifest dir (containing `extension.toml`), or a bare `.dylib`/`.so`/`.dll`
/// file. Dedups by canonicalized `dylib_path` so a source reached via two
/// roots loads once. Best-effort: malformed manifests / unreadable dirs are
/// skipped (§F5c will surface these via the EventBus).
pub fn discover_dylib(
    global_roots: &[PathBuf],
    project_roots: &[PathBuf],
) -> Vec<DiscoveredSource> {
    let mut out = Vec::new();
    for root in global_roots {
        discover_in_root(root, true, &mut out);
    }
    for root in project_roots {
        discover_in_root(root, false, &mut out);
    }
    dedup_by_dylib_path(&mut out);
    out
}

/// Scan one root (container dir, single manifest dir, or bare dylib file) and
/// push discovered sources into `out`.
fn discover_in_root(root: &Path, global: bool, out: &mut Vec<DiscoveredSource>) {
    if root.is_dir() {
        if root.join("extension.toml").exists() {
            // The root itself is a single manifest dir.
            if let Some(s) = discover_manifest_dir(root, global) {
                out.push(s);
            }
        } else {
            // Container: scan children, read-sorted for determinism.
            let Ok(entries) = std::fs::read_dir(root) else {
                return;
            };
            let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
            entries.sort_by_key(|e| e.path());
            for entry in entries {
                let p = entry.path();
                if p.is_dir() && p.join("extension.toml").exists() {
                    if let Some(s) = discover_manifest_dir(&p, global) {
                        out.push(s);
                    }
                } else if is_dylib_file(&p)
                    && let Some(s) = discover_bare(&p, global)
                {
                    out.push(s);
                }
            }
        }
    } else if root.is_file()
        && is_dylib_file(root)
        && let Some(s) = discover_bare(root, global)
    {
        out.push(s);
    }
}

/// Parse `extension.toml` under `dir` and resolve the dylib `entry` (default
/// filename when absent). Yields regardless of dylib-file existence (the loader
/// reports a missing file as `ExtensionError::Load`). Returns `None` on parse
/// failure (best-effort skip).
fn discover_manifest_dir(dir: &Path, global: bool) -> Option<DiscoveredSource> {
    let manifest = ExtensionManifest::parse(&dir.join("extension.toml")).ok()?;
    let dylib_path = match &manifest.entry {
        Some(entry) => dir.join(entry),
        None => dir.join(default_dylib_filename(&manifest.id)),
    };
    Some(DiscoveredSource {
        id: manifest.id,
        version: manifest.version,
        config_path: Some(dir.join("extension.toml")),
        dylib_path,
        global,
    })
}

/// Synthesize a source from a bare dylib file (no manifest). `id` is the file
/// stem with the platform `DLL_PREFIX` stripped (`libbare.dylib` → `bare`).
fn discover_bare(path: &Path, global: bool) -> Option<DiscoveredSource> {
    if !is_dylib_file(path) {
        return None;
    }
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let prefix = std::env::consts::DLL_PREFIX;
    let id = stem.strip_prefix(prefix).map(str::to_owned).unwrap_or(stem);
    Some(DiscoveredSource {
        id,
        version: "0.0.0".into(),
        config_path: None,
        dylib_path: path.to_path_buf(),
        global,
    })
}

/// `true` when `path` is a file whose extension matches the platform
/// `DLL_EXTENSION` (`dylib` / `so` / `dll`).
fn is_dylib_file(path: &Path) -> bool {
    path.is_file()
        && path.extension().and_then(|e| e.to_str()) == Some(std::env::consts::DLL_EXTENSION)
}

/// Drop later occurrences of a source whose canonicalized `dylib_path` was
/// already seen (a source reached via two roots loads once). Falls back to
/// the raw path when `canonicalize` fails (file not yet built) so dedup still
/// works.
fn dedup_by_dylib_path(out: &mut Vec<DiscoveredSource>) {
    let mut seen = std::collections::HashSet::new();
    out.retain(|s| {
        let key = s
            .dylib_path
            .canonicalize()
            .unwrap_or_else(|_| s.dylib_path.clone());
        seen.insert(key)
    });
}

/// Trust gate (§F5 FirstLoad / §F2c). Drops project-local (`global == false`)
/// sources when the workspace trust mode is `Untrusted` (`trust_untrusted ==
/// true`); global sources are retained regardless (their shared-install
/// provenance implies prior consent). Mirrors the `Untrusted` arm of §F2c T3's
/// per-turn `ProjectTrust{Untrusted}` dispatch, applied at discovery time so
/// an untrusted workspace never loads a local dylib's `Library`. (§F5c keeps
/// Model A as-is — no configured-path concept; `apply_trust_gate` is final for
/// the install/load path.)
pub fn apply_trust_gate(
    sources: Vec<DiscoveredSource>,
    trust_untrusted: bool,
) -> Vec<DiscoveredSource> {
    if !trust_untrusted {
        return sources; // trusted workspace: keep all.
    }
    sources.into_iter().filter(|s| s.global).collect() // untrusted: drop project-local.
}

#[cfg(test)]
mod dylib_tests {
    use super::*;

    #[test]
    fn discover_dylib_finds_manifest_subdir_with_default_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ext_dir = dir.path().join("demo");
        std::fs::create_dir(&ext_dir).expect("mkdir");
        std::fs::write(
            ext_dir.join("extension.toml"),
            "id = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .expect("write manifest");
        let found = discover_dylib(&[dir.path().to_path_buf()], &[]);
        assert_eq!(found.len(), 1, "expected 1 source, got {found:?}");
        assert_eq!(found[0].id, "demo");
        assert_eq!(found[0].version, "0.1.0");
        let expected = default_dylib_filename("demo");
        assert!(
            found[0].dylib_path.ends_with(&expected),
            "dylib_path {:?} should end with {expected}",
            found[0].dylib_path
        );
        assert!(found[0].config_path.is_some());
        assert!(found[0].global);
    }

    #[test]
    fn discover_dylib_finds_bare_dylib_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fname = format!("libbare.{}", std::env::consts::DLL_EXTENSION);
        let path = dir.path().join(&fname);
        std::fs::write(&path, b"").expect("write dylib placeholder");
        let found = discover_dylib(&[path.clone()], &[]);
        assert_eq!(found.len(), 1, "expected 1 source, got {found:?}");
        assert_eq!(found[0].id, "bare");
        assert_eq!(found[0].dylib_path, path);
        assert!(found[0].config_path.is_none());
    }

    #[test]
    fn discover_dylib_dedups_shared_dylib_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ext_dir = dir.path().join("demo");
        std::fs::create_dir(&ext_dir).expect("mkdir");
        std::fs::write(
            ext_dir.join("extension.toml"),
            "id = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .expect("write manifest");
        let root = dir.path().to_path_buf();
        // Same root passed twice → same dylib_path → dedup to 1.
        let found = discover_dylib(&[root.clone(), root], &[]);
        assert_eq!(found.len(), 1, "expected dedup to 1, got {found:?}");
    }

    #[test]
    fn discover_dylib_tags_global_and_project_roots_distinctly() {
        // Helper writes a manifest-subdir named `id` under `root`; the TempDir
        // guards stay alive at test scope (a closure returning a path would
        // drop the guard + delete the temp dir before discovery runs).
        let setup = |id: &str, root: &Path| {
            let ext_dir = root.join(id);
            std::fs::create_dir(&ext_dir).expect("mkdir");
            std::fs::write(
                ext_dir.join("extension.toml"),
                format!("id = \"{id}\"\nversion = \"1.0.0\"\n"),
            )
            .expect("write manifest");
        };
        let g = tempfile::tempdir().expect("tempdir");
        let p = tempfile::tempdir().expect("tempdir");
        setup("gext", g.path());
        setup("pext", p.path());
        let found = discover_dylib(&[g.path().to_path_buf()], &[p.path().to_path_buf()]);
        assert_eq!(found.len(), 2, "expected 2 sources, got {found:?}");
        let gext = found.iter().find(|s| s.id == "gext").expect("gext");
        assert!(gext.global, "gext should be global");
        let pext = found.iter().find(|s| s.id == "pext").expect("pext");
        assert!(!pext.global, "pext should be project-local");
    }

    #[test]
    fn apply_trust_gate_drops_project_local_when_untrusted() {
        let mk = |global| DiscoveredSource {
            id: "x".into(),
            version: "0".into(),
            config_path: None,
            dylib_path: std::path::PathBuf::from("/x"),
            global,
        };
        let sources = vec![mk(true), mk(false), mk(false)];
        let trusted = apply_trust_gate(sources.clone(), false);
        assert_eq!(trusted.len(), 3, "trusted keeps all");
        let untrusted = apply_trust_gate(sources, true);
        assert_eq!(untrusted.len(), 1, "untrusted drops project-local");
        assert!(untrusted[0].global);
    }
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
            all.iter()
                .map(|r| r.metadata.id)
                .collect::<Vec<_>>()
                .join(", ")
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
