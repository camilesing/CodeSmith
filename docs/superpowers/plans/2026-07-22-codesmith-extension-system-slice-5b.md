# §F5b — Dylib LOAD 侧 (phase 2 续作·上) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Each task = Red→impl→Green→commit (or green-on-write characterization where §F5 slice-1 precedent applies), matching §F2a/§F2b/§F2c/§F5 granularity/style.

**Goal:** Land the LOAD half of §F5: a `libloading`-based dylib loader + `extension.toml` manifest parser + phase-2 three-source discovery + a project-local trust gate (consuming the `FirstLoad`/`is_workspace_trusted` signal, Model A) + reload wiring, with a cdylib fixture crate proving the load path. Install/uninstall stays stub → §F5c.

**Architecture:** `codesmith-extensions` gains `manifest.rs` (serde `ExtensionManifest`), `loader.rs` (`load_dylib` free fn + `ExtensionRunner::libraries`/`load_dylib` method), and `discovery.rs` extensions (`discover_dylib` + `apply_trust_gate`, trust-agnostic — the host injects the trust bool). The tui `populate_extension_runtime` adds a dylib discover→trust-gate→reconcile→`load_dylib` block on the existing OS-thread runtime; `reload_extension_runtime` is unchanged (it calls `populate`, so reload auto-picks-up dylibs). Raw `libloading` + lockstep `*mut dyn Extension` (Approach 1, spec §2.4/§8.2) — no `abi_stable`, no new trait shape. The fixture is a `crate-type = ["cdylib","rlib"]` **dev-dep** so `cargo test -p codesmith-extensions --lib` builds its cdylib; `build.rs` computes the artifact path from `OUT_DIR` (no cargo subprocess, no target-dir lock).

**Tech Stack:** Rust 1.90.0; crates `codesmith-extensions` (manifest/loader/discovery/runner + build.rs + fixture dev-dep), `codesmith-tui` (engine.rs wiring + extension_commands list/info), `docs/EXTENSIONS.md` + `ROADMAP.md` (T7). `codesmith-agent`/`codesmith-agent-runtime` read-only w.r.t. the contract (no enum/trait change — `ExtensionError::Load` exists since §F1). New deps: `toml.workspace = true`, `libloading` (crates.io).

## Design decisions (load-bearing — finalized in the spec brainstorm; do not re-explore intent/requirements/design)
- **Range fork = split LOAD/INSTALL.** This slice = LOAD side; install/uninstall + install-source impls + `installed[]` provenance write → §F5c (stays stub).
- **ABI fork = raw `libloading` + lockstep `*mut dyn Extension` (Approach 1).** Dylib exports `#[no_mangle] pub extern "C" fn codesmith_register_extension() -> *mut dyn Extension`; loader `Box::from_raw` → `runner.load(&*ext)`. Same path as compiled-in extensions; no `abi_stable` (§2.4 no ABI churn). `ExtensionError::Load` already in contract (`crates/agent/src/extension.rs:65`).
- **Q1 — `ExtensionRunner.libraries: Mutex<Vec<Library>>`, reload does NOT clear.** Registered contributions are self-contained owned trait objects (vtable in the kept `Library`); `bind_core` append-inserts tools/commands (only `clear_handlers` exists — no `clear_tools`/`clear_commands`), so a removed dylib's tool Arc may still reference its vtable → keeping the Library alive is *correctness-preserving*, a bounded leak for re-discovered same dylibs. Manual `Debug` (`runner.rs:298-320`) gains a `libraries` count.
- **Q2 — Model A trust gate.** `apply_trust_gate(entries, project_trusted: bool)` drops `ProjectLocal` when `!project_trusted`. Host (`populate`) passes `crate::config::is_workspace_trusted(workspace)` (`&Path -> bool`, defined `crates/agent-runtime/src/workspace_trust.rs:116`, re-exported `crates/tui/src/config.rs:2640`). After onboarding accept → `mark_trusted` → `save_workspace_trust` (`config.rs:2643`) flips it true → `/extension reload` or restart picks up project-local dylib. Discovery stays trust-agnostic (pure fn) so the trust-gate logic is unit-testable in `codesmith-extensions`.
- **Q3 — fixture = workspace-member cdylib dev-dep.** `crates/extensions-fixture-dylib` (`crate-type=["cdylib","rlib"]`) exports `codesmith_register_extension` returning a `Box<dyn Extension>` that registers `fixture_echo` tool + `TurnStart` handler (records into `pub static FIXTURE_SEEN`). Built as a **dev-dep** of `codesmith-extensions` (rlib for `FIXTURE_SEEN` access; cdylib is the loaded artifact). `build.rs` computes the cdylib path from `OUT_DIR` (pop 3 → `<target>/<profile>`) + emits `cargo:rustc-env=CODESMITH_FIXTURE_DYLIB=<path>`. Lockstep by construction (same workspace + 1.90.0 toolchain).
- **Q4 — manifest `api_version` optional, warn not refuse.** Loader `tracing::warn!` on present+not-matching; lockstep enforced by build not runtime (§8.2).
- **TDD framing:** T1/T2/T3 use stub(`todo!()`/empty)→Red→impl→Green. T4 (integration with a built artifact) + T5 (wiring) + T6 (list/info) are green-on-write characterization/build-verified per the §F5 slice-1 precedent (tui e2e deferred — `run_tui`/`EngineHost` fixture scaffolding disproportionate; the dylib-load *mechanism* is covered by T4 runner-level + T3 unit, the *wiring* by build+no-regression).

## Baseline (must not regress at slice end — post-§F5 slice-1, commit `283aec12`)
`codesmith-extensions --lib` 15 · `codesmith-agent --lib` 98 · `codesmith-agent-runtime --lib` 1163+2 · `codesmith-tui --bin codesmith-tui` 2855+2 · `grep -c '\.emit(codesmith_agent::extension::ExtensionEvent' crates/agent-runtime/src/engine/host_executor.rs` = 16 · `grep -rn 'TrustReason::FirstLoad' crates/tui/src` = 1 (`tui/ui.rs:2892`).

**§F5b pre-state grep (this slice grows from 0):** `grep -c 'libloading' crates/extensions/Cargo.toml` = 0 · `grep -n 'toml' crates/extensions/Cargo.toml` (dep) = 0 (`serde` already present) · `loader.rs`/`manifest.rs` absent · `grep -rn 'discover_dylib' crates/` = 0 · `grep -rn 'codesmith_register_extension' crates/` = 0.

> **Pre-existing flaky test (NOT a regression — do not fix):** `mcp::tests::streamable_http_stale_session_reconnects_and_retries_tool_call` (`crates/agent-runtime/src/mcp.rs:5489`) fails intermittently under parallel load (mock-server race) but passes in isolation. §F5b does not touch `mcp.rs`. At slice end, if the `agent-runtime` run shows only this 1 failure, re-run in isolation to confirm green before treating the gate as met; expected green = 1163 passed + 2 ignored.

## File Structure (modified / added)

**`crates/extensions/`(modified)**
- `Cargo.toml` — T1: `toml.workspace = true`; T2: `libloading` (crates.io); T4: `[dev-dependencies] extensions-fixture-dylib = { path = "../extensions-fixture-dylib" }`.
- `src/lib.rs` — T1/T2/T3: `pub mod manifest;` `pub mod loader;` + re-exports (`ExtensionManifest`, `load_dylib`, `discover_dylib`, `DiscoveredDylib`, `DiscoveredSource`, `apply_trust_gate`).
- `src/manifest.rs`(NEW) — T1.
- `src/loader.rs`(NEW) — T2.
- `src/discovery.rs`(modified) — T3.
- `src/runner.rs`(modified) — T2: `libraries` field + `load_dylib` method + `Debug`.
- `build.rs`(NEW) — T4: emit `CODESMITH_FIXTURE_DYLIB` path.

**`crates/extensions-fixture-dylib/`(NEW workspace member; root `Cargo.toml` `[workspace].members` adds `"crates/extensions-fixture-dylib"`)**
- `Cargo.toml` — T4: `crate-type = ["cdylib","rlib"]`, deps `codesmith-agent` + `codesmith-tools` + `async-trait` + `serde_json`.
- `src/lib.rs` — T4: `codesmith_register_extension` + `FixtureExtension` (tool + handler).

**`crates/tui/src/core/engine.rs`(modified)** — T5: `populate_extension_runtime` (`:378-434`) gains a dylib discover→trust-gate→reconcile→`load_dylib` block.

**`crates/tui/src/commands/extension_commands.rs`(modified)** — T6: `list` (`:56`)/`info` (`:69`) enumerate `discover_dylib(...)` alongside `discover_static()`.

**docs** — T7: `docs/EXTENSIONS.md` (intro §F5b sentence `:32` + host-seam dylib row after `:251` + Sandbox Stance revision `:272-274`) + `ROADMAP.md` (§F5b progress block before `---` `:2575` + `### F5b` subsection after `:3018` + §F2c next-focus §F5 bullet `:2549`).

---

## Task 1: `manifest.rs` + `toml` dep + manifest parse tests

**Files:**
- Modify: `crates/extensions/Cargo.toml` (add `toml.workspace = true`)
- Create: `crates/extensions/src/manifest.rs`
- Modify: `crates/extensions/src/lib.rs` (add `pub mod manifest;` + `pub use manifest::ExtensionManifest;`)

- [ ] **Step 1: add the `toml` workspace dep.** In `crates/extensions/Cargo.toml` `[dependencies]`, after `serde_json.workspace = true` insert `toml.workspace = true`:
```toml
serde.workspace = true
serde_json.workspace = true
toml.workspace = true
thiserror.workspace = true
```
(`serde.workspace = true` already provides `derive` — the workspace declares `serde = { version = "1.0.228", features = ["derive"] }`; no serde edit needed.)

- [ ] **Step 2: write the failing tests + stub (Red).** Create `crates/extensions/src/manifest.rs` with the struct + a `todo!()` `from_str` + the test mod:
```rust
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
        todo!("§F5b T1: parse via toml::from_str")
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
```
Add to `crates/extensions/src/lib.rs` after `pub mod install_source;`:
```rust
pub mod manifest;
```
and after `pub use install_source::{...};`:
```rust
pub use manifest::ExtensionManifest;
```

- [ ] **Step 3: run the tests — expect FAIL (Red, `todo!()` panic).** Run: `cargo +1.90.0 test -p codesmith-extensions --lib manifest`. Expected: 3 tests run, all FAIL (`thread panicked at 'not implemented'`).

- [ ] **Step 4: implement `from_str` (Green).** Replace the `todo!()` body in `manifest.rs`:
```rust
    pub fn from_str(text: &str) -> Result<Self, ExtensionError> {
        toml::from_str(text).map_err(|e| ExtensionError::Load(format!("manifest parse: {e}")))
    }
```

- [ ] **Step 5: run the tests — expect PASS.** Run: `cargo +1.90.0 test -p codesmith-extensions --lib manifest`. Expected: `test result: ok. 3 passed; 0 failed`.

- [ ] **Step 6: build the workspace.** Run: `cargo +1.90.0 build -p codesmith-extensions`. Expected: green (manifest module compiles; `toml` dep resolves).

- [ ] **Step 7: commit.**
```bash
git add crates/extensions/Cargo.toml crates/extensions/src/manifest.rs crates/extensions/src/lib.rs
git commit -m "feat(framework): §F5b T1 extension.toml manifest (ExtensionManifest serde Deserialize: id/version/entry?/source?[type,ref?]/api_version?; from_str/parse → ExtensionError::Load on malformed/missing; +toml.workspace dep (serde derive already via workspace); +3 tests full/minimal/malformed; ext 15→18)"
```

---

## Task 2: `loader.rs` + `libloading` dep + `ExtensionRunner.libraries`/`load_dylib` + error tests

**Files:**
- Modify: `crates/extensions/Cargo.toml` (add `libloading`)
- Create: `crates/extensions/src/loader.rs`
- Modify: `crates/extensions/src/runner.rs` (imports + `libraries` field + `load_dylib` method + `Debug`)
- Modify: `crates/extensions/src/lib.rs` (`pub mod loader;` + `pub use loader::load_dylib;`)

- [ ] **Step 1: add the `libloading` dep.** Run: `cargo +1.90.0 add libloading -p codesmith-extensions`. Expected: a line like `libloading = "0.x"` appended under `[dependencies]` (cargo pins the latest stable; matches the repo's explicit-version style for non-workspace crates).

- [ ] **Step 2: create `loader.rs` with a `todo!()` `load_dylib` + error tests (Red).** Create `crates/extensions/src/loader.rs`:
```rust
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
    todo!("§F5b T2: Library::new + symbol + Box::from_raw")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_dylib_missing_file_is_load_error() {
        let path = std::path::PathBuf::from("/nonexistent/ext-does-not-exist.dylib");
        let r = load_dylib(&path);
        assert!(matches!(r, Err(ExtensionError::Load(_))), "got {r:?}");
    }

    #[test]
    fn load_dylib_not_a_dylib_is_load_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not-a-dylib");
        std::fs::write(&path, b"not a dylib").expect("write");
        let r = load_dylib(&path);
        assert!(matches!(r, Err(ExtensionError::Load(_))), "got {r:?}");
    }
}
```
Add to `crates/extensions/src/lib.rs` after `pub mod manifest;`:
```rust
pub mod loader;
```
and after `pub use manifest::ExtensionManifest;`:
```rust
pub use loader::load_dylib;
```

- [ ] **Step 3: run the tests — expect FAIL (Red, `todo!()` panic).** Run: `cargo +1.90.0 test -p codesmith-extensions --lib loader`. Expected: 2 tests run, both FAIL (`not implemented`).

- [ ] **Step 4: implement `load_dylib` (Green).** Replace the `todo!()` body in `loader.rs`:
```rust
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
```

- [ ] **Step 5: run the loader tests — expect PASS.** Run: `cargo +1.90.0 test -p codesmith-extensions --lib loader`. Expected: `test result: ok. 2 passed`.

- [ ] **Step 6: wire `libraries` + `load_dylib` onto `ExtensionRunner`.** In `crates/extensions/src/runner.rs`:
  - Add imports after `use crate::api::StubExtensionApi;` (line 23):
```rust
use std::path::Path;

use libloading::Library;
```
  - Add the field to the `ExtensionRunner` struct (after `handlers`, before the closing `}`, `runner.rs:102`):
```rust
    /// §F5b — loaded dylib `Library` handles. Pushed by `load_dylib`;
    /// reload does NOT clear — the Library's code/vtables must outlive any
    /// registered contributions still in `tools`/`commands`/`handlers`
    /// (`clear_handlers` drops handler Arcs but `tools`/`commands` are
    /// append-insert, so a removed dylib's tool Arc may still reference
    /// its vtable; keeping the Library alive is correctness-preserving,
    /// a bounded leak for re-discovered same dylibs). §F5b Q1.
    libraries: Mutex<Vec<Library>>,
```
  - Initialize in `ExtensionRunner::new` (`runner.rs:115`, after `handlers: Mutex::new(Vec::new()),`):
```rust
            libraries: Mutex::new(Vec::new()),
```
  - Add the `load_dylib` method after `load` (after `runner.rs:158`, before `bind_core` at `:167`):
```rust
    /// §F5b — load a dylib extension (spec §F5b / §7.2). Opens the library,
    /// calls its `codesmith_register_extension`, pushes the `Library` into
    /// `libraries` (must outlive registered contributions' vtables; reload
    /// does not clear — Q1), then runs `configure` via [`load`](Self::load).
    /// The `Extension` Box is dropped after `configure` (registered
    /// contributions are self-contained owned trait objects; vtables live in
    /// the kept `Library`). Lockstep (§8.2) assumed. Mirrors the static
    /// `load` path.
    pub async fn load_dylib(&self, path: &Path) -> Result<(), ExtensionError> {
        let (library, extension) = crate::loader::load_dylib(path)?;
        self.libraries
            .lock()
            .expect("libraries lock poisoned")
            .push(library);
        self.load(&*extension).await
    }
```
  - Add the `libraries` count to the manual `Debug` impl. In `impl std::fmt::Debug for ExtensionRunner` (`runner.rs:298-320`), after the `handlers` count block (`:306-310`) add:
```rust
        let libraries = self
            .libraries
            .lock()
            .expect("libraries mutex poisoned")
            .len();
```
  and in the `f.debug_struct("ExtensionRunner")` chain, after `.field("handlers", &handlers)` (`:317`) add:
```rust
            .field("libraries", &libraries)
```

- [ ] **Step 7: build the workspace.** Run: `cargo +1.90.0 build -p codesmith-extensions`. Expected: green (no test yet exercises `load_dylib` the method — that is T4; the field/method are infra verified by compile + T4).

- [ ] **Step 8: commit.**
```bash
git add crates/extensions/Cargo.toml crates/extensions/src/loader.rs crates/extensions/src/runner.rs crates/extensions/src/lib.rs Cargo.lock
git commit -m "feat(framework): §F5b T2 dylib loader + runner.libraries/load_dylib (loader.rs load_dylib path→Library::new→Symbol codesmith_register_extension→Box::from_raw; ExtensionRunner.libraries: Mutex<Vec<Library>> push on load_dylib, reload does not clear—correctness for append-insert tools/no clear_tools + bounded leak for re-discovered; Manual Debug +libraries count; +libloading dep; +2 error tests missing-file/not-a-dylib; runner.load_dylib method exercised by T4 fixture; ext 18→20)"
```

---

## Task 3: `discover_dylib` + `apply_trust_gate` + discovery/trust-gate tests

**Files:**
- Modify: `crates/extensions/src/discovery.rs` (add `DiscoveredSource`/`DiscoveredDylib`/`discover_dylib`/`apply_trust_gate` + tests)
- Modify: `crates/extensions/src/lib.rs` (re-exports)

- [ ] **Step 1: write the failing tests + stubs (Red).** Append to `crates/extensions/src/discovery.rs` (after the existing `discover_static` fn, before the existing `#[cfg(test)] mod tests`):
```rust
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::manifest::ExtensionManifest;

/// Where a discovered dylib was found (spec §7.2 three sources). The host
/// drops `ProjectLocal` entries for untrusted workspaces via
/// [`apply_trust_gate`] (§F5b Q2 Model A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveredSource {
    Global,
    ProjectLocal,
    ConfiguredPath,
}

/// A dylib discovered on disk + its manifest + origin.
#[derive(Debug, Clone)]
pub struct DiscoveredDylib {
    pub manifest: ExtensionManifest,
    pub dylib_path: PathBuf,
    pub source: DiscoveredSource,
}

/// The platform's dylib extension to match for bare-file discovery.
fn dylib_extensions() -> &'static [&'static str] {
    &[std::env::consts::DLL_EXTENSION]
}

/// Default dylib filename for an id: `<DLL_PREFIX><id>.<DLL_EXTENSION>`
/// (e.g. `libmy_ext.dylib` on macOS, `my_ext.dll` on Windows).
pub fn default_dylib_filename(id: &str) -> String {
    format!(
        "{}{}.{}",
        std::env::consts::DLL_PREFIX,
        id,
        std::env::consts::DLL_EXTENSION
    )
}

/// Inspect one directory entry; return a `DiscoveredDylib` if it is a bare
/// dylib file or a subdirectory with an `extension.toml`.
fn discover_one(path: &Path, source: DiscoveredSource) -> Option<DiscoveredDylib> {
    let manifest_path = path.join("extension.toml");
    if manifest_path.is_file() {
        let manifest = ExtensionManifest::parse(&manifest_path).ok()?;
        let entry = manifest
            .entry
            .clone()
            .unwrap_or_else(|| default_dylib_filename(&manifest.id));
        let dylib_path = path.join(entry);
        return Some(DiscoveredDylib { manifest, dylib_path, source });
    }
    if path.is_file() {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if dylib_extensions().iter().any(|e| *e == ext) {
                let stem = path.file_stem()?.to_string_lossy().into_owned();
                let manifest = ExtensionManifest {
                    id: stem,
                    version: "0.0.0".to_string(),
                    entry: None,
                    source: None,
                    api_version: None,
                };
                return Some(DiscoveredDylib {
                    manifest,
                    dylib_path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
    None
}

/// Phase-2 dylib discovery (spec §7.2). Scans three sources — `global`
/// (e.g. `~/.codesmith/extensions/`), `project`
/// (`<workspace>/.codesmith/extensions/`), and explicit `configured` paths
/// — one level deep. A directory entry is either a bare `*.<dylib-ext>`
/// file or a subdirectory containing an `extension.toml`. Dedups by
/// canonical dylib path (first-wins). `None`/empty sources are skipped.
/// Discovery is trust-agnostic; the host filters via [`apply_trust_gate`].
pub fn discover_dylib(
    global: Option<&Path>,
    project: &Path,
    configured: &[PathBuf],
) -> Vec<DiscoveredDylib> {
    Vec::new() // §F5b T3 stub — replaced in Step 3
}

/// §F5b Q2 Model A — drop `ProjectLocal` entries when the workspace is not
/// trusted. Pure (no trust-state dependency); the host passes the bool from
/// `is_workspace_trusted(workspace)`. Global + ConfiguredPath always pass.
pub fn apply_trust_gate(
    entries: Vec<DiscoveredDylib>,
    project_trusted: bool,
) -> Vec<DiscoveredDylib> {
    let _ = (entries, project_trusted);
    Vec::new() // §F5b T3 stub — replaced in Step 3
}

#[cfg(test)]
mod dylib_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Create `<parent>/<id>/extension.toml` + a fake dylib named by `entry`
    /// (or the default `<DLL_PREFIX><id>.<DLL_EXTENSION>` when `entry` is
    /// `None`). Returns the subdir path.
    fn write_manifest_subdir(parent: &Path, id: &str, entry: Option<&str>) -> PathBuf {
        let dir = parent.join(id);
        fs::create_dir_all(&dir).unwrap();
        let entry_line = entry
            .map(|e| format!("entry = \"{e}\"\n"))
            .unwrap_or_default();
        fs::write(
            dir.join("extension.toml"),
            format!("id = \"{id}\"\nversion = \"1.0.0\"\n{entry_line}"),
        )
        .unwrap();
        let dylib_name = entry
            .map(String::from)
            .unwrap_or_else(|| default_dylib_filename(id));
        fs::write(dir.join(&dylib_name), b"fake dylib").unwrap();
        dir
    }

    #[test]
    fn discover_dylib_finds_manifest_subdir_with_default_entry() {
        let dir = tempdir().unwrap();
        write_manifest_subdir(dir.path(), "my-ext", None);
        let found = discover_dylib(None, dir.path(), &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.id, "my-ext");
        assert_eq!(found[0].source, DiscoveredSource::ProjectLocal);
        assert!(found[0]
            .dylib_path
            .ends_with(default_dylib_filename("my-ext")));
    }

    #[test]
    fn discover_dylib_finds_bare_dylib_file() {
        let dir = tempdir().unwrap();
        let name = format!("libbare.{}", std::env::consts::DLL_EXTENSION);
        fs::write(dir.path().join(&name), b"fake").unwrap();
        let found = discover_dylib(None, dir.path(), &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.id, "libbare");
        assert_eq!(found[0].manifest.version, "0.0.0");
    }

    #[test]
    fn discover_dylib_dedups_shared_dylib_path() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a")).unwrap();
        fs::create_dir_all(dir.path().join("b")).unwrap();
        fs::write(dir.path().join("shared"), b"fake").unwrap();
        fs::write(
            dir.path().join("a").join("extension.toml"),
            "id = \"a\"\nversion = \"1.0.0\"\nentry = \"../shared\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("b").join("extension.toml"),
            "id = \"b\"\nversion = \"1.0.0\"\nentry = \"../shared\"\n",
        )
        .unwrap();
        let found = discover_dylib(None, dir.path(), &[]);
        assert_eq!(found.len(), 1, "deduped to one: {found:?}");
    }

    #[test]
    fn discover_dylib_tags_global_project_configured() {
        let global = tempdir().unwrap();
        let project = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        write_manifest_subdir(global.path(), "g", None);
        write_manifest_subdir(project.path(), "p", None);
        write_manifest_subdir(cfg.path(), "c", None);
        let found = discover_dylib(
            Some(global.path()),
            project.path(),
            &[cfg.path().to_path_buf()],
        );
        assert_eq!(found.len(), 3);
        let sources: Vec<_> = found.iter().map(|f| f.source).collect();
        assert!(sources.contains(&DiscoveredSource::Global));
        assert!(sources.contains(&DiscoveredSource::ProjectLocal));
        assert!(sources.contains(&DiscoveredSource::ConfiguredPath));
    }

    #[test]
    fn apply_trust_gate_drops_project_local_when_untrusted() {
        let mk = |source| DiscoveredDylib {
            manifest: ExtensionManifest {
                id: "x".into(),
                version: "0.0.0".into(),
                entry: None,
                source: None,
                api_version: None,
            },
            dylib_path: PathBuf::from("/x"),
            source,
        };
        let entries = vec![
            mk(DiscoveredSource::Global),
            mk(DiscoveredSource::ProjectLocal),
            mk(DiscoveredSource::ConfiguredPath),
        ];
        assert_eq!(apply_trust_gate(entries.clone(), true).len(), 3);
        let untrusted = apply_trust_gate(entries, false);
        assert_eq!(untrusted.len(), 2);
        assert!(untrusted
            .iter()
            .all(|e| e.source != DiscoveredSource::ProjectLocal));
    }
}
```
Add to `crates/extensions/src/lib.rs` re-exports (after `pub use manifest::ExtensionManifest;`):
```rust
pub use discovery::{
    apply_trust_gate, default_dylib_filename, discover_dylib, DiscoveredDylib,
    DiscoveredSource,
};
```

- [ ] **Step 2: run the tests — expect FAIL (Red, stubs return empty).** Run: `cargo +1.90.0 test -p codesmith-extensions --lib dylib_tests`. Expected: 5 tests run, the 4 discovery tests FAIL (assert `len()==1`/`3` vs `0`); the `apply_trust_gate` test FAILs (`len()==3` vs `0`).

- [ ] **Step 3: implement `discover_dylib` + `apply_trust_gate` (Green).** Replace the two stub bodies in `discovery.rs`:
```rust
pub fn discover_dylib(
    global: Option<&Path>,
    project: &Path,
    configured: &[PathBuf],
) -> Vec<DiscoveredDylib> {
    let mut out: Vec<DiscoveredDylib> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let sources = global
        .iter()
        .map(|d| (*d, DiscoveredSource::Global))
        .chain(std::iter::once((project, DiscoveredSource::ProjectLocal)))
        .chain(
            configured
                .iter()
                .map(|d| (d.as_path(), DiscoveredSource::ConfiguredPath)),
        );
    for (dir, source) in sources {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(found) = discover_one(&path, source) {
                let key = found
                    .dylib_path
                    .canonicalize()
                    .unwrap_or_else(|_| found.dylib_path.clone());
                if seen.insert(key) {
                    out.push(found);
                }
            }
        }
    }
    out
}

pub fn apply_trust_gate(
    entries: Vec<DiscoveredDylib>,
    project_trusted: bool,
) -> Vec<DiscoveredDylib> {
    entries
        .into_iter()
        .filter(|e| project_trusted || e.source != DiscoveredSource::ProjectLocal)
        .collect()
}
```

- [ ] **Step 4: run the tests — expect PASS.** Run: `cargo +1.90.0 test -p codesmith-extensions --lib dylib_tests`. Expected: `test result: ok. 5 passed`.

- [ ] **Step 5: build the workspace.** Run: `cargo +1.90.0 build -p codesmith-extensions`. Expected: green.

- [ ] **Step 6: commit.**
```bash
git add crates/extensions/src/discovery.rs crates/extensions/src/lib.rs
git commit -m "feat(framework): §F5b T3 discover_dylib + apply_trust_gate (discover_dylib(global?,project,configured[]) one-level-deep scan: subdir+extension.toml→parsed manifest/default entry, or bare *.<DLL_EXTENSION>→stem id; dedup by canonical dylib path first-wins; DiscoveredSource{Global,ProjectLocal,ConfiguredPath} tag; apply_trust_gate pure fn drops ProjectLocal when !project_trusted—host injects is_workspace_trusted bool, discovery trust-agnostic; +5 tests manifest-subdir/bare/dedup/tags/trust-gate; ext 20→25)"
```

---

## Task 4: cdylib fixture crate + `build.rs` + load-contributions test

**Files:**
- Create: `crates/extensions-fixture-dylib/Cargo.toml`
- Create: `crates/extensions-fixture-dylib/src/lib.rs`
- Modify: root `Cargo.toml` (`[workspace].members` adds the fixture)
- Modify: `crates/extensions/Cargo.toml` (`[dev-dependencies]` adds the fixture)
- Create: `crates/extensions/build.rs`
- Modify: `crates/extensions/src/loader.rs` (add the fixture load test to the test mod)

- [ ] **Step 1: create the fixture crate.** Create `crates/extensions-fixture-dylib/Cargo.toml`:
```toml
[package]
name = "extensions-fixture-dylib"
version.workspace = true
edition.workspace = true
license.workspace = true
publish = false

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
async-trait = "0.1"
codesmith-agent = { path = "../agent", version = "0.8.48" }
codesmith-tools = { path = "../tools", version = "0.8.48" }
serde_json.workspace = true
```
Create `crates/extensions-fixture-dylib/src/lib.rs`:
```rust
//! §F5b test fixture: a cdylib exporting `codesmith_register_extension`
//! returning a `Box<dyn Extension>` that registers a tool + a `TurnStart`
//! handler. Loaded by `codesmith-extensions` tests to prove the dylib
//! loader path (lockstep: same workspace + toolchain → vtable match).
//!
//! `crate-type = ["cdylib","rlib"]` — the cdylib is the on-disk artifact
//! the loader reads; the rlib lets `codesmith-extensions` dev-depend on it
//! to read [`FIXTURE_SEEN`] (and so `cargo test --lib` builds the cdylib).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use codesmith_agent::extension::*;
use codesmith_tools::{ToolCapability, ToolResult};
use serde_json::Value;

/// Observable handle: incremented by the fixture's `TurnStart` handler so
/// the loader test can assert the dylib's handler dispatches through the
/// runner. `pub` so the test (a separate compilation unit linking the
/// rlib) can read it.
pub static FIXTURE_SEEN: AtomicUsize = AtomicUsize::new(0);

pub struct FixtureExtension;

#[async_trait]
impl Extension for FixtureExtension {
    fn metadata(&self) -> &ExtensionMetadata {
        static M: ExtensionMetadata = ExtensionMetadata::new("fixture-dylib");
        &M
    }
    async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
        api.register_tool(Box::new(FixtureEchoTool))?;
        api.on(Arc::new(FixtureTurnStartHandler))?;
        Ok(())
    }
}

pub struct FixtureEchoTool;

#[async_trait]
impl ToolDefinition for FixtureEchoTool {
    fn name(&self) -> &str {
        "fixture_echo"
    }
    fn description(&self) -> &str {
        "Fixture echo tool."
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }
    async fn execute(
        &self,
        input: Value,
        _ctx: &dyn ExtensionContext,
    ) -> Result<ToolResult, ExtensionError> {
        let text = input
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(ToolResult::success(format!("fixture:{text}")))
    }
}

pub struct FixtureTurnStartHandler;

#[async_trait]
impl Handler for FixtureTurnStartHandler {
    async fn handle(
        &self,
        event: &ExtensionEvent,
        _ctx: &dyn ExtensionContext,
    ) -> Result<HandlerOutcome, ExtensionError> {
        if matches!(event, ExtensionEvent::TurnStart { .. }) {
            FIXTURE_SEEN.fetch_add(1, Ordering::Relaxed);
        }
        Ok(HandlerOutcome::Continue)
    }
}

/// C-ABI entry the host loader looks up. Returns a `Box<dyn Extension>` the
/// host reclaims via `Box::from_raw` (lockstep: same allocator). The
/// `*mut FixtureExtension` → `*mut dyn Extension` unsizing coercion happens
/// at the return coercion site.
#[no_mangle]
pub extern "C" fn codesmith_register_extension() -> *mut dyn Extension {
    Box::into_raw(Box::new(FixtureExtension))
}
```

- [ ] **Step 2: register the fixture as a workspace member + a dev-dep.** In root `Cargo.toml` `[workspace].members`, add `"crates/extensions-fixture-dylib"` after `"crates/extensions"`:
```toml
    "crates/extensions",
    "crates/extensions-fixture-dylib",
    "crates/hooks",
```
In `crates/extensions/Cargo.toml` `[dev-dependencies]`, add:
```toml
[dev-dependencies]
tempfile = "3.16"
extensions-fixture-dylib = { path = "../extensions-fixture-dylib" }
```

- [ ] **Step 3: create `build.rs` to emit the fixture's cdylib path.** Create `crates/extensions/build.rs`:
```rust
//! §F5b — emit the on-disk path of the fixture cdylib so tests can load it.
//!
//! The fixture (`extensions-fixture-dylib`, `crate-type = ["cdylib","rlib"]`)
//! is a **dev-dependency** of this crate, so `cargo test -p
//! codesmith-extensions --lib` builds its cdylib into `<target>/<profile>/`.
//! `OUT_DIR` is `<target>/<profile>/build/<hash>/out`; popping three
//! components yields `<target>/<profile>`, where the cdylib lands. This
//! avoids shelling out to `cargo` from build.rs (no target-dir lock
//! deadlock) — the dev-dep mechanism builds the artifact; build.rs only
//! computes the path (the file exists by test-run time).

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set for build script");
    let mut target_profile = std::path::PathBuf::from(out_dir);
    for _ in 0..3 {
        target_profile.pop();
    }
    let libname = format!(
        "{}extensions_fixture_dylib.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    );
    let artifact = target_profile.join(libname);
    println!(
        "cargo:rustc-env=CODESMITH_FIXTURE_DYLIB={}",
        artifact.display()
    );
    println!("cargo:rerun-if-changed=../extensions-fixture-dylib/src/lib.rs");
}
```

- [ ] **Step 4: add the load-contributions test to `loader.rs` (green-on-write characterization).** In `crates/extensions/src/loader.rs` `#[cfg(test)] mod tests`, extend the imports + add a `Ctx` + the test:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use codesmith_agent::extension::*;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    // --- error-path tests (T2) remain above ---

    struct Ctx {
        generation: u64,
    }
    #[async_trait]
    impl ExtensionContext for Ctx {
        fn cwd(&self) -> &std::path::Path {
            std::path::Path::new(".")
        }
        fn mode(&self) -> ExtensionMode {
            ExtensionMode::Tui
        }
        fn is_idle(&self) -> bool {
            true
        }
        fn signal(&self) -> tokio_util::sync::CancellationToken {
            tokio_util::sync::CancellationToken::new()
        }
        fn generation(&self) -> u64 {
            self.generation
        }
    }
    impl ExtensionCommandContext for Ctx {}

    /// §F5b — the fixture cdylib is built as a dev-dep; `build.rs` emits its
    /// path. Proves the full dylib load path: `load_dylib` → `configure`
    /// (registers `fixture_echo` tool + `TurnStart` handler) → `bind_core` →
    /// the tool is bound + the handler dispatches on `emit`. Lockstep holds
    /// (same workspace + 1.90.0 toolchain).
    #[test]
    fn load_dylib_fixture_contributes_tool_and_handler() {
        let path = env!("CODESMITH_FIXTURE_DYLIB");
        extensions_fixture_dylib::FIXTURE_SEEN.store(0, Ordering::SeqCst);
        let runner = crate::ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load_dylib(std::path::Path::new(path)))
            .expect("load fixture");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let tools: Vec<String> = runner
            .bound_tools()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(
            tools.iter().any(|n| n == "fixture_echo"),
            "fixture tool bound: {tools:?}"
        );
        rt.block_on(runner.emit(ExtensionEvent::TurnStart {
            turn_id: "t1".into(),
        }));
        assert!(
            extensions_fixture_dylib::FIXTURE_SEEN.load(Ordering::SeqCst) > 0,
            "fixture handler dispatched"
        );
    }
}
```
> Note: merge this into the existing `mod tests` from T2 (keep the two T2 error tests; add the `Ctx` + this test). `crate::ExtensionRunner` resolves via the lib re-export (`lib.rs:48`); `ExtensionEvent`/`ExtensionContext`/`ExtensionCommandContext`/`ExtensionMode` resolve via the crate-root `pub use codesmith_agent::extension::*`.

- [ ] **Step 5: build the fixture + run the test — expect PASS.** Run: `cargo +1.90.0 test -p codesmith-extensions --lib loader`. Expected: `test result: ok. 3 passed` (2 T2 error tests + 1 fixture load). If the fixture dylib fails to build, inspect `cargo +1.90.0 build -p extensions-fixture-dylib` first; confirm `CODESMITH_FIXTURE_DYLIB` points at an existing file (`cargo +1.90.0 build -p codesmith-extensions` then `ls` the path printed by `cargo build -vv`).

- [ ] **Step 6: build the whole workspace (fixture is now a member).** Run: `cargo +1.90.0 build --workspace`. Expected: green.

- [ ] **Step 7: commit.**
```bash
git add crates/extensions-fixture-dylib crates/extensions/build.rs crates/extensions/Cargo.toml crates/extensions/src/loader.rs Cargo.toml Cargo.lock
git commit -m "feat(framework): §F5b T4 cdylib fixture + build.rs path emit + load-contributions test (extensions-fixture-dylib: crate-type=[cdylib,rlib], codesmith_register_extension→Box<dyn Extension> registering fixture_echo tool+TurnStart handler→FIXTURE_SEEN; dev-dep of codesmith-extensions so --lib builds the cdylib; build.rs computes artifact path from OUT_DIR pop3→<target>/<profile>, emits CODESMITH_FIXTURE_DYLIB env—no cargo subprocess/lock; +load_dylib_fixture_contributes_tool_and_handler test asserts tool bound + handler dispatches; resolves spec §7 T4 build-ordering via dev-dep+path-compute; ext 25→26)"
```

---

## Task 5: `engine.rs` wiring — `populate_extension_runtime` dylib step

**Files:**
- Modify: `crates/tui/src/core/engine.rs` (`populate_extension_runtime` `:378-434`)

- [ ] **Step 1: surgically add the dylib block to `populate_extension_runtime` (`crates/tui/src/core/engine.rs:378-434`).** Read the current body first to confirm the exact anchor text, then apply three surgical edits. The edit is deliberately surgical — steps 1 (`discover_static`), 2 (the `enabled` reconcile), and 4 (`HostExtensionContext::new` + `bind_core`) stay UNCHANGED; this slice only INSERTS the dylib block and widens the load guard. Not reproducing step 4 avoids any risk of diverging from the current `HostExtensionContext::new(...)` argument shape (e.g. `workspace` vs `workspace.to_path_buf()`, or the `idle` construction).

  **Edit A — insert the dylib discover/gate/reconcile block** after the `let enabled: Vec<_> = discovered.into_iter().filter(...).collect();` line (step 2) and BEFORE the `if !enabled.is_empty()` guard:
```rust
    // §F5b — discover dylibs (global + project; configured paths → §F5c
    // when settings.extensions lands). Global dir = ~/.codesmith/extensions
    // (effective_home_dir re-exported via crate::config); project dir =
    // <workspace>/.codesmith/extensions. apply_trust_gate drops ProjectLocal
    // when the workspace is not trusted (Model A — consume FirstLoad's
    // persisted-trust flip via is_workspace_trusted). Discovery is
    // trust-agnostic; the gate is the host's concern.
    let global_dir = crate::config::effective_home_dir()
        .map(|home| home.join(".codesmith").join("extensions"));
    let project_dir = workspace.join(".codesmith").join("extensions");
    let project_trusted = crate::config::is_workspace_trusted(workspace);
    let discovered_dylib = codesmith_extensions::discover_dylib(
        global_dir.as_deref(),
        &project_dir,
        &[],
    );
    let enabled_dylib: Vec<_> = codesmith_extensions::apply_trust_gate(
        discovered_dylib,
        project_trusted,
    )
    .into_iter()
    .filter(|d| state.is_enabled(&d.manifest.id))
    .collect();
```

  **Edit B — widen the load guard** from `if !enabled.is_empty() {` to:
```rust
    if !enabled.is_empty() || !enabled_dylib.is_empty() {
```

  **Edit C — add the dylib load loop** inside the `std::thread::scope` `s.spawn(move || { ... })` closure, AFTER the existing `for reg in enabled { let ext = (reg.factory)(); let _ = load_rt.block_on(runner_for_thread.load(&*ext)); }` loop and BEFORE the closure's closing `});`:
```rust
                for d in enabled_dylib {
                    if let Err(e) =
                        load_rt.block_on(runner_for_thread.load_dylib(&d.dylib_path))
                    {
                        tracing::warn!(
                            target: "codesmith_extensions::loader",
                            "skip dylib {}: {e}",
                            d.dylib_path.display()
                        );
                    }
                }
```

  `enabled_dylib` is captured by the `move` closure alongside `enabled` (declare it before the closure, as in Edit A). Steps 1, 2, and 4 are not touched.
> `reload_extension_runtime` (`:447-456`) is unchanged — it calls `populate_extension_runtime`, so `/extension reload` auto-picks-up dylibs (Q1: `libraries` is not cleared, so reload pushes new Libraries; correctness-preserving per the registry-clear asymmetry).

- [ ] **Step 2: build + run the four suites (wiring verification; tui dylib e2e deferred per §F5 precedent).** The dylib-load *mechanism* is covered by T4 (runner-level fixture) + the discovery/trust-gate logic by T3; the wiring is glue verified by build + no-regression (a tui e2e needs an `EngineHost`+`run_tui`+real-trust fixture — disproportionate per §F5 slice-1).
  - `cargo +1.90.0 build --workspace` — green.
  - `cargo +1.90.0 test -p codesmith-extensions --lib` — 26 (T1-T4).
  - `cargo +1.90.0 test -p codesmith-agent --lib` — 98 (unchanged).
  - `cargo +1.90.0 test -p codesmith-agent-runtime --lib` — 1163+2 (unchanged; LOAD doesn't touch `host_executor`). If only `streamable_http_stale_session...` fails, re-run in isolation.
  - `cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui` — 2855+2 (no regression; the dylib step is a no-op when no dylibs are on disk).
  - `grep -rn 'discover_dylib' crates/tui/src/core/engine.rs` — ≥ 1.

- [ ] **Step 3: commit.**
```bash
git add crates/tui/src/core/engine.rs
git commit -m "feat(framework): §F5b T5 populate_extension_runtime dylib wiring (after static discover/reconcile/load: discover_dylib(global=~/.codesmith/extensions via effective_home_dir, project=<workspace>/.codesmith/extensions, configured=[])→apply_trust_gate(is_workspace_trusted workspace, Model A)→reconcile state.is_enabled→load_dylib on the same OS-thread load runtime, warn+continue on error per §8.3; reload_extension_runtime unchanged—calls populate so /extension reload auto-picks-up dylibs; tui dylib e2e deferred per §F5 precedent—mechanism covered by T4 fixture+T3 discovery; ext 26, agent 98, agent-runtime 1163+2, tui 2855+2; discover_dylib call in engine.rs=1)"
```

---

## Task 6: `/extension list` + `/extension info` surface dylib-discovered ext

**Files:**
- Modify: `crates/tui/src/commands/extension_commands.rs` (`list` `:56`, `info` `:69`)

- [ ] **Step 1: extend `list` + `info` to enumerate `discover_dylib`.** Replace `list` (`:56-67`) and `info` (`:69-82`) in `crates/tui/src/commands/extension_commands.rs`:
```rust
fn list(app: &App) -> CommandResult {
    let mut out = String::new();
    let mut ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    // Compiled-in (phase 1).
    let discovered = codesmith_extensions::discover_static();
    let mut compiled = String::new();
    for reg in &discovered {
        ids.insert(reg.metadata.id.to_string());
        compiled.push_str(&format!("  {} (v{}) [compiled]\n", reg.metadata.id, reg.metadata.version));
    }
    if !discovered.is_empty() {
        out.push_str("Compiled-in extensions:\n");
        out.push_str(&compiled);
    }

    // §F5b — dylib-discovered (global + project; configured → §F5c).
    let global_dir = crate::config::effective_home_dir()
        .map(|home| home.join(".codesmith").join("extensions"));
    let project_dir = app.workspace.join(".codesmith").join("extensions");
    let dylibs = codesmith_extensions::discover_dylib(global_dir.as_deref(), &project_dir, &[]);
    if !dylibs.is_empty() {
        out.push_str("Dylib extensions:\n");
        for d in &dylibs {
            // Skip ids already shown as compiled-in (dedup by id).
            if ids.insert(d.manifest.id.clone()) {
                out.push_str(&format!(
                    "  {} (v{}) [dylib, {}]\n",
                    d.manifest.id, d.manifest.version,
                    match d.source {
                        codesmith_extensions::DiscoveredSource::Global => "global",
                        codesmith_extensions::DiscoveredSource::ProjectLocal => "project",
                        codesmith_extensions::DiscoveredSource::ConfiguredPath => "configured",
                    }
                ));
            }
        }
    }

    if out.is_empty() {
        return CommandResult::message("No extensions discovered.");
    }
    CommandResult::message(out)
}

fn info(app: &App, arg: &str) -> CommandResult {
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /extension info <id>");
    }
    // Compiled-in lookup.
    let discovered = codesmith_extensions::discover_static();
    if let Some(reg) = discovered.iter().find(|r| r.metadata.id == id) {
        return CommandResult::message(format!(
            "id: {}\nversion: {}\nsource: compiled-in\ncontributions: (see /extension status)\n",
            reg.metadata.id, reg.metadata.version
        ));
    }
    // §F5b — dylib lookup.
    let global_dir = crate::config::effective_home_dir()
        .map(|home| home.join(".codesmith").join("extensions"));
    let project_dir = app.workspace.join(".codesmith").join("extensions");
    let dylibs = codesmith_extensions::discover_dylib(global_dir.as_deref(), &project_dir, &[]);
    if let Some(d) = dylibs.into_iter().find(|d| d.manifest.id == id) {
        return CommandResult::message(format!(
            "id: {}\nversion: {}\nsource: dylib ({})\npath: {}\ncontributions: (see /extension status)\n",
            d.manifest.id,
            d.manifest.version,
            match d.source {
                codesmith_extensions::DiscoveredSource::Global => "global",
                codesmith_extensions::DiscoveredSource::ProjectLocal => "project",
                codesmith_extensions::DiscoveredSource::ConfiguredPath => "configured",
            },
            d.dylib_path.display()
        ));
    }
    CommandResult::error(format!("No extension with id '{id}'."))
}
```
> `list`'s signature changes from `fn list(_app: &App)` to `fn list(app: &App)` and `info` from `fn info(_app, ..)` to `fn info(app, ..)` (both now use `app.workspace`); the `try_dispatch` call site (`:35-36`) already passes `app`, so no caller change. `install_stub`/`uninstall_stub` stay stub (§F5c).

- [ ] **Step 2: build + run the tui suite (dispatch smoke regression).** Run:
  - `cargo +1.90.0 build --workspace` — green.
  - `cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui` — 2855+2 (the `/extension` smoke tests in `commands/mod.rs` still dispatch; list/info now also scan dylib dirs — a no-op when none exist).

- [ ] **Step 3: commit.**
```bash
git add crates/tui/src/commands/extension_commands.rs
git commit -m "feat(framework): §F5b T6 /extension list+info surface dylib-discovered ext (list merges discover_static()+discover_dylib(global,project,[]) deduped by id with [compiled]/[dylib,global|project] tags; info falls back to dylib lookup by id showing source+path; list signature _app→app for workspace; install/uninstall stubs unchanged→§F5c; tui 2855+2 no-regression—existing dispatch smoke green; dylib display e2e deferred per §F5 precedent)"
```

---

## Task 7: docs — EXTENSIONS host-seam + Sandbox Stance; ROADMAP §F5b progress + `### F5b` subsection

**Files:**
- Modify: `docs/EXTENSIONS.md` (intro `:32`, host-seam table after `:251`, Sandbox Stance `:272-274`)
- Modify: `ROADMAP.md` (§F2c next-focus §F5 bullet `:2549`, §F5b progress block before `---` `:2575`, `### F5b` subsection after `:3018`)

- [ ] **Step 1: EXTENSIONS.md intro §F5b sentence (`:32`).** Insert after `no dylib machinery).` (line 32, before ` \`ToolExecutionUpdate\``):
```
§F5b (dylib LOAD side — `libloading` loader + `extension.toml` manifest + three-source discovery + project-local trust gate [Model A, consume `FirstLoad`/`is_workspace_trusted`] + reload wiring) is done. The INSTALL side (install-source impls + `CargoBuilder`/`Placer` + `/extension install`/`uninstall` real impl + `installed[]` provenance write) remains §F5c; this slice loads dylibs from disk, it does not fetch/build/place them.
```

- [ ] **Step 2: EXTENSIONS.md host-seam dylib row (after the `ProjectTrust` row at `:251`).** Insert a new row after line 251:
```
| `—` (dylib LOAD, not an event) | `populate_extension_runtime` (`tui/src/core/engine.rs`) after `discover_static` | n/a (load phase) | §F5b: `discover_dylib(global, project, configured=[])` → `apply_trust_gate(is_workspace_trusted(workspace))` → `state.is_enabled` reconcile → `ExtensionRunner::load_dylib` on the OS-thread load runtime; reload auto-picks-up via `reload_extension_runtime`→`populate`. `ExtensionRunner.libraries` keeps `Library` handles (reload does not clear — correctness for append-insert tools / no `clear_tools`). Lockstep `*mut dyn Extension` via `codesmith_register_extension` (§8.2). |
```

- [ ] **Step 3: EXTENSIONS.md Sandbox Stance revision (`:272-274`).** Replace the clause `The dylib loader, \`extension.toml\`, and project-local discovery trust gate remain §F5 续作 / §F3+.` (lines 272-274) with:
```
The dylib loader (`libloading` + lockstep `*mut dyn Extension` via `codesmith_register_extension`), `extension.toml` manifest, and project-local discovery trust gate (Model A — `apply_trust_gate` drops `ProjectLocal` dylibs when `is_workspace_trusted(workspace)` is false; the `ProjectTrust { FirstLoad }` event flips that trust at onboarding accept) are §F5b (done) — but the loader only *loads* dylibs from disk; install (fetch/build/place) + `installed[]` provenance remain §F5c. A loaded dylib runs in-process with full host access — trust the source; containerize for untrusted sources.
```

- [ ] **Step 4: ROADMAP §F2c next-focus §F5 bullet (`:2549`).** Replace line 2549:
```
- §F5b 已落地（见下 §F5b 进度块）：dylib LOAD 侧（`libloading` loader + `extension.toml` manifest + 三源发现 + 项目本地 trust gate [Model A] + reload wiring）。剩余 §F5：INSTALL 侧（install-source impls + `CargoBuilder`/`Placer` + `/extension install`/`uninstall` 真实现 + `installed[]` provenance 写）→ §F5c。
```

- [ ] **Step 5: ROADMAP §F5b progress block (before the `---` at `:2575`).** Insert a new progress card after line 2575 (`---`) and before `## §A` (`:2577`), mirroring the §F5 progress-block structure (`:2552-2573`):
```
**进度（2026-07-22 §F5b dylib LOAD 侧——§F5 续作上半：disk dylib loader + extension.toml manifest + 三源发现 + 项目本地 trust gate[Model A, consume FirstLoad] + reload wiring，`feat/pluggable-framework-core`）：**

接 §F5 slice 1（`FirstLoad` emit site）。§F5b 是该 emit 的第一个真实 consumer：trust gate 读 `is_workspace_trusted(workspace)`（`FirstLoad` 接受翻转的持久化信任），不信任则跳过项目本地 dylib。LOAD 半落地：loader + manifest + 发现 + trust gate + reload wiring + cdylib fixture。INSTALL 半（fetch/build/place/provenance 写）保持 stub → §F5c。raw `libloading` + lockstep `*mut dyn Extension`（Approach 1，§2.4 无 ABI churn）。spec：`docs/superpowers/specs/2026-07-22-codesmith-extension-system-slice-5b-design.md`；plan：`docs/superpowers/plans/2026-07-22-codesmith-extension-system-slice-5b.md`。

**关键设计决策：**
- **范围 fork = split LOAD/INSTALL**：本切片 = LOAD；install/uninstall + install-source impls + `installed[]` provenance 写 → §F5c（保持 stub）。
- **ABI fork = raw libloading + lockstep**：`codesmith_register_extension() -> *mut dyn Extension` → `Box::from_raw` → `runner.load`。无 `abi_stable`（§2.4）。`ExtensionError::Load` 已在契约。
- **Q1 `ExtensionRunner.libraries`**：`Mutex<Vec<Library>>`，reload 不清——对 append-insert tools / 无 `clear_tools` 的现状是*正确性保底*（移除 dylib 的 tool Arc 仍引用旧 vtable），重发现同 dylib 则*有界泄漏*。
- **Q2 Model A trust gate**：`apply_trust_gate(entries, is_workspace_trusted(workspace))` 丢 `ProjectLocal`；discovery trust-agnostic（host 注入 bool）。
- **Q3 fixture = cdylib dev-dep**：`crate-type=["cdylib","rlib"]` dev-dep → `cargo test --lib` 构建 cdylib；`build.rs` 从 `OUT_DIR` 算路径发 env（无 cargo subprocess/lock）。
- **Q4 `api_version` 可选 warn**：不 refuse（lockstep 由 build 强制）。

**落地步骤：**
1. T1 `manifest.rs` + `toml` dep + parse 测试。
2. T2 `loader.rs` + `libloading` + `runner.libraries`/`load_dylib` + 错误测试。
3. T3 `discover_dylib` + `apply_trust_gate` + 发现/trust-gate 测试。
4. T4 cdylib fixture crate + `build.rs` + load-contributions 测试。
5. T5 `populate_extension_runtime` dylib wiring（reload 自动拾取）。
6. T6 `/extension list`+`info` 显示 dylib ext。
7. T7 docs（EXTENSIONS + ROADMAP）。

**测试/验证：** `cargo +1.90.0 build --workspace` 全绿；`codesmith-extensions --lib` 15→26（3 manifest + 2 loader + 5 discovery/trust-gate + 1 fixture）；`codesmith-agent --lib` 98（不变）；`codesmith-agent-runtime --lib` 1163+2（不变——LOAD 不触 host_executor；flaky `streamable_http_stale_session...` 隔离重跑绿）；`codesmith-tui --bin codesmith-tui` 2855+2（不变——tui dylib e2e deferred per §F5 precedent）；grep `libloading` in extensions Cargo.toml ≥1、`loader.rs`/`manifest.rs` 存在、`discover_dylib` in engine.rs ≥1、`codesmith_register_extension` in fixture ≥1、host_executor `.emit`=16（不变）、`TrustReason::FirstLoad` in tui=1（不变）。

**By-design gaps（显式 out-of-scope）：**
- §F5c INSTALL 侧：install-source impls（Git/LocalPath must-have；CratesIo/Prebuilt nice-to-have）+ `CargoBuilder` + `Placer` + `/extension install`/`uninstall` 真实现 + `installed[]` provenance 写。
- `clear_tools`/`clear_commands` + Library 真卸载（Q1 接受 bounded 留存保底正确性）。
- tui-level dylib e2e（`run_tui` 触发发现/reload）：§F5 precedent（`EngineHost`+`run_tui`+真信任 fixture 比例失衡）。

**下一聚焦工作：**
- §F5c INSTALL 侧 + 残项（按需）。
```

- [ ] **Step 6: ROADMAP `### F5b` subsection (after `:3018`).** Append after the `### F5` subsection's last line (`:3018`, end of file), mirroring `### F5` structure (`:2993-3018`):
```

### F5b — Dylib LOAD side (loader + manifest + discovery + trust gate + reload wiring)

- `crates/extensions/src/loader.rs` (`load_dylib(path) -> (Library, Box<dyn
  Extension>)`): `libloading::Library::new` → symbol
  `codesmith_register_extension` → `Box::from_raw`. `ExtensionRunner.libraries:
  Mutex<Vec<Library>>` + `load_dylib` method pushes the Library (reload does
  NOT clear — Q1 correctness for append-insert tools / no `clear_tools`) then
  `configure`s via `load`. `Extension` Box drops after `configure`
  (contributions self-contained; vtables in the kept Library).
- `crates/extensions/src/manifest.rs` (`ExtensionManifest` serde
  Deserialize: `id`/`version`/`entry?`/`source?[type,ref?]`/`api_version?`);
  `from_str`/`parse` → `ExtensionError::Load`. Q4: `api_version` optional,
  warn-only (lockstep is build-enforced, §8.2).
- `crates/extensions/src/discovery.rs` `discover_dylib(global?, project,
  configured[])`: one-level-deep scan (subdir+`extension.toml` → parsed
  manifest/default `<DLL_PREFIX><id>.<DLL_EXTENSION>` entry, or bare
  `*.<DLL_EXTENSION>` → stem id); dedup by canonical dylib path first-wins;
  `DiscoveredSource{Global,ProjectLocal,ConfiguredPath}` tag.
  `apply_trust_gate(entries, project_trusted: bool)` drops `ProjectLocal`
  when untrusted — host injects `is_workspace_trusted(workspace)` (Model A,
  the first real consumer of the `FirstLoad`→persisted-trust flip).
- `crates/tui/src/core/engine.rs` `populate_extension_runtime`: after static
  discover/reconcile/load, `discover_dylib`→`apply_trust_gate`→`state.is_enabled`
  reconcile→`load_dylib` on the same OS-thread load runtime (warn+continue on
  error, §8.3). `reload_extension_runtime` unchanged — `/extension reload`
  auto-picks-up dylibs.
- `crates/extensions-fixture-dylib` (workspace member, `crate-type=
  ["cdylib","rlib"]`): `codesmith_register_extension` returns a `Box<dyn
  Extension>` registering `fixture_echo` tool + `TurnStart` handler
  (`FIXTURE_SEEN`). Built as a `codesmith-extensions` **dev-dep** so
  `cargo test --lib` builds the cdylib; `build.rs` computes its path from
  `OUT_DIR` (no cargo subprocess/lock). Lockstep by construction.
- `/extension list`+`info` surface dylib-discovered ext (deduped by id).

**Status (slice 5b §F5b):** done. Dylib LOAD side landed. Still deferred
(§F5c): install-source impls (Git/LocalPath must-have; CratesIo/Prebuilt
nice-to-have) + `CargoBuilder` + `Placer` + `/extension install`/`uninstall`
real impl + `installed[]` provenance write; `clear_tools`/`clear_commands` +
Library unload (Q1 accepts bounded retention). tui dylib e2e deferred per §F5
precedent. Remaining §F3–§F8 unchanged.
```

- [ ] **Step 7: verify no regressions (docs-only change).** Run: `cargo +1.90.0 build --workspace` — green (no code change). Optionally re-run `cargo +1.90.0 test -p codesmith-extensions --lib` to confirm T1-T4 still pass.

- [ ] **Step 8: commit.**
```bash
git add docs/EXTENSIONS.md ROADMAP.md
git commit -m "docs(framework): §F5b T7 (EXTENSIONS intro §F5b sentence + host-seam dylib LOAD row in populate_extension_runtime + Sandbox Stance revision: dylib loader/manifest/trust gate §F5b done—loads from disk only, install→§F5c, loaded dylib runs in-process trust-the-source; ROADMAP §F2c next-focus §F5 bullet marks §F5b done + new §F5b progress block before §A + ### F5b subsection mirroring ### F5 with Status/Still-deferred §F5c; no code change—T7 doc-only, all 4 suites green at T6 commit unchanged)"
```

---

## Verification gate (slice end — not committed)
- [ ] `cargo +1.90.0 build --workspace` green (incl. the new `extensions-fixture-dylib` member).
- [ ] `cargo +1.90.0 test -p codesmith-extensions --lib` = 26 (was 15; +3 manifest + 2 loader + 5 discovery/trust-gate + 1 fixture).
- [ ] `cargo +1.90.0 test -p codesmith-agent --lib` = 98 (unchanged — contract read-only, no enum/trait change).
- [ ] `cargo +1.90.0 test -p codesmith-agent-runtime --lib` = 1163+2 (unchanged — LOAD does not touch `host_executor`; if only `streamable_http_stale_session...` fails, re-run in isolation to confirm green).
- [ ] `cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui` = 2855+2 (unchanged — no new tui tests; tui dylib wiring/list e2e deferred per §F5 precedent).
- [ ] `grep -c 'libloading' crates/extensions/Cargo.toml` ≥ 1.
- [ ] `grep -n 'toml.workspace' crates/extensions/Cargo.toml` = 1.
- [ ] `ls crates/extensions/src/{loader,manifest}.rs` exist; `ls crates/extensions/build.rs` exists.
- [ ] `grep -rn 'discover_dylib' crates/tui/src/core/engine.rs` ≥ 1 (populate call site).
- [ ] `grep -rn 'codesmith_register_extension' crates/extensions-fixture-dylib/` ≥ 1.
- [ ] **must-not-regress:** `grep -c '\.emit(codesmith_agent::extension::ExtensionEvent' crates/agent-runtime/src/engine/host_executor.rs` = 16; `grep -rn 'TrustReason::FirstLoad' crates/tui/src` = 1.

## Out of scope (explicitly deferred — see §2 + T7 docs)
- **§F5c INSTALL side:** install-source impls (Git / LocalPath must-have; CratesIo / Prebuilt nice-to-have) + `CargoBuilder` + `Placer` + `/extension install`/`uninstall` real impl + `installed[]` provenance write (`ExtensionStateStore` has no mutator — §F5c adds). `install_stub`/`uninstall_stub` stay stub.
- **`abi_stable`** — rejected (§2.4 no ABI churn; raw `libloading` + lockstep).
- **`clear_tools`/`clear_commands` + Library real unload** — later (Q1 accepts bounded retention as correctness-preserving).
- **`settings.extensions[]` configured-paths source** — `discover_dylib` accepts `configured: &[PathBuf]` (plumbed, empty in §F5b); config backing + wiring → §F5c.
- **hot-load** — never (§2.4; reload is clean break).
- **full event-set emit wiring** (§F2/§F3+), `EventBus` impl, `registerProvider`, renderer/shortcut/flag — each its own §F slice, unchanged.
- **tui-level dylib e2e** (`run_tui` triggering dylib discovery/reload) — deferred per §F5 / §F2b `SessionBeforeSwitch` precedent; the dylib-load mechanism is covered by T4 (runner-level fixture) + discovery by T3, the wiring by build + no-regression.
