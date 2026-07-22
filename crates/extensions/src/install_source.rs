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
