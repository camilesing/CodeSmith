//! Install-source abstraction **traits** (spec §6.4). Impls are §F5
//! (dylib loading). Slice 1 ships only the trait shapes so the §F1
//! `/extension install` stub (Task 8) can reference
//! [`ExtensionError::Install`](codesmith_agent::extension::ExtensionError)
//! and so the ROADMAP §F5 entry has a stable contract to point at.

use std::path::{Path, PathBuf};
use std::process::Command;

use codesmith_agent::extension::ExtensionError;

/// A fetched install artifact (path + provenance string for
/// `ExtensionStateStore.installed`).
#[derive(Debug)]
pub struct SourceArtifact {
    pub path: PathBuf,
    pub provenance: String,
}

/// Fetch an extension source to `dest`. §F5 impls: `GitSource`,
/// `CratesIoSource`, `LocalPathSource`, `PrebuiltDylibSource`.
pub trait ExtensionSource: Send + Sync {
    fn fetch(&self, dest: &Path) -> Result<SourceArtifact, ExtensionError>;
}

/// Build a fetched source into a dylib. §F5 impl: `CargoBuilder`.
pub trait ExtensionBuilder: Send + Sync {
    fn build(&self, src_dir: &Path) -> Result<PathBuf, ExtensionError>;
}

/// Place a built dylib into `~/.codesmith/extensions/<id>/`. §F5 impl.
pub trait ExtensionPlacer: Send + Sync {
    fn place(&self, artifact: &Path) -> Result<PathBuf, ExtensionError>;
}

/// §F5 placeholder source — returns [`ExtensionError::Install`] always.
/// Slice 1's `/extension install` stub (Task 8) uses it to produce the
/// "requires dylib loader (phase 2)" error without ceremony.
pub struct UnimplementedSource;
impl ExtensionSource for UnimplementedSource {
    fn fetch(&self, _dest: &Path) -> Result<SourceArtifact, ExtensionError> {
        Err(ExtensionError::Install(
            "install requires the dylib loader (§F5)".into(),
        ))
    }
}

/// Install placement scope (§F5c). Default `Project` (trust-gated by §F5b
/// `apply_trust_gate`); `Global` opt-in via `--global` (loads unconditionally).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallScope {
    Project,
    Global,
}

/// The source kind parsed from the `<kind>:` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Git,
    Path,
    CratesIo, // §F5c stubbed (nice-to-have)
    Prebuilt, // §F5c stubbed (nice-to-have)
}

/// Parsed `/extension install <spec> [--global]` source spec (§F5c).
/// Grammar: `<kind>:<body>[@<ref>]` where `kind ∈ {git, path, crate, prebuilt}`.
/// `@<ref>` is split on the LAST `@` (so `git:host/path@v1` → ref `v1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpec {
    pub kind: SourceKind,
    pub body: String,
    pub ref_: Option<String>,
    pub scope: InstallScope,
}

impl SourceSpec {
    /// Parse a `/extension install` arg string. `--global` → `Global` scope
    /// (default `Project`). The first non-`--` token is the `<kind>:<body>`
    /// spec; everything else (flags) is ignored.
    pub fn parse(arg: &str) -> Result<Self, ExtensionError> {
        let scope = if arg.split_whitespace().any(|t| t == "--global") {
            InstallScope::Global
        } else {
            InstallScope::Project
        };
        let spec_token = arg
            .split_whitespace()
            .find(|t| !t.starts_with("--"))
            .ok_or_else(|| {
                ExtensionError::Install(
                    "missing source spec (expected `<kind>:<body>[@<ref>]`)".into(),
                )
            })?;
        let (kind_str, rest) = spec_token
            .split_once(':')
            .ok_or_else(|| {
                ExtensionError::Install(format!(
                    "source spec must be `<kind>:<body>`; got {spec_token:?}"
                ))
            })?;
        let kind = match kind_str {
            "git" => SourceKind::Git,
            "path" => SourceKind::Path,
            "crate" => SourceKind::CratesIo,
            "prebuilt" => SourceKind::Prebuilt,
            other => {
                return Err(ExtensionError::Install(format!(
                    "unknown source kind {other:?}; expected git|path|crate|prebuilt"
                )))
            }
        };
        let (body, ref_) = match rest.rsplit_once('@') {
            Some((b, r)) if !r.is_empty() => (b.to_string(), Some(r.to_string())),
            _ => (rest.to_string(), None),
        };
        if body.is_empty() {
            return Err(ExtensionError::Install("source body is empty".into()));
        }
        Ok(SourceSpec {
            kind,
            body,
            ref_,
            scope,
        })
    }
}

/// Git install source (§F5c must-have). `git clone --depth 1 [--branch <ref>]`.
pub struct GitSource {
    pub url: String,
    pub ref_: Option<String>,
}

impl GitSource {
    pub fn new(url: impl Into<String>, ref_: Option<String>) -> Self {
        Self {
            url: url.into(),
            ref_,
        }
    }

    /// Canonical provenance string (`git:<url>` or `git:<url>@<ref>`).
    pub fn provenance(&self) -> String {
        match &self.ref_ {
            Some(r) => format!("git:{}@{}", self.url, r),
            None => format!("git:{}", self.url),
        }
    }
}

impl ExtensionSource for GitSource {
    fn fetch(&self, dest: &Path) -> Result<SourceArtifact, ExtensionError> {
        let mut cmd = Command::new("git");
        cmd.arg("clone").arg("--depth").arg("1");
        if let Some(r) = &self.ref_ {
            cmd.arg("--branch").arg(r);
        }
        cmd.arg(&self.url).arg(dest);
        let out = cmd
            .output()
            .map_err(|e| ExtensionError::Install(format!("spawn git (on PATH?): {e}")))?;
        if !out.status.success() {
            return Err(ExtensionError::Install(format!(
                "git clone {} failed: {}",
                self.url,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(SourceArtifact {
            path: dest.to_path_buf(),
            provenance: self.provenance(),
        })
    }
}

/// Local-path install source (§F5c must-have). Recursively copies the dir.
pub struct LocalPathSource {
    pub dir: PathBuf,
}

impl LocalPathSource {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl ExtensionSource for LocalPathSource {
    fn fetch(&self, dest: &Path) -> Result<SourceArtifact, ExtensionError> {
        if !self.dir.is_dir() {
            return Err(ExtensionError::Install(format!(
                "path source not a dir: {}",
                self.dir.display()
            )));
        }
        copy_dir_recursive(&self.dir, dest)?;
        let canon = std::fs::canonicalize(&self.dir).unwrap_or_else(|_| self.dir.clone());
        Ok(SourceArtifact {
            path: dest.to_path_buf(),
            provenance: format!("path:{}", canon.display()),
        })
    }
}

/// Recursive dir copy (§F5c `LocalPathSource::fetch`). std has no recursive copy.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), ExtensionError> {
    std::fs::create_dir_all(dst)
        .map_err(|e| ExtensionError::Install(format!("mkdir {}: {e}", dst.display())))?;
    for entry in std::fs::read_dir(src)
        .map_err(|e| ExtensionError::Install(format!("read_dir {}: {e}", src.display())))?
    {
        let entry =
            entry.map_err(|e| ExtensionError::Install(format!("dir entry: {e}")))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|e| ExtensionError::Install(format!("copy {}: {e}", from.display())))?;
        }
    }
    Ok(())
}

/// Build a fetched source into a cdylib via `cargo build` (§F5c).
/// `cargo build --release --locked --target-dir <temp>`, then scan
/// `target/release/` for the platform cdylib (`.<DLL_EXTENSION>`). No JSON
/// parse (R2: avoids a `serde_json` dep). Robust for single-cdylib crates;
/// errors on 0 or >1 cdylib (ambiguous).
pub struct CargoBuilder {
    target_dir: PathBuf,
}

impl CargoBuilder {
    pub fn new(target_dir: impl Into<PathBuf>) -> Self {
        Self {
            target_dir: target_dir.into(),
        }
    }
}

impl ExtensionBuilder for CargoBuilder {
    fn build(&self, src_dir: &Path) -> Result<PathBuf, ExtensionError> {
        let out = Command::new("cargo")
            .arg("build")
            .arg("--release")
            .arg("--locked")
            .arg("--target-dir")
            .arg(&self.target_dir)
            .current_dir(src_dir)
            .output()
            .map_err(|e| ExtensionError::Install(format!("spawn cargo (on PATH?): {e}")))?;
        if !out.status.success() {
            return Err(ExtensionError::Install(format!(
                "cargo build failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let release_dir = self.target_dir.join("release");
        let entries = std::fs::read_dir(&release_dir).map_err(|e| {
            ExtensionError::Install(format!("read release dir {}: {e}", release_dir.display()))
        })?;
        let mut found: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension().and_then(|e| e.to_str())
                        == Some(std::env::consts::DLL_EXTENSION)
            })
            .collect();
        match found.len() {
            1 => Ok(found.pop().expect("len==1")),
            0 => Err(ExtensionError::Install(format!(
                "no cdylib (.{}) produced in {}",
                std::env::consts::DLL_EXTENSION,
                release_dir.display()
            ))),
            n => Err(ExtensionError::Install(format!(
                "{n} cdylibs in {} (ambiguous); §F5c supports single-cdylib crates only",
                release_dir.display()
            ))),
        }
    }
}

/// Place a built cdylib into `<root>/<id>/` (§F5c). The dylib is renamed to
/// `default_dylib_filename(id)` so `discover_dylib` (manifest with no `entry`)
/// re-finds it as a manifest-subdir source (not bare). The `extension.toml`
/// is written separately by the `Installer` (it has version/source; the trait
/// `place(artifact)` does not). R3.
pub struct Placer {
    pub id: String,
    pub root: PathBuf,
}

impl Placer {
    pub fn new(id: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            root: root.into(),
        }
    }

    /// The dest dir `<root>/<id>/` (manifest + dylib live here).
    pub fn dir(&self) -> PathBuf {
        self.root.join(&self.id)
    }
}

impl ExtensionPlacer for Placer {
    fn place(&self, artifact: &Path) -> Result<PathBuf, ExtensionError> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| ExtensionError::Install(format!("mkdir {}: {e}", dir.display())))?;
        let dest = dir.join(crate::discovery::default_dylib_filename(&self.id));
        std::fs::copy(artifact, &dest)
            .map_err(|e| ExtensionError::Install(format!("copy dylib to {}: {e}", dest.display())))?;
        Ok(dest)
    }
}

#[cfg(test)]
mod source_spec_tests {
    use super::*;

    #[test]
    fn parse_git_no_ref_defaults_project() {
        let s = SourceSpec::parse("git:github.com/foo/bar").unwrap();
        assert_eq!(s.kind, SourceKind::Git);
        assert_eq!(s.body, "github.com/foo/bar");
        assert_eq!(s.ref_, None);
        assert_eq!(s.scope, InstallScope::Project);
    }

    #[test]
    fn parse_git_with_ref() {
        let s = SourceSpec::parse("git:github.com/foo/bar@v1.0.0").unwrap();
        assert_eq!(s.body, "github.com/foo/bar");
        assert_eq!(s.ref_.as_deref(), Some("v1.0.0"));
    }

    #[test]
    fn parse_path_kind() {
        let s = SourceSpec::parse("path:/abs/ext/dir").unwrap();
        assert_eq!(s.kind, SourceKind::Path);
        assert_eq!(s.body, "/abs/ext/dir");
        assert_eq!(s.scope, InstallScope::Project);
    }

    #[test]
    fn parse_crate_kind_recognized() {
        let s = SourceSpec::parse("crate:my-ext").unwrap();
        assert_eq!(s.kind, SourceKind::CratesIo);
    }

    #[test]
    fn parse_prebuilt_kind_recognized() {
        let s = SourceSpec::parse("prebuilt:https://x/y.dylib").unwrap();
        assert_eq!(s.kind, SourceKind::Prebuilt);
    }

    #[test]
    fn parse_global_flag_sets_global_scope() {
        let s = SourceSpec::parse("git:foo/bar --global").unwrap();
        assert_eq!(s.scope, InstallScope::Global);
        let s2 = SourceSpec::parse("--global git:foo/bar").unwrap();
        assert_eq!(s2.scope, InstallScope::Global);
    }

    #[test]
    fn parse_missing_kind_separator_is_install_error() {
        let r = SourceSpec::parse("nospec");
        assert!(matches!(r, Err(ExtensionError::Install(_))), "got {r:?}");
    }

    #[test]
    fn parse_unknown_kind_is_install_error() {
        let r = SourceSpec::parse("svn:foo/bar");
        assert!(matches!(r, Err(ExtensionError::Install(_))), "got {r:?}");
    }

    #[test]
    fn parse_empty_body_is_install_error() {
        let r = SourceSpec::parse("git:");
        assert!(matches!(r, Err(ExtensionError::Install(_))), "got {r:?}");
    }

    #[test]
    fn parse_missing_spec_token_is_install_error() {
        let r = SourceSpec::parse("--global");
        assert!(matches!(r, Err(ExtensionError::Install(_))), "got {r:?}");
    }
}

#[cfg(test)]
mod source_impl_tests {
    use super::*;

    #[test]
    fn git_source_provenance_with_and_without_ref() {
        let with = GitSource::new("github.com/foo/bar", Some("v1".into()));
        assert_eq!(with.provenance(), "git:github.com/foo/bar@v1");
        let without = GitSource::new("github.com/foo/bar", None);
        assert_eq!(without.provenance(), "git:github.com/foo/bar");
    }

    #[test]
    fn git_source_fetch_invalid_url_is_install_error() {
        // Best-effort: git clone of an invalid host fails (no network also fails).
        // Either way → ExtensionError::Install. Skipped if `git` not on PATH.
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("git not on PATH; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let s = GitSource::new("https://install-test-invalid-host.invalid/none.git", None);
        let r = s.fetch(dir.path());
        assert!(matches!(r, Err(ExtensionError::Install(_))), "got {r:?}");
    }

    #[test]
    fn local_path_source_copies_dir_and_provenance() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(
            src.path().join("Cargo.toml"),
            b"[package]\nname=\"x\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(src.path().join("src")).unwrap();
        std::fs::write(src.path().join("src/lib.rs"), b"").unwrap();
        let dst = tempfile::tempdir().unwrap();
        let s = LocalPathSource::new(src.path().to_path_buf());
        let art = s.fetch(dst.path()).unwrap();
        assert!(dst.path().join("Cargo.toml").exists(), "Cargo.toml copied");
        assert!(dst.path().join("src/lib.rs").exists(), "src/lib.rs copied");
        assert!(art.provenance.starts_with("path:"), "provenance: {}", art.provenance);
        assert_eq!(art.path, dst.path());
    }

    #[test]
    fn local_path_source_missing_dir_is_install_error() {
        let s = LocalPathSource::new("/nonexistent/ext/dir");
        let dst = tempfile::tempdir().unwrap();
        let r = s.fetch(dst.path());
        assert!(matches!(r, Err(ExtensionError::Install(_))), "got {r:?}");
    }

    #[test]
    fn local_path_source_recursive_copy() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("a/b")).unwrap();
        std::fs::write(src.path().join("a/b/c.txt"), b"deep").unwrap();
        let dst = tempfile::tempdir().unwrap();
        let s = LocalPathSource::new(src.path().to_path_buf());
        s.fetch(dst.path()).unwrap();
        assert!(dst.path().join("a/b/c.txt").is_file());
        assert_eq!(std::fs::read(dst.path().join("a/b/c.txt")).unwrap(), b"deep");
    }
}

#[cfg(test)]
mod cargo_builder_tests {
    use super::*;

    /// Build a tiny standalone cdylib crate in a TempDir + assert CargoBuilder
    /// produces the cdylib. Skips if `cargo` not on PATH (CI without rust
    /// toolchain). Uses a temp `--target-dir` (no workspace lock conflict).
    #[test]
    fn cargo_builder_builds_tiny_cdylib() {
        if std::process::Command::new("cargo")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("cargo not on PATH; skipping cargo_builder test");
            return;
        }
        let src = tempfile::tempdir().expect("src tempdir");
        let pkg = src.path();
        std::fs::write(
            pkg.join("Cargo.toml"),
            "[package]\nname = \"tiny_cdylib_test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(
            pkg.join("src/lib.rs"),
            "#![allow(unused)]\n#[unsafe(no_mangle)]\npub extern \"C\" fn codesmith_register_extension() -> *mut () { std::ptr::null_mut() }\n",
        )
        .unwrap();
        // --locked needs a Cargo.lock; generate one (no-dep crate → offline-safe).
        let _ = std::process::Command::new("cargo")
            .arg("generate-lockfile")
            .current_dir(pkg)
            .output();
        let target = tempfile::tempdir().expect("target tempdir");
        let builder = CargoBuilder::new(target.path().to_path_buf());
        let cdylib = builder.build(pkg).expect("build tiny cdylib");
        assert_eq!(
            cdylib.extension().and_then(|e| e.to_str()),
            Some(std::env::consts::DLL_EXTENSION),
            "cdylib extension: {cdylib:?}"
        );
        assert!(cdylib.is_file(), "cdylib exists: {cdylib:?}");
    }

    #[test]
    fn cargo_builder_missing_cargo_manifest_is_install_error() {
        // Point cargo build at a dir with no Cargo.toml → fails → Install error.
        let empty = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let builder = CargoBuilder::new(target.path().to_path_buf());
        let r = builder.build(empty.path());
        assert!(matches!(r, Err(ExtensionError::Install(_))), "got {r:?}");
    }
}

#[cfg(test)]
mod placer_tests {
    use super::*;

    #[test]
    fn placer_copies_dylib_to_default_filename() {
        let root = tempfile::tempdir().unwrap();
        // A fake "built dylib" (any file; Placer doesn't validate it's a real dylib).
        let src = tempfile::tempdir().unwrap();
        let artifact = src.path().join("libwhatever.bin");
        std::fs::write(&artifact, b"binary").unwrap();
        let placer = Placer::new("my-ext", root.path().to_path_buf());
        let dest = placer.place(&artifact).unwrap();
        let expected =
            root.path()
                .join("my-ext")
                .join(crate::discovery::default_dylib_filename("my-ext"));
        assert_eq!(dest, expected, "placed at default filename");
        assert!(dest.is_file(), "placed file exists");
        assert_eq!(std::fs::read(&dest).unwrap(), b"binary", "content copied");
    }

    #[test]
    fn placer_creates_id_subdir() {
        let root = tempfile::tempdir().unwrap();
        let placer = Placer::new("ext2", root.path().to_path_buf());
        assert_eq!(placer.dir(), root.path().join("ext2"));
    }

    #[test]
    fn placer_place_creates_root_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("nested").join("extensions");
        let placer = Placer::new("ext3", root.clone());
        let artifact = tmp.path().join("a.bin");
        std::fs::write(&artifact, b"x").unwrap();
        let dest = placer.place(&artifact).unwrap();
        assert!(dest.is_file());
        assert!(root.join("ext3").is_dir());
    }
}
