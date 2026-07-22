//! `extension.toml` manifest for phase-2 dylib discovery (spec §7.2 / §F5b).
//!
//! A dylib discovered in a subdirectory carries an `extension.toml`
//! declaring `id` / `version` / `entry` (dylib filename, defaults to
//! `<DLL_PREFIX><id>.<DLL_EXTENSION>` when absent — resolved by the
//! loader, not here) / optional `[source]` provenance / optional
//! `api_version` (§F5b Q4: warn-only at load; lockstep is build-enforced,
//! §8.2).

use std::path::Path;

use codesmith_agent::extension::ExtensionError;

/// Provenance recorded in `extension.toml [source]` (§F5c writes real
/// install provenance; §F5b parses + round-trips only).
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct ManifestSource {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
}

/// The `extension.toml` manifest (spec §7.2). `entry` defaults to
/// `<DLL_PREFIX><id>.<DLL_EXTENSION>` when absent (resolved downstream).
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct ExtensionManifest {
    pub id: String,
    pub version: String,
    pub entry: Option<String>,
    pub source: Option<ManifestSource>,
    pub api_version: Option<String>,
}

impl ExtensionManifest {
    /// Parse an `extension.toml` document. Returns
    /// [`ExtensionError::Load`] on malformed TOML or a missing required
    /// field (`id`/`version`).
    pub fn from_str(text: &str) -> Result<Self, ExtensionError> {
        toml::from_str(text).map_err(|e| ExtensionError::Load(format!("manifest parse: {e}")))
    }

    /// Parse the `extension.toml` at `path`.
    pub fn parse(path: &Path) -> Result<Self, ExtensionError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ExtensionError::Load(format!("read manifest {path:?}: {e}")))?;
        Self::from_str(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parse_full() {
        let text = r#"
id = "my-ext"
version = "1.0.0"
entry = "libmy_ext.dylib"
api_version = "0.8"
[source]
type = "git"
ref = "v1.0.0"
"#;
        let m = ExtensionManifest::from_str(text).expect("parse");
        assert_eq!(m.id, "my-ext");
        assert_eq!(m.version, "1.0.0");
        assert_eq!(m.entry.as_deref(), Some("libmy_ext.dylib"));
        assert_eq!(m.api_version.as_deref(), Some("0.8"));
        let src = m.source.expect("source");
        assert_eq!(src.kind, "git");
        assert_eq!(src.ref_.as_deref(), Some("v1.0.0"));
    }

    #[test]
    fn manifest_parse_minimal_omits_optionals() {
        let text = r#"
id = "bare"
version = "0.1.0"
"#;
        let m = ExtensionManifest::from_str(text).expect("parse");
        assert_eq!(m.id, "bare");
        assert_eq!(m.version, "0.1.0");
        assert!(m.entry.is_none());
        assert!(m.source.is_none());
        assert!(m.api_version.is_none());
    }

    #[test]
    fn manifest_parse_malformed_is_load_error() {
        let m = ExtensionManifest::from_str("id = \n broken [");
        assert!(matches!(m, Err(ExtensionError::Load(_))), "got {m:?}");
    }
}
