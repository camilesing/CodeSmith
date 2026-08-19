# §F5c — Dylib INSTALL 侧 (phase 2 续作·下) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the dylib INSTALL side — fetch (Git/LocalPath) → build (CargoBuilder) → place (Placer) → write `extension.toml` + `installed[]` provenance — and replace the `/extension install`/`uninstall` stubs with real impls.

**Architecture:** A pure `Installer` orchestrator (extensions crate) coordinates `&dyn ExtensionSource` → `&dyn ExtensionBuilder` → D8 temp-load `metadata()` for id/version → `Placer` (copies dylib) → writes `extension.toml`. The tui command computes the root (scope + workspace + `effective_home_dir`), drives state mutators (`add_installed`/`remove_installed`), and emits the trust-warn. `CratesIo`/`Prebuilt` short-circuit to a "§F5c-later" error. Trait-DI lets the e2e inject `FakeSource`+`FakeBuilder` (returns the §F5b fixture dylib) to prove install→discover→load→bind without a real `cargo build`.

**Tech Stack:** rustc 1.90.0 / edition 2024, plain `cargo` (no toolchain pin), `libloading` (§F5b), `toml` (§F5b), `tempfile` (promoted to real dep of `codesmith-extensions`), `std::process::Command` (git/cargo shell-out), no new external crate deps.

**Spec:** `docs/superpowers/specs/2026-07-23-codesmith-extension-system-slice-5c-design.md`

**Spec reconciliation notes (deviations from approved spec, record in commit messages):**
- **R1 Installer purity:** Installer (extensions crate) holds only `source + builder + root`. `state` mutators + trust-warn + scope→root live in the tui command (extensions cannot depend on tui; tui depends on extensions). Spec §4's `state`/`scope` Installer fields were a layering error.
- **R2 CargoBuilder glob, no JSON:** scan `target/release/*.<DLL_EXTENSION>` for the cdylib instead of `--message-format=json` (avoids a new `serde_json` dep; spec §8 "no new dep" honored). Robust for single-cdylib crates.
- **R3 manifest write in Installer:** `Placer` only copies the dylib (trait `place(artifact)` has no version/source); the Installer writes `extension.toml` (it has id/version/source).
- **R4 crate/prebuilt short-circuit in command:** the command early-returns the "§F5c-later" error; the Installer is pure (no `SourceKind` check). Spec §6 step 2 said "UnimplementedSource → fetch Err"; command-guard is cleaner.
- **R5 e2e no state assertion:** the e2e is in the extensions crate (to access the `CODESMITH_FIXTURE_DYLIB` env from §F5b's `build.rs`); it asserts install→discover→load→bind, NOT `installed[]` (state is tui-side, unit-tested in T2). Spec §9 e2e row mentioned `installed[]` — dropped here.
- **R6 tempfile promoted** from dev-dep to real dep of `codesmith-extensions` (Installer uses `tempdir()` at runtime; not a new external dep).

---

## File Structure (created / modified)

- `crates/extensions/src/install_source.rs` (modify): add `SourceSpec` + `SourceKind` + `InstallScope` (T1); `GitSource` + `LocalPathSource` (T3); `CargoBuilder` (T4); `Placer` (T5).
- `crates/extensions/src/installer.rs` (NEW): `Installer` orchestrator + `InstallReport`/`UninstallReport` + e2e (T6).
- `crates/extensions/src/discovery.rs` (modify): `default_dylib_filename` → `pub(crate)`; fix `:181` stale comment (T5).
- `crates/extensions/src/lib.rs` (modify): re-export new public items (T6).
- `crates/extensions/Cargo.toml` (modify): promote `tempfile` to real dep (T6).
- `crates/tui/src/extension_state.rs` (modify): `installed` → `BTreeMap` + mutators (T2).
- `crates/tui/src/commands/extension_commands.rs` (modify): real `install`/`uninstall` + `install_precheck` (T7).
- `docs/EXTENSIONS.md` + `ROADMAP.md` (modify): §F5c host-seam + Sandbox Stance + progress block + `### F5c` subsection (T7).

---

## Task 1: `SourceSpec` parser + `InstallScope` + `SourceKind`

**Files:**
- Modify: `crates/extensions/src/install_source.rs` (append to existing file)

- [ ] **Step 1: Write the failing tests**

Append to `crates/extensions/src/install_source.rs`:
```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codesmith-extensions source_spec_tests`
Expected: FAIL — `SourceSpec`/`SourceKind`/`InstallScope` not found (compile error).

- [ ] **Step 3: Write minimal implementation**

Add to the top section of `crates/extensions/src/install_source.rs` (after the existing `use` lines):
```rust
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
    CratesIo,   // §F5c stubbed (nice-to-have)
    Prebuilt,   // §F5c stubbed (nice-to-have)
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
            .ok_or_else(|| ExtensionError::Install("missing source spec (expected `<kind>:<body>[@<ref>]`)".into()))?;
        let (kind_str, rest) = spec_token
            .split_once(':')
            .ok_or_else(|| ExtensionError::Install(format!("source spec must be `<kind>:<body>`; got {spec_token:?}")))?;
        let kind = match kind_str {
            "git" => SourceKind::Git,
            "path" => SourceKind::Path,
            "crate" => SourceKind::CratesIo,
            "prebuilt" => SourceKind::Prebuilt,
            other => return Err(ExtensionError::Install(format!("unknown source kind {other:?}; expected git|path|crate|prebuilt"))),
        };
        let (body, ref_) = match rest.rsplit_once('@') {
            Some((b, r)) if !r.is_empty() => (b.to_string(), Some(r.to_string())),
            _ => (rest.to_string(), None),
        };
        if body.is_empty() {
            return Err(ExtensionError::Install("source body is empty".into()));
        }
        Ok(SourceSpec { kind, body, ref_, scope })
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-extensions source_spec_tests`
Expected: PASS — 10 tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/extensions/src/install_source.rs
git commit -m "feat(framework): §F5c T1 SourceSpec parser + InstallScope + SourceKind (prefix grammar git:/path: + --global flag; crate/prebuilt recognized→§F5c stub; 10 Red→Green tests; API reconciliation: spec §4 Installer state/scope fields were layering error→R1 Installer pure; no trait changes)"
```

---

## Task 2: `ExtensionStateStore.installed` → `BTreeMap` + mutators

**Files:**
- Modify: `crates/tui/src/extension_state.rs:33-48` (struct + OnDiskState) + `:106-108` (reader) + tests

- [ ] **Step 1: Write the failing tests**

Append to the `tests` mod in `crates/tui/src/extension_state.rs`:
```rust
    #[test]
    fn add_installed_persists_and_provenance_for_reads() {
        let (dir, mut store) = fresh();
        store.add_installed("my-ext", "git:github.com/foo/bar@v1").unwrap();
        assert_eq!(store.provenance_for("my-ext").as_deref(), Some("git:github.com/foo/bar@v1"));
        let reloaded = ExtensionStateStore::load_from(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert_eq!(reloaded.provenance_for("my-ext").as_deref(), Some("git:github.com/foo/bar@v1"));
        assert!(reloaded.installed_ids().contains(&"my-ext".to_string()));
    }

    #[test]
    fn remove_installed_persists() {
        let (dir, mut store) = fresh();
        store.add_installed("a", "git:x@v1").unwrap();
        store.add_installed("b", "git:y@v2").unwrap();
        store.remove_installed("a").unwrap();
        assert!(store.provenance_for("a").is_none());
        assert!(store.provenance_for("b").is_some());
        let reloaded = ExtensionStateStore::load_from(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(reloaded.provenance_for("a").is_none());
        assert!(reloaded.provenance_for("b").is_some());
    }

    #[test]
    fn installed_persists_as_toml_table() {
        let (dir, mut store) = fresh();
        store.add_installed("ext", "path:/abs").unwrap();
        let raw = std::fs::read_to_string(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(
            raw.contains("[installed]") || raw.contains("installed = {"),
            "installed must serialize as a TOML table: {raw}"
        );
        assert!(raw.contains("ext"));
        assert!(raw.contains("path:/abs"));
    }

    #[test]
    fn installed_ids_deterministic_order() {
        let (_dir, mut store) = fresh();
        store.add_installed("zeta", "git:z@1").unwrap();
        store.add_installed("alpha", "git:a@2").unwrap();
        let mut ids = store.installed_ids();
        ids.sort();
        assert_eq!(ids, vec!["alpha".to_string(), "zeta".to_string()]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codesmith-tui --bin codesmith-tui installed_`
Expected: FAIL — `add_installed`/`provenance_for`/`installed_ids` not found (compile error).

- [ ] **Step 3: Write minimal implementation**

In `crates/tui/src/extension_state.rs`:

Change the `installed` field type (line ~39):
```rust
use std::collections::BTreeMap;
```
(add `BTreeMap` to the existing `use std::collections::BTreeSet;` line — change to `use std::collections::{BTreeMap, BTreeSet};`)

Replace the `installed` field in `ExtensionStateStore`:
```rust
    /// §F5c: install-source provenance keyed by extension id (e.g.
    /// `"fixture-dylib" -> "git:github.com/foo/bar@v1"`). §F5b read/wrote it
    /// as a `BTreeSet<String>` for forward-compat; §F5c changes to a map so
    /// `/extension uninstall <id>` can remove by id. No migration: §F5b never
    /// populated it (no real data on disk).
    installed: BTreeMap<String, String>,
```

Replace `OnDiskState.installed` (line ~47):
```rust
    #[serde(default)]
    installed: BTreeMap<String, String>,
```

In `load_from`, the `Self { ... installed: ... }` arms — replace `installed: BTreeSet::new()` with `installed: BTreeMap::new()`, and the parsed arm `installed: parsed.installed.into_iter().collect()` with `installed: parsed.installed` (already a map).

Replace the `installed()` reader + add mutators (line ~106):
```rust
    /// Provenance strings for installed extensions (back-compat: returns the
    /// values). §F5c keys by id internally; use `installed_ids()` for keys.
    pub fn installed(&self) -> Vec<String> {
        self.installed.values().cloned().collect()
    }

    /// Ids of installed extensions (§F5c).
    pub fn installed_ids(&self) -> Vec<String> {
        self.installed.keys().cloned().collect()
    }

    /// Provenance for one installed extension id (§F5c).
    pub fn provenance_for(&self, id: &str) -> Option<String> {
        self.installed.get(id).cloned()
    }

    /// Record install provenance for `id` (§F5c). Overwrites if reinstalled.
    pub fn add_installed(&mut self, id: &str, provenance: &str) -> Result<()> {
        self.installed.insert(id.to_string(), provenance.to_string());
        self.persist()
    }

    /// Remove install provenance for `id` (§F5c). No-op if absent.
    pub fn remove_installed(&mut self, id: &str) -> Result<()> {
        self.installed.remove(id);
        self.persist()
    }
```

In `persist`, the `OnDiskState { disabled: ..., installed: ... }` — change `installed: self.installed.iter().cloned().collect()` to `installed: self.installed.clone()`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-tui --bin codesmith-tui extension_state`
Expected: PASS — existing 6 + 4 new = 10 tests green. Run the whole tui suite to confirm no regression (expect 2829 pass/26 pre-existing runtime_api fail/2 ignored + 4 new = 2833 pass):

Run: `cargo test -p codesmith-tui --bin codesmith-tui 2>&1 | tail -5`

- [ ] **Step 5: Commit**

```bash
git add crates/tui/src/extension_state.rs
git commit -m "feat(framework): §F5c T2 installed→BTreeMap<id,provenance> + mutators (add_installed/remove_installed/provenance_for/installed_ids; OnDiskState.installed→TOML table; no migration—§F5b never populated; 4 Red→Green tests; tui 2829→2833 pass/26 pre-existing runtime_api fail/2 ignored unchanged—no §F5c regression)"
```

---

## Task 3: `GitSource` + `LocalPathSource` (`ExtensionSource` impls)

**Files:**
- Modify: `crates/extensions/src/install_source.rs` (add impls + tests)

- [ ] **Step 1: Write the failing tests**

Append to `crates/extensions/src/install_source.rs`:
```rust
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
        if std::process::Command::new("git").arg("--version").output().is_err() {
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
        std::fs::write(src.path().join("Cargo.toml"), b"[package]\nname=\"x\"\nversion=\"0.1.0\"\n").unwrap();
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codesmith-extensions source_impl_tests`
Expected: FAIL — `GitSource`/`LocalPathSource` not found (compile error).

- [ ] **Step 3: Write minimal implementation**

Add to `crates/extensions/src/install_source.rs` (top, after `use` lines — extend the existing `use std::path::{Path, PathBuf};` is already there; add `use std::process::Command;`):
```rust
use std::process::Command;

/// Git install source (§F5c must-have). `git clone --depth 1 [--branch <ref>]`.
pub struct GitSource {
    pub url: String,
    pub ref_: Option<String>,
}

impl GitSource {
    pub fn new(url: impl Into<String>, ref_: Option<String>) -> Self {
        Self { url: url.into(), ref_ }
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
        let out = cmd.output().map_err(|e| ExtensionError::Install(format!("spawn git (on PATH?): {e}")))?;
        if !out.status.success() {
            return Err(ExtensionError::Install(format!(
                "git clone {} failed: {}",
                self.url,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(SourceArtifact { path: dest.to_path_buf(), provenance: self.provenance() })
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
            return Err(ExtensionError::Install(format!("path source not a dir: {}", self.dir.display())));
        }
        copy_dir_recursive(&self.dir, dest)?;
        let canon = std::fs::canonicalize(&self.dir).unwrap_or_else(|_| self.dir.clone());
        Ok(SourceArtifact { path: dest.to_path_buf(), provenance: format!("path:{}", canon.display()) })
    }
}

/// Recursive dir copy (§F5c LocalPathSource.fetch). std has no recursive copy.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), ExtensionError> {
    std::fs::create_dir_all(dst).map_err(|e| ExtensionError::Install(format!("mkdir {}: {e}", dst.display())))?;
    for entry in std::fs::read_dir(src).map_err(|e| ExtensionError::Install(format!("read_dir {}: {e}", src.display())))? {
        let entry = entry.map_err(|e| ExtensionError::Install(format!("dir entry: {e}")))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| ExtensionError::Install(format!("copy {}: {e}", from.display())))?;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-extensions source_impl_tests`
Expected: PASS — 5 tests green (`git_source_fetch_invalid_url_is_install_error` may pass via no-git-skip or invalid-host).

- [ ] **Step 5: Commit**

```bash
git add crates/extensions/src/install_source.rs
git commit -m "feat(framework): §F5c T3 GitSource + LocalPathSource (ExtensionSource impls; git clone --depth 1 [--branch ref]; recursive dir copy; provenance git:<url>[@ref] / path:<abs>; 5 Red→Green tests; no new dep—std::process::Command + std::fs only)"
```

---

## Task 4: `CargoBuilder` (`ExtensionBuilder` impl, glob scan)

**Files:**
- Modify: `crates/extensions/src/install_source.rs` (add `CargoBuilder` + tests)

- [ ] **Step 1: Write the failing test**

Append to `crates/extensions/src/install_source.rs`:
```rust
#[cfg(test)]
mod cargo_builder_tests {
    use super::*;

    /// Build a tiny standalone cdylib crate in a TempDir + assert CargoBuilder
    /// produces the cdylib. Skips if `cargo` not on PATH (CI without rust
    /// toolchain). Uses a temp `--target-dir` (no workspace lock conflict).
    #[test]
    fn cargo_builder_builds_tiny_cdylib() {
        if std::process::Command::new("cargo").arg("--version").output().is_err() {
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
    fn cargo_builder_missing_cargo_is_install_error() {
        // Can't easily strip PATH for std::process::Command; instead point at a
        // dir with no Cargo.toml → cargo build fails → Install error.
        let empty = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let builder = CargoBuilder::new(target.path().to_path_buf());
        let r = builder.build(empty.path());
        assert!(matches!(r, Err(ExtensionError::Install(_))), "got {r:?}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p codesmith-extensions cargo_builder_tests`
Expected: FAIL — `CargoBuilder` not found (compile error).

- [ ] **Step 3: Write minimal implementation**

Add to `crates/extensions/src/install_source.rs`:
```rust
/// Build a fetched source into a cdylib via `cargo build` (§F5c).
/// `cargo build --release --locked --target-dir <temp>`, then scan
/// `target/release/` for the platform cdylib (`.<DLL_EXTENSION>`). No JSON
/// parse (R2: avoids a serde_json dep). Robust for single-cdylib crates;
/// errors on 0 or >1 cdylib.
pub struct CargoBuilder {
    target_dir: PathBuf,
}

impl CargoBuilder {
    pub fn new(target_dir: impl Into<PathBuf>) -> Self {
        Self { target_dir: target_dir.into() }
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
        let mut found: Vec<PathBuf> = std::fs::read_dir(&release_dir)
            .map_err(|e| ExtensionError::Install(format!("read release dir {}: {e}", release_dir.display())))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension().and_then(|e| e.to_str()) == Some(std::env::consts::DLL_EXTENSION)
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-extensions cargo_builder_tests`
Expected: PASS — `cargo_builder_builds_tiny_cdylib` (builds a real tiny cdylib, ~5-10s) + `cargo_builder_missing_cargo_is_install_error` green.

- [ ] **Step 5: Commit**

```bash
git add crates/extensions/src/install_source.rs
git commit -m "feat(framework): §F5c T4 CargoBuilder (cargo build --release --locked --target-dir temp; scan target/release/*.<DLL_EXT> for cdylib—no JSON/serde_json per R2; 0 or >1 cdylib→Install error; 2 Red→Green tests incl real tiny-cdylib build; no new dep)"
```

---

## Task 5: `Placer` + `default_dylib_filename` pub(crate) + discovery comment fix

**Files:**
- Modify: `crates/extensions/src/discovery.rs:50` (`default_dylib_filename` → `pub(crate)`) + `:181` (comment fix)
- Modify: `crates/extensions/src/install_source.rs` (add `Placer` + tests)

- [ ] **Step 1: Write the failing tests**

Append to `crates/extensions/src/install_source.rs`:
```rust
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
        let expected = root.path().join("my-ext").join(crate::discovery::default_dylib_filename("my-ext"));
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codesmith-extensions placer_tests`
Expected: FAIL — `Placer` not found + `default_dylib_filename` private (compile error).

- [ ] **Step 3: Write minimal implementation**

In `crates/extensions/src/discovery.rs`, change the `default_dylib_filename` visibility (line ~50) — replace `fn default_dylib_filename` with `pub(crate) fn default_dylib_filename`:
```rust
pub(crate) fn default_dylib_filename(id: &str) -> String {
```

Fix the stale `:181` comment in `apply_trust_gate` — replace the trailing parenthetical:
```rust
/// this to keep project-*configured* sources even when untrusted.)
```
with:
```rust
/// this unchanged — §F5c keeps Model A (no configured-path concept);
/// `apply_trust_gate` is final as-is for the install/load path.)
```

Add `Placer` to `crates/extensions/src/install_source.rs`:
```rust
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
        Self { id: id.into(), root: root.into() }
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-extensions placer_tests`
Expected: PASS — 3 tests green. Also re-run discovery tests to confirm `pub(crate)` change + comment fix don't break:
Run: `cargo test -p codesmith-extensions dylib_tests`
Expected: 5 dylib_tests still green.

- [ ] **Step 5: Commit**

```bash
git add crates/extensions/src/discovery.rs crates/extensions/src/install_source.rs
git commit -m "feat(framework): §F5c T5 Placer + default_dylib_filename pub(crate) + discovery:181 comment fix (Placer copies dylib→<root>/<id>/<default_dylib_filename(id)>; manifest written by Installer per R3; default_dylib_filename pub(crate) so Placer+discover share filename; fix stale §F5b comment '§F5c refines to keep project-configured'→'§F5c keeps Model A as-is' since configured-paths out-of-scope; 3 Red→Green tests; dylib_tests 5 unchanged)"
```

---

## Task 6: `Installer` orchestrator + `lib.rs` re-export + `Cargo.toml` tempfile + e2e

**Files:**
- Create: `crates/extensions/src/installer.rs`
- Modify: `crates/extensions/src/lib.rs` (re-export + `mod installer`)
- Modify: `crates/extensions/Cargo.toml` (promote `tempfile` to real dep)

- [ ] **Step 1: Write the failing e2e test**

Create `crates/extensions/src/installer.rs` with the test first (impl as `todo!()`):
```rust
//! §F5c install orchestrator. Coordinates fetch → build → D8 temp-load
//! `metadata()` for id/version → `Placer` (copies dylib) → write
//! `extension.toml`. Pure: no state/config (those are tui-layer; R1). The
//! `Placer` is constructed inside `install()` after D8 yields the id (id is
//! unknown until the built dylib's `metadata()` is read).

use std::path::{Path, PathBuf};

use codesmith_agent::extension::{Extension, ExtensionError};

use crate::install_source::{
    ExtensionBuilder, ExtensionPlacer, ExtensionSource, InstallReport, InstallScope, Placer,
    SourceArtifact, SourceSpec,
};

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
        Self { source, builder, root }
    }

    pub fn install(&self, spec: &SourceSpec) -> Result<InstallReport, ExtensionError> {
        todo!()
    }

    /// Remove `<root>/<id>/` from any of `roots`. Returns `removed` bool.
    pub fn uninstall_files(id: &str, roots: &[PathBuf]) -> Result<UninstallReport, ExtensionError> {
        todo!()
    }
}

pub struct InstallReport {
    pub id: String,
    pub version: String,
    pub path: PathBuf,
    pub provenance: String,
}

pub struct UninstallReport {
    pub id: String,
    pub removed: bool,
}

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use crate::install_source::*;
    use async_trait::async_trait;
    use codesmith_agent::extension::*;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct Ctx { generation: u64 }
    #[async_trait]
    impl ExtensionContext for Ctx {
        fn cwd(&self) -> &Path { Path::new(".") }
        fn mode(&self) -> ExtensionMode { ExtensionMode::Tui }
        fn is_idle(&self) -> bool { true }
        fn signal(&self) -> CancellationToken { CancellationToken::new() }
        fn generation(&self) -> u64 { self.generation }
    }
    impl ExtensionCommandContext for Ctx {}

    struct FakeSource { provenance: String }
    impl ExtensionSource for FakeSource {
        fn fetch(&self, dest: &Path) -> Result<SourceArtifact, ExtensionError> {
            Ok(SourceArtifact { path: dest.to_path_buf(), provenance: self.provenance.clone() })
        }
    }
    struct FakeBuilder { dylib: PathBuf }
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
        let source = FakeSource { provenance: "test:fake".into() };
        let builder = FakeBuilder { dylib: PathBuf::from(fixture) };
        let installer = Installer::new(&source, &builder, root.path().to_path_buf());
        let spec = SourceSpec::parse("path:/ignored").unwrap();
        let report = installer.install(&spec).expect("install");
        assert_eq!(report.id, "fixture-dylib", "id from fixture metadata (D8)");
        assert!(report.path.is_file(), "placed dylib exists: {}", report.path.display());
        let manifest_path = root.path().join("fixture-dylib").join("extension.toml");
        assert!(manifest_path.is_file(), "manifest written");
        let manifest_text = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(manifest_text.contains("id = \"fixture-dylib\""), "manifest id: {manifest_text}");
        assert!(manifest_text.contains("[source]"), "manifest source: {manifest_text}");
        assert_eq!(report.provenance, "test:fake");

        let found = crate::discover_dylib(&[root.path().to_path_buf()], &[]);
        assert_eq!(found.len(), 1, "discover finds 1: {found:?}");
        assert_eq!(found[0].id, "fixture-dylib");
        assert!(found[0].config_path.is_some(), "manifest-subdir (not bare)");

        let runner = crate::ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load_dylib(&found[0].dylib_path)).expect("load placed dylib");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let tools: Vec<String> = runner.bound_tools().into_iter().map(|(n, _)| n).collect();
        assert!(tools.iter().any(|n| n == "fixture_echo"), "fixture_echo bound: {tools:?}");
    }

    #[test]
    fn uninstall_files_removes_id_dir() {
        let root = tempfile::tempdir().unwrap();
        let placer = Placer::new("gone", root.path().to_path_buf());
        let artifact = root.path().join("a.bin");
        std::fs::write(&artifact, b"x").unwrap();
        placer.place(&artifact).unwrap();
        assert!(root.path().join("gone").exists());
        let report = Installer::uninstall_files("gone", &[root.path().to_path_buf()]).unwrap();
        assert!(report.removed);
        assert!(!root.path().join("gone").exists());
        let report2 = Installer::uninstall_files("absent", &[root.path().to_path_buf()]).unwrap();
        assert!(!report2.removed);
    }
}
```

NOTE: `InstallReport`/`InstallScope`/`SourceArtifact` are imported from `crate::install_source`, so they must be defined there (InstallReport is defined in installer.rs here — fix: define `InstallReport`/`UninstallReport` in installer.rs + import `InstallScope`/`SourceArtifact`/`Placer`/`SourceSpec` from install_source). The `use crate::install_source::{ ... InstallReport, InstallScope ...}` line is wrong — `InstallReport` is in installer.rs. Correct the import to:
```rust
use crate::install_source::{
    ExtensionBuilder, ExtensionPlacer, ExtensionSource, Placer, SourceArtifact, SourceSpec,
};
```
(`InstallScope` is unused in installer.rs after R1 — drop it. `ExtensionPlacer` unused too unless Placer uses the trait — the `Placer` impl is in install_source.rs; installer.rs constructs `Placer` directly, doesn't need the trait import. Drop `ExtensionPlacer`.)

- [ ] **Step 2: Wire the module + run test to verify it fails (todo! panic)**

In `crates/extensions/src/lib.rs`, add:
```rust
pub mod installer;
```
and re-export:
```rust
pub use installer::{InstallReport, Installer, UninstallReport};
```

In `crates/extensions/Cargo.toml`, move `tempfile` from `[dev-dependencies]` to `[dependencies]` (if it's in dev-deps) — or add `tempfile.workspace = true` to `[dependencies]`. Check current placement first:
Run: `grep -n tempfile crates/extensions/Cargo.toml`
If under `[dev-dependencies]`, move the line under `[dependencies]`. R6.

Run: `cargo test -p codesmith-extensions e2e_tests`
Expected: FAIL — `install` / `uninstall_files` are `todo!()` → panic "not yet implemented".

- [ ] **Step 3: Write minimal implementation**

Replace the `todo!()` bodies in `crates/extensions/src/installer.rs`:
```rust
    pub fn install(&self, spec: &SourceSpec) -> Result<InstallReport, ExtensionError> {
        // 3. fetch
        let dest = tempfile::tempdir()
            .map_err(|e| ExtensionError::Install(format!("tempdir for fetch: {e}")))?;
        let art: SourceArtifact = self.source.fetch(dest.path())?;
        // 4. build
        let cdylib = self.builder.build(&art.path)?;
        // 5. D8: temp-load metadata → (id, version), then drop (no configure/register).
        let (_lib, ext_box) = crate::loader::load_dylib(&cdylib)?;
        let metadata = ext_box.metadata();
        let id = metadata.id.clone();
        let version = metadata.version.clone();
        drop(metadata);
        drop(ext_box);
        drop(_lib);
        // 6. place (Placer constructed here — id from D8, R1/R3).
        let placer = Placer::new(&id, &self.root);
        let placed = placer.place(&cdylib)?;
        // 7. write extension.toml (id/version/source; entry omitted → discover
        //    resolves default_dylib_filename(id), matching the placed file).
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

    pub fn uninstall_files(id: &str, roots: &[PathBuf]) -> Result<UninstallReport, ExtensionError> {
        let mut removed = false;
        for root in roots {
            let dir = root.join(id);
            if dir.exists() {
                std::fs::remove_dir_all(&dir)
                    .map_err(|e| ExtensionError::Install(format!("remove {}: {e}", dir.display())))?;
                removed = true;
            }
        }
        Ok(UninstallReport { id: id.to_string(), removed })
    }
```

Add the helper (top of file, after imports):
```rust
fn manifest_kind(spec: &SourceSpec) -> &'static str {
    match spec.kind {
        crate::install_source::SourceKind::Git => "git",
        crate::install_source::SourceKind::Path => "path",
        crate::install_source::SourceKind::CratesIo => "crate",
        crate::install_source::SourceKind::Prebuilt => "prebuilt",
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-extensions e2e_tests`
Expected: PASS — `install_to_load_roundtrip_binds_fixture_tool` (full round-trip via the §F5b fixture) + `uninstall_files_removes_id_dir` green.

Run the full extensions suite to confirm no regression:
Run: `cargo test -p codesmith-extensions 2>&1 | tail -5`
Expected: 26 (§F5b) + 10 (T1) + 5 (T3) + 2 (T4) + 3 (T5) + 2 (T6) = 48 tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/extensions/src/installer.rs crates/extensions/src/lib.rs crates/extensions/Cargo.toml
git commit -m "feat(framework): §F5c T6 Installer orchestrator + install→load e2e (pure Installer: source+builder+root; fetch→build→D8 temp-load metadata→Placer→write manifest; uninstall_files(id,roots); R1 no state/config in Installer (tui-layer), R3 manifest in Installer not Placer, R5 e2e no state assertion; lib.rs re-export; tempfile promoted to real dep R6; 2 Red→Green tests incl full round-trip via §F5b fixture dylib→discover→load→fixture_echo bound; ext 26→48)"
```

---

## Task 7: `/extension install`/`uninstall` real impl + docs

**Files:**
- Modify: `crates/tui/src/commands/extension_commands.rs:22-47` (dispatch) + `:201-211` (stubs) + tests
- Modify: `docs/EXTENSIONS.md` (host-seam install row + Sandbox Stance §F5c)
- Modify: `ROADMAP.md` (§F5c progress block + `### F5c` subsection)

- [ ] **Step 1: Write the failing tests**

In `crates/tui/src/commands/extension_commands.rs`, replace the existing `install_stub`/`uninstall_stub` tests in the `tests` mod:
```rust
    #[test]
    fn install_precheck_missing_arg_is_usage_error() {
        let r = install_precheck("");
        assert!(r.is_some());
        assert!(r.unwrap().is_error);
    }

    #[test]
    fn install_precheck_crate_kind_is_not_yet_implemented() {
        let r = install_precheck("crate:my-ext");
        assert!(r.is_some());
        let r = r.unwrap();
        assert!(r.is_error);
        assert!(r.message.unwrap().contains("§F5c-later"));
    }

    #[test]
    fn install_precheck_prebuilt_kind_is_not_yet_implemented() {
        let r = install_precheck("prebuilt:https://x/y.dylib");
        assert!(r.is_some());
        assert!(r.unwrap().is_error);
    }

    #[test]
    fn install_precheck_git_path_proceeds_none() {
        assert!(install_precheck("git:github.com/foo/bar").is_none());
        assert!(install_precheck("path:/abs/dir").is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codesmith-tui --bin codesmith-tui install_precheck`
Expected: FAIL — `install_precheck` not found + old `install_stub`/`uninstall_stub` tests removed (compile error: the old tests reference removed fns).

- [ ] **Step 3: Write minimal implementation**

In `crates/tui/src/commands/extension_commands.rs`, replace `install_stub`/`uninstall_stub` (line ~201) with:
```rust
/// Pre-App validation for `/extension install`: parse + crate/prebuilt guard.
/// Returns `Some(error)` for bad args / not-yet-implemented kinds; `None` to
/// proceed with the App. R4: crate/prebuilt short-circuit here, not in Installer.
fn install_precheck(arg: &str) -> Option<CommandResult> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Some(CommandResult::error(
            "Usage: /extension install <kind>:<body>[@<ref>] [--global]  (kinds: git, path)",
        ));
    }
    let spec = match codesmith_extensions::SourceSpec::parse(arg) {
        Ok(s) => s,
        Err(e) => return Some(CommandResult::error(format!("Invalid source spec: {e}"))),
    };
    if matches!(
        spec.kind,
        codesmith_extensions::SourceKind::CratesIo | codesmith_extensions::SourceKind::Prebuilt
    ) {
        return Some(CommandResult::error(format!(
            "§F5c-later: {:?} source not yet implemented (this slice supports git/path only)",
            spec.kind
        )));
    }
    None
}

/// Compute the extensions root for a scope (§F5c). Global =
/// `~/.codesmith/extensions` (falls back to project if no home); Project =
/// `<workspace>/.codesmith/extensions`.
fn extensions_root_for(scope: codesmith_extensions::InstallScope, workspace: &std::path::Path) -> std::path::PathBuf {
    match scope {
        codesmith_extensions::InstallScope::Global => crate::config::effective_home_dir()
            .map(|h| h.join(".codesmith").join("extensions"))
            .unwrap_or_else(|| workspace.join(".codesmith").join("extensions")),
        codesmith_extensions::InstallScope::Project => workspace.join(".codesmith").join("extensions"),
    }
}

fn install(app: &mut App, arg: &str) -> CommandResult {
    if let Some(err) = install_precheck(arg) {
        return err;
    }
    // Precheck passed → spec is valid + git/path.
    let spec = codesmith_extensions::SourceSpec::parse(arg).expect("precheck validated");
    let root = extensions_root_for(spec.scope, &app.workspace);
    // Construct the source + builder.
    let source: Box<dyn codesmith_extensions::ExtensionSource> = match spec.kind {
        codesmith_extensions::SourceKind::Git => {
            Box::new(codesmith_extensions::GitSource::new(spec.body.clone(), spec.ref_.clone()))
        }
        codesmith_extensions::SourceKind::Path => {
            Box::new(codesmith_extensions::LocalPathSource::new(spec.body.clone()))
        }
        _ => unreachable!("precheck rejected crate/prebuilt"),
    };
    let build_target = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => return CommandResult::error(format!("tempdir for build: {e}")),
    };
    let builder = codesmith_extensions::CargoBuilder::new(build_target.path().to_path_buf());
    let installer = codesmith_extensions::Installer::new(source.as_ref(), &builder, root.clone());
    let report = match installer.install(&spec) {
        Ok(r) => r,
        Err(e) => return CommandResult::error(format!("install failed: {e}")),
    };
    // Record provenance (tui-side state mutator; R1).
    if let Err(e) = app.extension_state.add_installed(&report.id, &report.provenance) {
        return CommandResult::error(format!("installed but state write failed: {e}"));
    }
    // Trust-warn (R1: install is trust-agnostic; warn if project + untrusted).
    let will_load = match spec.scope {
        codesmith_extensions::InstallScope::Global => true,
        codesmith_extensions::InstallScope::Project => crate::config::is_workspace_trusted(&app.workspace),
    };
    let trust_note = if will_load {
        String::new()
    } else {
        format!("\n⚠ won't load until the workspace is trusted (accept the trust prompt or /trust, then /extension reload).")
    };
    CommandResult::message(format!(
        "Installed extension '{}' (v{}) to {}.\nprovenance: {}\nRun /extension reload to load it.{}",
        report.id,
        report.version,
        report.path.display(),
        report.provenance,
        trust_note,
    ))
}

fn uninstall(app: &mut App, arg: &str) -> CommandResult {
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /extension uninstall <id>");
    }
    // Search both roots (convention-based location; state doesn't store scope).
    let project_root = app.workspace.join(".codesmith").join("extensions");
    let mut roots = vec![project_root];
    if let Some(h) = crate::config::effective_home_dir() {
        roots.push(h.join(".codesmith").join("extensions"));
    }
    let report = match codesmith_extensions::Installer::uninstall_files(id, &roots) {
        Ok(r) => r,
        Err(e) => return CommandResult::error(format!("uninstall failed: {e}")),
    };
    if let Err(e) = app.extension_state.remove_installed(id) {
        return CommandResult::error(format!("files removed but state write failed: {e}"));
    }
    if report.removed {
        CommandResult::message(format!(
            "Uninstalled extension '{id}'.\n⚠ tools/commands remain bound until process restart (bounded retention, §F5b Q1); handlers clear on next /extension reload."
        ))
    } else {
        CommandResult::message(format!("No installed extension '{id}' found on disk (state cleared)."))
    }
}
```

Update the `try_dispatch` match arms (line ~41): replace `"install" => install_stub(arg),` with `"install" => install(app, arg),` and `"uninstall" => uninstall_stub(arg),` with `"uninstall" => uninstall(app, arg),`.

Re-export `SourceKind`/`InstallScope`/`GitSource`/`LocalPathSource`/`CargoBuilder`/`ExtensionSource` from `codesmith_extensions` (if not already) — verify in `crates/extensions/src/lib.rs` the `pub use install_source::*;` or explicit re-exports exist. Add if missing:
```rust
pub use install_source::{CargoBuilder, ExtensionBuilder, ExtensionPlacer, ExtensionSource, GitSource, InstallScope, LocalPathSource, Placer, SourceArtifact, SourceKind, SourceSpec, UnimplementedSource};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-tui --bin codesmith-tui install_precheck`
Expected: PASS — 4 tests green.

Run the commands surface smoke + full suite:
Run: `cargo test -p codesmith-tui --bin codesmith-tui commands:: 2>&1 | tail -5`
Run: `cargo test -p codesmith-tui --bin codesmith-tui 2>&1 | tail -5`
Expected: 4 new install_precheck tests pass; the `every_registered_command_dispatches_to_a_handler` smoke still passes (dispatching `/extension install crate:foo` returns an error CommandResult, not a panic); tui 2833 (T2) + 4 (T7) = 2837 pass/26 pre-existing runtime_api fail/2 ignored.

- [ ] **Step 5: Docs — EXTENSIONS + ROADMAP**

In `docs/EXTENSIONS.md`:
- Update the `/extension install` / `/extension uninstall` rows in the In-TUI Manager table (line ~64-65) from "🚧 stub 'phase 2'" to "✅ working (§F5c)" with a one-line effect: "Fetches (git/path) → builds (cargo) → places to `<root>/<id>/` + writes `extension.toml` + records `installed[]` provenance; `--global` opt-in; project install warns if untrusted. `crate:`/`prebuilt:` return '§F5c-later'."
- Update the intro slice-status sentence (line ~28-34): mark §F5c done — "the INSTALL side (install-source impls [Git/LocalPath] + `CargoBuilder`/`Placer` + `/extension install`/`uninstall` real impl + `installed[]` provenance write) landed in §F5c; `CratesIo`/`Prebuilt` + true unload remain deferred."
- Update the Sandbox Stance (line ~282-286): note §F5c — install runs `cargo build` (build.rs = arbitrary code, trust-the-source per §8.1; containerize for untrusted); uninstall removes files+state but loaded tools/commands persist until process restart (bounded retention, §F5b Q1).

In `ROADMAP.md` (after the §F5b block, before `## §A`):
- Update the §F5b "下一聚焦工作" `:2604` line to mark §F5c done.
- Add a `### F5c` subsection mirroring `### F5b` structure: Status (done), 关键设计决策 (Q1-Q7 + D8, R1-R6 reconciliations), 落地步骤 (T1-T7), 测试/验证 (real test counts), By-design gaps (CratesIo/Prebuilt + true unload + tui e2e deferred), 下一聚焦工作 (§F3+ on demand).

- [ ] **Step 6: Commit**

```bash
git add crates/tui/src/commands/extension_commands.rs crates/extensions/src/lib.rs docs/EXTENSIONS.md ROADMAP.md
git commit -m "feat(framework): §F5c T7 /extension install+uninstall real impl + docs (install_precheck R4 crate/prebuilt guard; install: SourceSpec→Git/LocalSource+CargoBuilder+Installer→add_installed+trust-warn; uninstall: uninstall_files(both roots)+remove_installed+bounded-retention warn; EXTENSIONS install/uninstall rows ✅ + intro §F5c-done + Sandbox Stance cargo-build-trust; ROADMAP §F5c progress + ### F5c subsection; API reconciliation R1-R6 recorded; 4 Red→Green install_precheck tests; tui 2833→2837 pass/26 pre-existing runtime_api fail/2 ignored; ext 48 unchanged; all 4 suites: ext 48/agent 98/agent-runtime 1163+2/tui 2837 pass+26 pre-existing+2 ignored)"
```

---

## Self-Review (run after writing the plan)

**1. Spec coverage:**
- §3 In-scope 1 SourceSpec parser → T1 ✓
- §3 In-scope 2 GitSource/LocalPathSource → T3 ✓
- §3 In-scope 3 CargoBuilder → T4 ✓
- §3 In-scope 4 Placer → T5 ✓
- §3 In-scope 5 Installer orchestrator → T6 ✓
- §3 In-scope 6 installed→BTreeMap + mutators → T2 ✓
- §3 In-scope 7 extension.toml write → T6 (Installer) ✓
- §3 In-scope 8 /extension install/uninstall real → T7 ✓
- §3 In-scope 9 D8 temp-load metadata → T6 (install step 5) ✓
- §3 In-scope 10 e2e → T6 ✓
- §3 Out-of-scope (CratesIo/Prebuilt stub, clear_tools/unload, configured-paths, tui e2e) → respected (T7 crate/prebuilt guard; no clear_tools; discover_dylib stays 2-arg; e2e is extensions-crate) ✓
- §7 reconciliation (discover_dylib 2-arg, default_dylib_filename pub(crate), installed→BTreeMap, ManifestSource unchanged, discovery:181 comment fix) → T2/T5 ✓

**2. Placeholder scan:** No "TBD"/"TODO"/"implement later"/"add error handling" in steps. All code blocks complete. ✓ (T6 Step 1 `todo!()` is the TDD Red state, replaced in Step 3 — intentional, not a placeholder.)

**3. Type consistency:**
- `SourceSpec { kind, body, ref_, scope }` — used consistently in T1/T6/T7. ✓
- `InstallReport { id, version, path, provenance }` — defined T6, used T7. ✓ (no `will_load`/`scope` in report — R1: those are command-side.)
- `Placer::new(id, root)` + `Placer::dir()` — defined T5, used T6. ✓
- `Installer::new(source, builder, root)` + `install(spec)` + `uninstall_files(id, roots)` — defined T6, used T7. ✓
- `default_dylib_filename` pub(crate) — T5 changes visibility, T6 e2e + Placer use it. ✓

No type drift across tasks. ✓

---

## Verification gate (slice end — run, record real counts)

```bash
cargo build --workspace 2>&1 | tail -3
cargo test -p codesmith-extensions 2>&1 | tail -3          # expect 48 (26 §F5b + 22 §F5c)
cargo test -p codesmith-agent 2>&1 | tail -3               # expect 98 (unchanged)
cargo test -p codesmith-agent-runtime 2>&1 | tail -3       # expect 1163+2 (unchanged; flaky streamable_http test isolated re-run green)
cargo test -p codesmith-tui --bin codesmith-tui 2>&1 | tail -5  # expect 2837 pass/26 pre-existing runtime_api fail/2 ignored
```

**grep (§F5c new):**
```bash
grep -rn "GitSource\|LocalPathSource\|CargoBuilder\|Placer\|Installer\|SourceSpec" crates/extensions/src | wc -l   # ≥6
grep -n "add_installed\|remove_installed" crates/tui/src/extension_state.rs   # ≥2
test -f crates/extensions/src/installer.rs && echo "installer.rs exists"
grep -n "cargo build" crates/extensions/src/install_source.rs   # ≥1
grep -n "install_precheck\|fn install\|fn uninstall" crates/tui/src/commands/extension_commands.rs   # ≥3
```

**grep (§F5b unchanged):**
```bash
grep -n "libloading" crates/extensions/Cargo.toml   # ≥1
test -f crates/extensions/src/loader.rs && test -f crates/extensions/src/manifest.rs && test -f crates/extensions-fixture-dylib/build.rs && echo "§F5b files intact"
grep -c "discover_dylib" crates/tui/src/core/engine.rs   # ≥1
grep -c "codesmith_register_extension" crates/extensions-fixture-dylib/src/lib.rs   # ≥1
grep -c "\.emit(" crates/agent-runtime/src/engine/host_executor.rs   # =16 (unchanged — §F5c doesn't touch host_executor)
grep -rn "TrustReason::FirstLoad" crates/tui/src | wc -l   # =1 (unchanged)
```

**Honest-test red-line:** tui 26 `runtime_api::tests` PRE-EXISTING fail (environmental HTTP-server-bind, identical at §F5b base `7a6819a7`) — NOT a §F5c regression. Report "tui 2837 pass/26 pre-existing runtime_api fail/2 ignored", never "green". `agent-runtime` `streamable_http_stale_session_reconnects_and_retries_tool_call` flaky — isolated re-run green.
