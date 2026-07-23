//! Install-source abstraction **traits** (spec §6.4). Impls are §F5
//! (dylib loading). Slice 1 ships only the trait shapes so the §F1
//! `/extension install` stub (Task 8) can reference
//! [`ExtensionError::Install`](codesmith_agent::extension::ExtensionError)
//! and so the ROADMAP §F5 entry has a stable contract to point at.

use std::path::{Path, PathBuf};

use codesmith_agent::extension::ExtensionError;

/// A fetched install artifact (path + provenance string for
/// `ExtensionStateStore.installed`).
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
