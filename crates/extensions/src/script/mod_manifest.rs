//! `mod.toml` manifest + Mods discovery (mirrors `discovery.rs` /
//! `manifest.rs` for the script layer).
//!
//! Discovery walks global + project mods roots, scans subdirectories that
//! carry a `mod.toml`, applies the same entry path-traversal protections as
//! dylib discovery (`discovery.rs:129`), and dedups by canonicalized mod
//! dir. Best-effort: malformed manifests are skipped (warned).

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr as _;

use codesmith_agent::extension::ExtensionError;

/// The `mod.toml` manifest. `entry` defaults to `mod.rhai` when absent.
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct ModManifest {
    pub id: String,
    pub name: Option<String>,
    pub version: String,
    pub description: Option<String>,
    pub entry: Option<String>,
}

impl std::str::FromStr for ModManifest {
    type Err = ExtensionError;

    /// Parse a `mod.toml` document. Returns [`ExtensionError::Load`] on
    /// malformed TOML or a missing required field (`id`/`version`).
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        toml::from_str(text).map_err(|e| ExtensionError::Load(format!("mod manifest parse: {e}")))
    }
}

impl ModManifest {
    /// Parse the `mod.toml` at `path`.
    pub fn parse(path: &Path) -> Result<Self, ExtensionError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ExtensionError::Load(format!("read mod manifest {path:?}: {e}")))?;
        Self::from_str(&text)
    }
}

/// A discovered script mod. `dir` is the mod's own directory;
/// `entry_path` is the resolved `.rhai` entry; `global` distinguishes the
/// shared install root (`~/.codesmith/mods`) from a project-local one
/// (`<workspace>/.codesmith/mods`), which the trust gate treats differently.
#[derive(Debug, Clone)]
pub struct DiscoveredMod {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub dir: PathBuf,
    pub entry_path: PathBuf,
    pub global: bool,
}

/// Default entry filename when `mod.toml` carries no `entry`.
pub(crate) const DEFAULT_ENTRY: &str = "mod.rhai";

/// Mod ids become directory names, state keys and tool-name prefixes of
/// sorts — keep them conservative: ASCII alphanumeric plus `-`, `_`, `.`;
/// no path separators, no whitespace, and never `.`/`..`.
fn valid_mod_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Walk global + project mods roots and discover all script mods. Each root
/// is a container directory of mod subdirectories (only subdirs carrying a
/// `mod.toml` count; bare `.rhai` files are NOT picked up — a mod without a
/// manifest has no activation identity). Dedups by canonicalized mod dir so
/// a mod reached via two roots loads once. Best-effort: malformed manifests
/// / unreadable dirs are skipped with a `tracing::warn`.
pub fn discover_mods(global_roots: &[PathBuf], project_roots: &[PathBuf]) -> Vec<DiscoveredMod> {
    let mut out = Vec::new();
    for root in global_roots {
        discover_mods_in_root(root, true, &mut out);
    }
    for root in project_roots {
        discover_mods_in_root(root, false, &mut out);
    }
    dedup_by_dir(&mut out);
    out
}

/// Scan one mods root (container of mod subdirectories).
fn discover_mods_in_root(root: &Path, global: bool, out: &mut Vec<DiscoveredMod>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let p = entry.path();
        if p.is_dir()
            && p.join("mod.toml").exists()
            && let Some(m) = discover_mod_dir(&p, global)
        {
            out.push(m);
        }
    }
}

/// Parse `mod.toml` under `dir` and resolve the entry script. Returns
/// `None` on parse failure / invalid id / traversal-bearing `entry`
/// (best-effort skip, mirroring `discovery.rs`).
fn discover_mod_dir(dir: &Path, global: bool) -> Option<DiscoveredMod> {
    let manifest = match ModManifest::parse(&dir.join("mod.toml")) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                "skipping mod manifest {}: {e}",
                dir.join("mod.toml").display()
            );
            return None;
        }
    };
    if !valid_mod_id(&manifest.id) {
        tracing::warn!(
            "skipping mod at {}: invalid id {:?}",
            dir.display(),
            manifest.id
        );
        return None;
    }
    let entry_path = match &manifest.entry {
        Some(entry) => {
            // Untrusted data from disk: an absolute path or a `..` component
            // would point execution outside the mod's own dir. Reject the
            // manifest (same guard as dylib `entry`, `discovery.rs:129`).
            let entry_path = Path::new(entry);
            if entry_path.is_absolute()
                || entry_path
                    .components()
                    .any(|component| matches!(component, Component::ParentDir))
            {
                tracing::warn!(
                    "skipping mod at {}: entry {:?} is absolute or contains '..'",
                    dir.display(),
                    entry
                );
                return None;
            }
            dir.join(entry_path)
        }
        None => dir.join(DEFAULT_ENTRY),
    };
    Some(DiscoveredMod {
        name: manifest.name.clone().unwrap_or_else(|| manifest.id.clone()),
        id: manifest.id,
        version: manifest.version,
        description: manifest.description,
        dir: dir.to_path_buf(),
        entry_path,
        global,
    })
}

/// Drop later occurrences of a mod whose canonicalized dir was already seen
/// (a mod reached via two roots loads once). Falls back to the raw path
/// when `canonicalize` fails.
fn dedup_by_dir(out: &mut Vec<DiscoveredMod>) {
    let mut seen = HashSet::new();
    out.retain(|m| {
        let key = m.dir.canonicalize().unwrap_or_else(|_| m.dir.clone());
        seen.insert(key)
    });
}

/// Trust gate (mirrors `discovery::apply_trust_gate`): drops project-local
/// mods when the workspace is untrusted; global mods are retained (shared
/// install provenance implies prior consent).
pub fn apply_mod_trust_gate(mods: Vec<DiscoveredMod>, trust_untrusted: bool) -> Vec<DiscoveredMod> {
    if !trust_untrusted {
        return mods;
    }
    mods.into_iter().filter(|m| m.global).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_mod(root: &Path, id: &str, manifest_extra: &str, entry_body: &str) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("mod.toml"),
            format!("id = \"{id}\"\nversion = \"0.1.0\"\n{manifest_extra}"),
        )
        .expect("write manifest");
        std::fs::write(dir.join(DEFAULT_ENTRY), entry_body).expect("write entry");
        dir
    }

    #[test]
    fn manifest_parse_full() {
        let text = r#"
id = "commit-guard"
name = "Commit Guard"
version = "0.1.0"
description = "拦截危险 git 操作"
entry = "main.rhai"
"#;
        let m = ModManifest::from_str(text).expect("parse");
        assert_eq!(m.id, "commit-guard");
        assert_eq!(m.name.as_deref(), Some("Commit Guard"));
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.description.as_deref(), Some("拦截危险 git 操作"));
        assert_eq!(m.entry.as_deref(), Some("main.rhai"));
    }

    #[test]
    fn manifest_parse_minimal_omits_optionals() {
        let m = ModManifest::from_str("id = \"bare\"\nversion = \"1.0\"\n").expect("parse");
        assert_eq!(m.id, "bare");
        assert!(m.name.is_none());
        assert!(m.description.is_none());
        assert!(m.entry.is_none());
    }

    #[test]
    fn manifest_parse_malformed_is_load_error() {
        let m = ModManifest::from_str("id = \n broken [");
        assert!(matches!(m, Err(ExtensionError::Load(_))), "got {m:?}");
    }

    #[test]
    fn discover_mods_finds_manifest_subdir_with_default_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_mod(dir.path(), "demo", "", "// noop");
        let found = discover_mods(&[dir.path().to_path_buf()], &[]);
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].id, "demo");
        assert_eq!(found[0].name, "demo");
        assert_eq!(found[0].version, "0.1.0");
        assert_eq!(found[0].entry_path, found[0].dir.join("mod.rhai"));
        assert!(found[0].global);
    }

    #[test]
    fn discover_mods_honors_name_and_description() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_mod(
            dir.path(),
            "demo",
            "name = \"Demo\"\ndescription = \"a demo\"",
            "// noop",
        );
        let found = discover_mods(&[dir.path().to_path_buf()], &[]);
        assert_eq!(found[0].name, "Demo");
        assert_eq!(found[0].description.as_deref(), Some("a demo"));
    }

    #[test]
    fn discover_mods_skips_absolute_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let absolute = if cfg!(windows) {
            "C:\\\\evil.rhai"
        } else {
            "/tmp/evil.rhai"
        };
        write_mod(
            dir.path(),
            "evil",
            &format!("entry = \"{absolute}\""),
            "// noop",
        );
        assert!(
            discover_mods(&[dir.path().to_path_buf()], &[]).is_empty(),
            "absolute entry must be skipped"
        );
    }

    #[test]
    fn discover_mods_skips_parent_traversal_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_mod(dir.path(), "evil", "entry = \"../sibling.rhai\"", "// noop");
        assert!(
            discover_mods(&[dir.path().to_path_buf()], &[]).is_empty(),
            "'..' entry must be skipped"
        );
    }

    #[test]
    fn discover_mods_skips_invalid_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("bad");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join("mod.toml"),
            "id = \"bad id\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(sub.join("mod.rhai"), "// noop").unwrap();
        assert!(
            discover_mods(&[dir.path().to_path_buf()], &[]).is_empty(),
            "id with whitespace must be skipped"
        );
    }

    #[test]
    fn discover_mods_allows_relative_entry_without_traversal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = write_mod(dir.path(), "demo", "entry = \"src/main.rhai\"", "// noop");
        std::fs::create_dir_all(sub.join("src")).unwrap();
        std::fs::write(sub.join("src").join("main.rhai"), "// real entry").unwrap();
        let found = discover_mods(&[dir.path().to_path_buf()], &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].entry_path, sub.join("src").join("main.rhai"));
    }

    #[test]
    fn discover_mods_dedups_shared_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_mod(dir.path(), "demo", "", "// noop");
        let root = dir.path().to_path_buf();
        let found = discover_mods(&[root.clone(), root], &[]);
        assert_eq!(found.len(), 1, "expected dedup to 1, got {found:?}");
    }

    #[test]
    fn discover_mods_tags_global_and_project_distinctly() {
        let g = tempfile::tempdir().expect("tempdir");
        let p = tempfile::tempdir().expect("tempdir");
        write_mod(g.path(), "gmod", "", "// noop");
        write_mod(p.path(), "pmod", "", "// noop");
        let found = discover_mods(&[g.path().to_path_buf()], &[p.path().to_path_buf()]);
        assert_eq!(found.len(), 2);
        assert!(found.iter().find(|m| m.id == "gmod").unwrap().global);
        assert!(!found.iter().find(|m| m.id == "pmod").unwrap().global);
    }

    #[test]
    fn discover_mods_ignores_bare_rhai_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("loose.rhai"), "// not a mod").unwrap();
        assert!(discover_mods(&[dir.path().to_path_buf()], &[]).is_empty());
    }

    #[test]
    fn apply_mod_trust_gate_drops_project_local_when_untrusted() {
        let mk = |global| DiscoveredMod {
            id: "x".into(),
            name: "x".into(),
            version: "0".into(),
            description: None,
            dir: PathBuf::from("/x"),
            entry_path: PathBuf::from("/x/mod.rhai"),
            global,
        };
        let mods = vec![mk(true), mk(false), mk(false)];
        assert_eq!(apply_mod_trust_gate(mods.clone(), false).len(), 3);
        let gated = apply_mod_trust_gate(mods, true);
        assert_eq!(gated.len(), 1);
        assert!(gated[0].global);
    }
}
