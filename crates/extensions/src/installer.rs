//! §F5c install orchestrator. Coordinates fetch → build → D8 temp-load
//! `metadata()` for id/version → `Placer` (copies dylib) → write
//! `extension.toml`. Pure: no state/config (those are tui-layer; R1 — this
//! crate cannot depend on `codesmith-tui`'s `ExtensionStateStore` or
//! `is_workspace_trusted`). The `Placer` is constructed inside `install()`
//! after D8 yields the id (id is unknown until the built dylib's `metadata()`
//! is read).

use std::path::{Path, PathBuf};

use codesmith_agent::extension::ExtensionError;

use crate::install_source::{
    ExtensionBuilder, ExtensionPlacer, ExtensionSource, Placer, SourceArtifact, SourceKind,
    SourceSpec,
};

/// Install result (§F5c). `id`/`version` come from D8 temp-loading the built
/// dylib's `metadata()` (not Cargo.toml parsing). `provenance` is the
/// source's normalized spec (e.g. `git:<url>[@<ref>]`). Trust-warn is the
/// caller's job (tui command) — R1.
#[derive(Debug, Clone)]
pub struct InstallReport {
    pub id: String,
    pub version: String,
    pub path: PathBuf,
    pub provenance: String,
}

/// Uninstall result (§F5c). `removed` is true if any `<root>/<id>/` dir was
/// deleted. State mutation (`remove_installed`) is the caller's.
#[derive(Debug, Clone)]
pub struct UninstallReport {
    pub id: String,
    pub removed: bool,
}

/// Install/uninstall orchestrator (§F5c). Holds trait-DI `source` + `builder`;
/// the `Placer` is constructed inside `install()` after D8 yields the id.
/// R1: pure — no state/config.
pub struct Installer<'a> {
    pub source: &'a dyn ExtensionSource,
    pub builder: &'a dyn ExtensionBuilder,
    pub root: PathBuf,
}

impl<'a> Installer<'a> {
    pub fn new(
        source: &'a dyn ExtensionSource,
        builder: &'a dyn ExtensionBuilder,
        root: PathBuf,
    ) -> Self {
        Self {
            source,
            builder,
            root,
        }
    }

    pub fn install(&self, spec: &SourceSpec) -> Result<InstallReport, ExtensionError> {
        // 3. fetch (source is trust-agnostic; §F5c short-circuits crate/prebuilt
        //    in the tui command before constructing this — R4).
        let dest = tempfile::tempdir()
            .map_err(|e| ExtensionError::Install(format!("tempdir for fetch: {e}")))?;
        let art: SourceArtifact = self.source.fetch(dest.path())?;
        // 4. build
        let cdylib = self.builder.build(&art.path)?;
        // 5. D8: temp-load the built dylib's `metadata()` for id/version. No
        //    configure/register (we only read metadata, then drop). The
        //    `Library` + `Box` must be alive for the `metadata()` call (vtable
        //    lives in the `Library`); drop both before `place` so the build
        //    artifact file is free to copy on all platforms.
        let (_lib, ext_box) = crate::loader::load_dylib(&cdylib)?;
        let id = ext_box.metadata().id.to_string();
        let version = ext_box.metadata().version.to_string();
        drop(ext_box);
        drop(_lib);
        // 6. place (Placer constructed here — id from D8; R3).
        let placer = Placer::new(&id, &self.root);
        let placed = placer.place(&cdylib)?;
        // 7. write extension.toml. `entry` is omitted so `discover_dylib`
        //    resolves `default_dylib_filename(id)`, matching the placed file.
        let mut manifest = format!("id = \"{id}\"\nversion = \"{version}\"\n");
        manifest.push_str(&format!("[source]\ntype = \"{}\"\n", manifest_kind(spec)));
        if let Some(r) = &spec.ref_ {
            manifest.push_str(&format!("ref = \"{r}\"\n"));
        }
        std::fs::write(placer.dir().join("extension.toml"), manifest)
            .map_err(|e| ExtensionError::Install(format!("write manifest: {e}")))?;
        Ok(InstallReport {
            id,
            version,
            path: placed,
            provenance: art.provenance,
        })
    }

    /// Remove `<root>/<id>/` from any of `roots`. `removed` is true if any
    /// dir was deleted. §F5c: state mutation is the caller's (R1).
    pub fn uninstall_files(id: &str, roots: &[PathBuf]) -> Result<UninstallReport, ExtensionError> {
        let mut removed = false;
        for root in roots {
            let dir = root.join(id);
            if dir.exists() {
                std::fs::remove_dir_all(&dir).map_err(|e| {
                    ExtensionError::Install(format!("remove {}: {e}", dir.display()))
                })?;
                removed = true;
            }
        }
        Ok(UninstallReport {
            id: id.to_string(),
            removed,
        })
    }
}

fn manifest_kind(spec: &SourceSpec) -> &'static str {
    match spec.kind {
        SourceKind::Git => "git",
        SourceKind::Path => "path",
        SourceKind::CratesIo => "crate",
        SourceKind::Prebuilt => "prebuilt",
    }
}

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use crate::install_source::*;
    use async_trait::async_trait;
    use codesmith_agent::extension::*;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    // Mirrors `crates/extensions/src/loader.rs:61-82` (§F5b): `bind_core`
    // holds `Arc<dyn ExtensionCommandContext>`; the test Ctx must impl the
    // sub-trait (marker) for the coercion to fire.
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

    struct FakeSource {
        provenance: String,
    }
    impl ExtensionSource for FakeSource {
        fn fetch(&self, dest: &Path) -> Result<SourceArtifact, ExtensionError> {
            Ok(SourceArtifact {
                path: dest.to_path_buf(),
                provenance: self.provenance.clone(),
            })
        }
    }
    struct FakeBuilder {
        dylib: PathBuf,
    }
    impl ExtensionBuilder for FakeBuilder {
        fn build(&self, _src_dir: &Path) -> Result<PathBuf, ExtensionError> {
            Ok(self.dylib.clone())
        }
    }

    /// §F5c install→load round-trip: FakeSource + FakeBuilder (returns the §F5b
    /// fixture dylib) + real Placer + real manifest write → discover → load →
    /// `fixture_echo` bound. Proves the install pipeline end-to-end without a
    /// real `cargo build` (avoids target-dir lock / dep-tree rebuild). R5: no
    /// state assertion (state is tui-side, unit-tested in T2).
    #[test]
    fn install_to_load_roundtrip_binds_fixture_tool() {
        let fixture = env!("CODESMITH_FIXTURE_DYLIB");
        let root = tempfile::tempdir().expect("temp root");
        let source = FakeSource {
            provenance: "test:fake".into(),
        };
        let builder = FakeBuilder {
            dylib: PathBuf::from(fixture),
        };
        let installer = Installer::new(&source, &builder, root.path().to_path_buf());
        let spec = SourceSpec::parse("path:/ignored").unwrap();
        let report = installer.install(&spec).expect("install");
        assert_eq!(report.id, "fixture-dylib", "id from fixture metadata (D8)");
        assert!(
            report.path.is_file(),
            "placed dylib exists: {}",
            report.path.display()
        );
        let manifest_path = root.path().join("fixture-dylib").join("extension.toml");
        assert!(manifest_path.is_file(), "manifest written");
        let manifest_text = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(
            manifest_text.contains("id = \"fixture-dylib\""),
            "manifest id: {manifest_text}"
        );
        assert!(manifest_text.contains("[source]"), "manifest source: {manifest_text}");
        assert_eq!(report.provenance, "test:fake");

        let found = crate::discover_dylib(&[root.path().to_path_buf()], &[]);
        assert_eq!(found.len(), 1, "discover finds 1: {found:?}");
        assert_eq!(found[0].id, "fixture-dylib");
        assert!(
            found[0].config_path.is_some(),
            "manifest-subdir (not bare)"
        );

        let runner = crate::ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load_dylib(&found[0].dylib_path))
            .expect("load placed dylib");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let tools: Vec<String> = runner
            .bound_tools()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(
            tools.iter().any(|n| n == "fixture_echo"),
            "fixture_echo bound: {tools:?}"
        );
    }

    #[test]
    fn uninstall_files_removes_id_dir() {
        let root = tempfile::tempdir().unwrap();
        let placer = Placer::new("gone", root.path().to_path_buf());
        let artifact = root.path().join("a.bin");
        std::fs::write(&artifact, b"x").unwrap();
        placer.place(&artifact).unwrap();
        assert!(root.path().join("gone").exists());
        let report =
            Installer::uninstall_files("gone", &[root.path().to_path_buf()]).unwrap();
        assert!(report.removed);
        assert!(!root.path().join("gone").exists());
        let report2 =
            Installer::uninstall_files("absent", &[root.path().to_path_buf()]).unwrap();
        assert!(!report2.removed);
    }
}
