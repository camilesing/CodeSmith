# §F5e — CratesIo + Prebuilt INSTALL source impls — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the tui-layer `install_precheck` "§F5c-later" early-return for `crate:`/`prebuilt:` source kinds with real `CratesIoSource` + `PrebuiltDylibSource` impls that flow through `Installer::install` end-to-end.

**Architecture:** Add 3 new types to `crates/extensions/src/install_source.rs` (`CratesIoSource`, `PrebuiltDylibSource`, `IdentityBuilder`) + an `HttpFetcher` trait with a curl shell-out real impl (`CurlHttpFetcher`) + a test-only `FakeHttpFetcher` (trait-DI for no-network unit tests, mirroring §F5c's `FakeSource`/`FakeBuilder` pattern in `installer.rs:162-180`). `SourceSpec` gains a `checksum` field + `--checksum <hex>` flag + kind-dependent `@`-split. The tui `install_precheck` drops the crate/prebuilt early-return; `install()` constructs the right source+builder per kind (`CargoBuilder` for git/path/crate, `IdentityBuilder` for prebuilt) + emits the §F5c trust-warn + a new prebuilt checksum-absent warn. curl shell-out (3rd after git/cargo) + `tar -xzf` for `.crate` extraction + `sha2` (workspace dep) for checksum; **zero new external crate dep**.

**Tech Stack:** Rust (edition 2024, rustc 1.90.0), plain `cargo`. New dep: `sha2` (already a workspace dep at root `Cargo.toml:59`; promoted into `crates/extensions/Cargo.toml`). HTTP via `curl` shell-out (`std::process::Command`); `.crate` extract via `tar` shell-out; JSON parse via `serde_json` (already a dep).

**Spec:** `docs/superpowers/specs/2026-07-27-codesmith-extension-system-slice-5e-design.md` (commit `c257adab` on `feat/f5e-cratesio-prebuilt`).

**Baseline (main HEAD `2d66279b`):** ext `51` / agent `98` / agent-runtime `1165+2 ignored` / tui `2866 pass + 26 pre-existing runtime_api env-fail + 2 ignored`. Report tui as "N pass/26 pre-existing runtime_api fail/2 ignored" — never "green", never attributed to §F5e.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/extensions/Cargo.toml` | modify | add `sha2.workspace = true` (T4) |
| `crates/extensions/src/install_source.rs` | modify | T1 `HttpFetcher`+`CurlHttpFetcher`+`FakeHttpFetcher`; T2 `SourceSpec` extensions; T3 `IdentityBuilder`; T4 `CratesIoSource`; T5 `PrebuiltDylibSource` + new test modules |
| `crates/extensions/src/lib.rs` | modify | re-export new public types (folded into T1/T3/T4/T5) |
| `crates/tui/src/commands/extension_commands.rs` | modify | T6 drop `install_precheck` crate/prebuilt early-return; `install()` real source+builder construction per kind; checksum-absent warn; rewrite 2 tests + add 1 |
| `docs/EXTENSIONS.md` | modify | T7 install source-kind section: drop "§F5c-later" for crate/prebuilt; document `crate:`/`prebuilt:` syntax + `--checksum` + HTTPS-only |
| `ROADMAP.md` | modify | T7 §F5c "By-design gaps" strikethrough-correct CratesIo/Prebuilt → done; add `### F5e` progress block |

`crates/extensions/src/installer.rs` is **unchanged** (`Installer::install` + `manifest_kind` already kind-aware; `IdentityBuilder` is just another `ExtensionBuilder` impl).

## Task dependency graph

```
T1 (HttpFetcher) ──┬──> T4 (CratesIoSource) ──┐
T2 (SourceSpec)  ──┼──> T5 (PrebuiltDylibSource)─┼──> T6 (tui wiring) ──> T7 (docs)
T3 (IdentityBuilder) ─────────────────────────┘
```
T1, T2, T3 are independent (parallelizable). T4 + T5 need T1. T5 reuses the `sha2` dep added in T4. T6 needs T1-T5. T7 needs T6.

---

## Task 1: `HttpFetcher` trait + `CurlHttpFetcher` + `FakeHttpFetcher`

**Files:**
- Modify: `crates/extensions/src/install_source.rs` (add types after the `Placer` impl ~`:321`, before `#[cfg(test)] mod source_spec_tests`)
- Modify: `crates/extensions/src/lib.rs` (re-export `HttpFetcher`, `CurlHttpFetcher`)

- [ ] **Step 1: Write the failing tests**

Append a new test module at the end of `crates/extensions/src/install_source.rs`:

```rust
#[cfg(test)]
mod http_fetcher_tests {
    use super::*;

    #[test]
    fn fake_http_returns_canned_text() {
        let h = FakeHttpFetcher::new().with("https://x/i", br#"{"vers":"1.0"}"#.to_vec());
        let t = h.fetch_text("https://x/i").unwrap();
        assert!(t.contains("\"vers\":\"1.0\""), "{t}");
        let r = h.fetch_text("https://x/missing");
        assert!(matches!(r, Err(ExtensionError::Install(_))), "{r:?}");
    }

    #[test]
    fn fake_http_fetch_to_writes_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out.bin");
        let h = FakeHttpFetcher::new().with("https://x/y", b"dylib-bytes".to_vec());
        h.fetch_to("https://x/y", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"dylib-bytes");
    }

    /// Real curl fetch of the crates.io sparse index. Skip if `curl` not on
    /// PATH; `#[ignore]` (network) — opt in via `cargo test -- --ignored`.
    #[test]
    #[ignore = "network: curls index.crates.io"]
    fn curl_http_fetch_text_reads_sparse_index() {
        if std::process::Command::new("curl")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("curl not on PATH; skipping");
            return;
        }
        let h = CurlHttpFetcher::new();
        let body = h
            .fetch_text("https://index.crates.io/se/rd/serde")
            .expect("curl sparse index");
        assert!(
            body.contains("\"name\":\"serde\""),
            "body head: {}",
            &body[..body.len().min(200)]
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codesmith-extensions --lib http_fetcher_tests`
Expected: COMPILE ERROR — `cannot find type \`FakeHttpFetcher\`` / `CurlHttpFetcher` / `HttpFetcher` not found.

- [ ] **Step 3: Write minimal implementation**

In `crates/extensions/src/install_source.rs`, add after the `Placer` impl (before the `#[cfg(test)] mod source_spec_tests` line):

```rust
/// HTTP fetch abstraction (§F5e Q1). Real impl `CurlHttpFetcher` shells out
/// to `curl`; tests inject `FakeHttpFetcher`. `Send + Sync` so
/// `Arc<dyn HttpFetcher>` crosses the tui→Installer handoff (install runs on
/// the UI thread; tests may cross threads).
pub trait HttpFetcher: Send + Sync {
    /// Fetch `url` bytes into the file at `dest`. TLS verified (curl default).
    fn fetch_to(&self, url: &str, dest: &Path) -> Result<(), ExtensionError>;
    /// Fetch `url` body as text (e.g. the crates.io sparse-index JSON-lines).
    fn fetch_text(&self, url: &str) -> Result<String, ExtensionError>;
}

/// curl shell-out `HttpFetcher` (§F5e Q1, 3rd shell-out after git/cargo).
/// `curl -fsSL -A <ua> <url> [-o <dest>]`. `-f` fails on HTTP errors, `-S`
/// shows errors, `-L` follows redirects, default cert verification. Assumes
/// `curl` on PATH (like `git`/`cargo` per §F5c). A `User-Agent` is required
/// by crates.io (index + static) — without it the registry 4xx's.
pub struct CurlHttpFetcher {
    user_agent: String,
}

impl CurlHttpFetcher {
    pub fn new() -> Self {
        Self {
            user_agent: format!(
                "codesmith-extensions/{} (install)",
                env!("CARGO_PKG_VERSION")
            ),
        }
    }
}

impl Default for CurlHttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpFetcher for CurlHttpFetcher {
    fn fetch_to(&self, url: &str, dest: &Path) -> Result<(), ExtensionError> {
        let out = Command::new("curl")
            .arg("-fsSL")
            .arg("-A")
            .arg(&self.user_agent)
            .arg(url)
            .arg("-o")
            .arg(dest)
            .output()
            .map_err(|e| ExtensionError::Install(format!("spawn curl (on PATH?): {e}")))?;
        if !out.status.success() {
            return Err(ExtensionError::Install(format!(
                "curl {} failed: {}",
                url,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(())
    }

    fn fetch_text(&self, url: &str) -> Result<String, ExtensionError> {
        let out = Command::new("curl")
            .arg("-fsSL")
            .arg("-A")
            .arg(&self.user_agent)
            .arg(url)
            .output()
            .map_err(|e| ExtensionError::Install(format!("spawn curl (on PATH?): {e}")))?;
        if !out.status.success() {
            return Err(ExtensionError::Install(format!(
                "curl {} failed: {}",
                url,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Test-only `HttpFetcher` (§F5e). Holds a URL→bytes map; unknown URL →
/// `ExtensionError::Install`. Lets `CratesIoSource`/`PrebuiltDylibSource`
/// unit tests run with no network (mirrors §F5c `FakeSource`/`FakeBuilder`).
#[cfg(test)]
pub struct FakeHttpFetcher {
    responses: std::collections::HashMap<String, Vec<u8>>,
}

#[cfg(test)]
impl FakeHttpFetcher {
    pub fn new() -> Self {
        Self {
            responses: std::collections::HashMap::new(),
        }
    }

    /// Register a canned response body for `url`.
    pub fn with(mut self, url: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        self.responses.insert(url.into(), body.into());
        self
    }
}

#[cfg(test)]
impl Default for FakeHttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl HttpFetcher for FakeHttpFetcher {
    fn fetch_to(&self, url: &str, dest: &Path) -> Result<(), ExtensionError> {
        let body = self
            .responses
            .get(url)
            .ok_or_else(|| ExtensionError::Install(format!("FakeHttp: no canned response for {url}")))?;
        std::fs::write(dest, body)
            .map_err(|e| ExtensionError::Install(format!("FakeHttp write {url}: {e}")))?;
        Ok(())
    }

    fn fetch_text(&self, url: &str) -> Result<String, ExtensionError> {
        let body = self
            .responses
            .get(url)
            .ok_or_else(|| ExtensionError::Install(format!("FakeHttp: no canned response for {url}")))?;
        Ok(String::from_utf8_lossy(body).into_owned())
    }
}
```

Add the re-exports to `crates/extensions/src/lib.rs` — find the existing `pub use install_source::{...}` line and add `CurlHttpFetcher`, `HttpFetcher` to the list (the `FakeHttpFetcher` is `#[cfg(test)]` so it is NOT re-exported; source-impl tests use it via `use super::*` in the same crate).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-extensions --lib http_fetcher_tests`
Expected: `2 passed; 0 failed; 1 ignored` (the network `curl_http_fetch_text_reads_sparse_index` is `#[ignore]`).

Then run the full crate to confirm no regression:
Run: `cargo test -p codesmith-extensions --lib`
Expected: `53 passed; 0 failed; 1 ignored` (51 baseline + 2 new http_fetcher_tests; 1 ignored).

- [ ] **Step 5: Commit**

```bash
git add crates/extensions/src/install_source.rs crates/extensions/src/lib.rs
git commit -m "feat(extensions): §F5e T1 HttpFetcher trait + CurlHttpFetcher (curl shell-out) + FakeHttpFetcher (test-only)

3rd shell-out after GitSource(git)/CargoBuilder(cargo); zero new external crate dep. curl -fsSL -A <ua> <url> [-o <dest>] (HTTPS default-verified; -f fails on HTTP errors; -L follow redirects; UA required by crates.io). FakeHttpFetcher (#[cfg(test)]) holds URL→bytes map for no-network CratesIoSource/PrebuiltDylibSource unit tests (mirrors §F5c FakeSource/FakeBuilder pattern installer.rs:162-180). Send+Sync bound so Arc<dyn HttpFetcher> crosses tui→Installer handoff. Re-exported HttpFetcher/CurlHttpFetcher from lib.rs (FakeHttpFetcher test-only, not re-exported).

ext 51→53 (+2 http_fetcher_tests; 1 #[ignore] network curl sparse-index). docs-only spec c257adab precedes. Design: spec §3 Q1 + §9."
```

---

## Task 2: `SourceSpec` parser — `checksum` field + `--checksum` flag + kind-dependent `@`-split

**Files:**
- Modify: `crates/extensions/src/install_source.rs` (`SourceSpec` struct `:69-74` + `SourceSpec::parse` `:80-126` + add `is_valid_sha256_hex` helper; tests in `source_spec_tests` `:324-394`)

- [ ] **Step 1: Write the failing tests**

In the `source_spec_tests` module (`crates/extensions/src/install_source.rs`), append these tests (after the existing `parse_missing_spec_token_is_install_error`):

```rust
    #[test]
    fn parse_checksum_flag_sets_checksum_field() {
        let s = SourceSpec::parse(
            "prebuilt:https://x/y.dylib --checksum \
             d1bb2d9926b9bd18e51fc8edd663e311ff3b1fb96c9d4689854f8686f7c6c216",
        )
        .unwrap();
        assert_eq!(s.kind, SourceKind::Prebuilt);
        assert_eq!(
            s.checksum.as_deref(),
            Some("d1bb2d9926b9bd18e51fc8edd663e311ff3b1fb96c9d4689854f8686f7c6c216")
        );
    }

    #[test]
    fn parse_checksum_must_be_64_lowercase_hex() {
        let r = SourceSpec::parse("prebuilt:x --checksum abc123");
        assert!(matches!(r, Err(ExtensionError::Install(_))), "{r:?}");
        let r = SourceSpec::parse(
            "prebuilt:x --checksum D1BB2D9926B9BD18E51FC8EDD663E311FF3B1FB96C9D4689854F8686F7C6C216",
        );
        assert!(matches!(r, Err(ExtensionError::Install(_))), "uppercase rejected: {r:?}");
    }

    #[test]
    fn parse_checksum_requires_value() {
        let r = SourceSpec::parse("prebuilt:x --checksum");
        assert!(matches!(r, Err(ExtensionError::Install(_))), "{r:?}");
    }

    #[test]
    fn parse_prebuilt_url_with_at_not_split() {
        // URL with @ userinfo must NOT be @-split (would mangle to ref_).
        let s = SourceSpec::parse("prebuilt:https://u:p@host.example/y.dylib").unwrap();
        assert_eq!(s.body, "https://u:p@host.example/y.dylib");
        assert_eq!(s.ref_, None);
        assert_eq!(s.checksum, None);
    }

    #[test]
    fn parse_crate_with_version_splits_at() {
        let s = SourceSpec::parse("crate:serde@1.0.204").unwrap();
        assert_eq!(s.kind, SourceKind::CratesIo);
        assert_eq!(s.body, "serde");
        assert_eq!(s.ref_.as_deref(), Some("1.0.204"));
    }

    #[test]
    fn parse_crate_no_version_has_no_ref() {
        let s = SourceSpec::parse("crate:serde").unwrap();
        assert_eq!(s.body, "serde");
        assert_eq!(s.ref_, None);
    }

    #[test]
    fn parse_path_with_at_not_split() {
        let s = SourceSpec::parse("path:/a@b").unwrap();
        assert_eq!(s.body, "/a@b");
        assert_eq!(s.ref_, None);
    }

    #[test]
    fn parse_unknown_flag_is_install_error() {
        let r = SourceSpec::parse("git:foo/bar --bogus");
        assert!(matches!(r, Err(ExtensionError::Install(_))), "{r:?}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codesmith-extensions --lib source_spec_tests`
Expected: COMPILE ERROR — `no field \`checksum\` on type \`SourceSpec\``, + the prebuilt-`@` tests fail (current parser always `rsplit_once('@')`, mangling the userinfo URL).

- [ ] **Step 3: Write minimal implementation**

In `crates/extensions/src/install_source.rs`, replace the `SourceSpec` struct (`:69-74`) + `parse` (`:80-126`) with:

```rust
/// Parsed `/extension install <spec> [--global] [--checksum <hex>]` source
/// spec (§F5c + §F5e). Grammar: `<kind>:<body>[@<ref>]` where
/// `kind ∈ {git, path, crate, prebuilt}`. `@<ref>` splits ONLY for git + crate
/// (git ref / crate version); path + prebuilt take the body whole (prebuilt
/// URLs may contain `@` userinfo — §F5e sub-choice B). `--checksum <sha256>`
/// applies to prebuilt (kind-agnostic field; git/path/crate ignore it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpec {
    pub kind: SourceKind,
    pub body: String,
    pub ref_: Option<String>,
    pub scope: InstallScope,
    pub checksum: Option<String>,
}

impl SourceSpec {
    /// Parse a `/extension install` arg string. `--global` → `Global` scope
    /// (default `Project`); `--checksum <64-hex>` sets the prebuilt checksum.
    /// The first non-`--` token (not consumed as a flag value) is the
    /// `<kind>:<body>` spec. Unknown `--` flags → `Install` error (§F5e).
    pub fn parse(arg: &str) -> Result<Self, ExtensionError> {
        let mut scope = InstallScope::Project;
        let mut checksum: Option<String> = None;
        let mut spec_token: Option<&str> = None;
        let mut tokens = arg.split_whitespace().peekable();
        while let Some(t) = tokens.next() {
            match t {
                "--global" => scope = InstallScope::Global,
                "--checksum" => {
                    let val = tokens.next().ok_or_else(|| {
                        ExtensionError::Install(
                            "--checksum requires a value (64 lowercase hex chars)".into(),
                        )
                    })?;
                    if !is_valid_sha256_hex(val) {
                        return Err(ExtensionError::Install(format!(
                            "invalid --checksum: expected 64 lowercase hex chars, got {val:?}"
                        )));
                    }
                    checksum = Some(val.to_string());
                }
                _ if t.starts_with("--") => {
                    return Err(ExtensionError::Install(format!("unknown flag {t:?}")));
                }
                _ => {
                    if spec_token.is_none() {
                        spec_token = Some(t);
                    }
                }
            }
        }
        let spec_token = spec_token.ok_or_else(|| {
            ExtensionError::Install("missing source spec (expected `<kind>:<body>[@<ref>]`)".into())
        })?;
        let (kind_str, rest) = spec_token.split_once(':').ok_or_else(|| {
            ExtensionError::Install(format!("source spec must be `<kind>:<body>`; got {spec_token:?}"))
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
        // §F5e sub-choice B: kind-dependent @-split. git/crate split
        // (ref / version on ref_); path/prebuilt take body whole.
        let (body, ref_) = match kind {
            SourceKind::Git | SourceKind::CratesIo => match rest.rsplit_once('@') {
                Some((b, r)) if !r.is_empty() => (b.to_string(), Some(r.to_string())),
                _ => (rest.to_string(), None),
            },
            SourceKind::Path | SourceKind::Prebuilt => (rest.to_string(), None),
        };
        if body.is_empty() {
            return Err(ExtensionError::Install("source body is empty".into()));
        }
        Ok(SourceSpec {
            kind,
            body,
            ref_,
            scope,
            checksum,
        })
    }
}

/// 64 lowercase hex chars (sha256 digest). Used for `--checksum` validation.
fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-extensions --lib source_spec_tests`
Expected: all `source_spec_tests` pass (the existing 9 + the 8 new = 17).

Then full crate:
Run: `cargo test -p codesmith-extensions --lib`
Expected: `61 passed; 0 failed; 1 ignored` (53 from T1 + 8 new source_spec_tests).

- [ ] **Step 5: Commit**

```bash
git add crates/extensions/src/install_source.rs
git commit -m "feat(extensions): §F5e T2 SourceSpec --checksum flag + kind-dependent @-split

SourceSpec gains checksum: Option<String> + --checksum <64-lowercase-hex> validation (sub-choice B; matches --global flag precedent; URLs with @ userinfo don't collide). Kind-dependent @-split: git/crate split (ref/version on ref_); path/prebuilt take body whole (prebuilt URL with @ userinfo previously mangled by always-rsplit_once). Unknown -- flags now error (was silently ignored). Existing parse tests unchanged (git @ref / crate no-@ / path no-@ / --global all preserved).

ext 53→61 (+8 source_spec_tests). Design: spec §3 sub-choice B + §6."
```

---

## Task 3: `IdentityBuilder` no-op

**Files:**
- Modify: `crates/extensions/src/install_source.rs` (add `IdentityBuilder` after `CargoBuilder` impl ~`:285`)
- Modify: `crates/extensions/src/lib.rs` (re-export `IdentityBuilder`)

- [ ] **Step 1: Write the failing test**

Append a new test module at the end of `crates/extensions/src/install_source.rs`:

```rust
#[cfg(test)]
mod identity_builder_tests {
    use super::*;

    #[test]
    fn identity_builder_returns_input_path() {
        let b = IdentityBuilder;
        let src = std::env::temp_dir().join("fake.dylib");
        let out = b.build(&src).expect("identity build");
        assert_eq!(out, src, "identity builder returns input as-is");
    }

    #[test]
    fn identity_builder_accepts_file_path() {
        // Prebuilt path: build()'s src_dir param receives a FILE (the dylib),
        // not a dir (as for CargoBuilder). Documented in the impl.
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("x.dylib");
        std::fs::write(&file, b"binary").unwrap();
        let out = IdentityBuilder.build(&file).unwrap();
        assert_eq!(out, file);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codesmith-extensions --lib identity_builder_tests`
Expected: COMPILE ERROR — `cannot find type \`IdentityBuilder\``.

- [ ] **Step 3: Write minimal implementation**

In `crates/extensions/src/install_source.rs`, add after the `CargoBuilder` impl (before `pub struct Placer` ~`:287`):

```rust
/// No-op `ExtensionBuilder` for the prebuilt path (§F5e Q3). The fetched
/// artifact IS already a dylib (no build needed); `build(src)` returns `src`
/// as-is. For prebuilt, `src` is a file path (the downloaded dylib), NOT a
/// directory as for `CargoBuilder`. `Installer::install` then D8-temp-loads
/// it (runs `codesmith_register_extension` for id/version) → places → manifest.
pub struct IdentityBuilder;

impl ExtensionBuilder for IdentityBuilder {
    fn build(&self, src: &Path) -> Result<PathBuf, ExtensionError> {
        Ok(src.to_path_buf())
    }
}
```

Add `IdentityBuilder` to the `pub use install_source::{...}` re-export in `crates/extensions/src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-extensions --lib identity_builder_tests`
Expected: `2 passed; 0 failed`.

Full crate:
Run: `cargo test -p codesmith-extensions --lib`
Expected: `63 passed; 0 failed; 1 ignored` (61 from T2 + 2 new).

- [ ] **Step 5: Commit**

```bash
git add crates/extensions/src/install_source.rs crates/extensions/src/lib.rs
git commit -m "feat(extensions): §F5e T3 IdentityBuilder no-op (prebuilt build-skip)

build(src) -> src as-is; PrebuiltDylibSource.fetch returns the downloaded dylib FILE as art.path; tui injects IdentityBuilder for prebuilt (CargoBuilder for git/path/crate). Installer stays kind-agnostic (R4 invariant preserved). Mirrors the §F5c FakeBuilder pattern (installer.rs:173) — IdentityBuilder IS the trivial real-case analogue. build()'s src_dir param receives a file (not dir) for prebuilt — documented.

ext 61→63 (+2 identity_builder_tests). Design: spec §3 Q3."
```

---

## Task 4: `CratesIoSource` (+ `sha2` dep)

**Files:**
- Modify: `crates/extensions/Cargo.toml` (add `sha2.workspace = true` under `[dependencies]`)
- Modify: `crates/extensions/src/install_source.rs` (add imports + `CratesIoSource` + `IndexEntry` + new test module)
- Modify: `crates/extensions/src/lib.rs` (re-export `CratesIoSource`)

- [ ] **Step 1: Add the `sha2` dependency**

In `crates/extensions/Cargo.toml`, under `[dependencies]`, add (alphabetical position after `serde_json.workspace = true`):

```toml
sha2.workspace = true
```

- [ ] **Step 2: Write the failing tests**

Append a new test module at the end of `crates/extensions/src/install_source.rs`:

```rust
#[cfg(test)]
mod crates_io_source_tests {
    use super::*;
    use std::sync::Arc;

    /// Build a canned `.crate` (gzipped tar) in a temp dir: creates
    /// `<name>-<vers>/Cargo.toml` then `tar -czf`. Returns `(bytes, sha256_hex)`.
    /// Returns `None` (skips) if `tar` not on PATH.
    fn make_crate_fixture(name: &str, vers: &str) -> Option<(Vec<u8>, String)> {
        if std::process::Command::new("tar").arg("--version").output().is_err() {
            eprintln!("tar not on PATH; skipping");
            return None;
        }
        let work = tempfile::tempdir().ok()?;
        let inner = work.path().join(format!("{name}-{vers}"));
        std::fs::create_dir_all(&inner).ok()?;
        std::fs::write(
            inner.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"{vers}\"\nedition = \"2021\"\n"),
        )
        .ok()?;
        let crate_file = work.path().join(format!("{name}-{vers}.crate"));
        let out = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&crate_file)
            .arg("-C")
            .arg(work.path())
            .arg(format!("{name}-{vers}"))
            .output()
            .ok()?;
        assert!(out.status.success(), "tar: {}", String::from_utf8_lossy(&out.stderr));
        let bytes = std::fs::read(&crate_file).ok()?;
        let mut h = sha2::Sha256::new();
        sha2::Digest::update(&mut h, &bytes);
        let cksum = format!("{:x}", h.finalize());
        Some((bytes, cksum))
    }

    /// Throwaway index URL for a name (constructs a source with a no-op
    /// FakeHttp just to read `index_url_for_test`). Avoids repeating the
    /// `Arc::new(FakeHttpFetcher::new())` boilerplate in every test.
    fn idx_url_for(name: &str) -> String {
        CratesIoSource::new(name.to_string(), None, Arc::new(FakeHttpFetcher::new()))
            .index_url_for_test()
    }

    #[test]
    fn crates_io_index_url_path_lengths() {
        // crates.io sparse-index layout (verified 2026-07-27): 1→1/, 2→2/c1/,
        // 3→3/c1/, 4+→c1c2/c3c4/. Crate names are ASCII → byte-slice is safe.
        assert_eq!(idx_url_for("a"), "https://index.crates.io/1/a");
        assert_eq!(idx_url_for("ab"), "https://index.crates.io/2/a/ab");
        assert_eq!(idx_url_for("abc"), "https://index.crates.io/3/a/abc");
        assert_eq!(idx_url_for("abcd"), "https://index.crates.io/ab/cd/abcd");
        assert_eq!(idx_url_for("serde"), "https://index.crates.io/se/rd/serde");
        assert_eq!(idx_url_for("abcde"), "https://index.crates.io/ab/cd/abcde");
    }

    #[test]
    fn crates_io_fetch_extracts_and_verifies_checksum() {
        let name = "fixcrate";
        let vers = "0.2.0";
        let Some((bytes, cksum)) = make_crate_fixture(name, vers) else {
            return;
        };
        let idx_url = idx_url_for(name);
        // `cksum` is mandatory on IndexEntry → bake the REAL fixture cksum in.
        let idx_body = format!(
            "{{\"name\":\"{name}\",\"vers\":\"{vers}\",\"yanked\":false,\"cksum\":\"{cksum}\",\"pubtime\":\"2024-02-01T00:00:00.000Z\"}}\n"
        );
        let crate_url = format!("https://static.crates.io/crates/{name}/{name}-{vers}.crate");
        let http = FakeHttpFetcher::new()
            .with(idx_url.as_str(), idx_body.into_bytes())
            .with(crate_url.as_str(), bytes);
        let src = CratesIoSource::new(name.to_string(), None, Arc::new(http));
        let dest = tempfile::tempdir().unwrap();
        let art = src.fetch(dest.path()).expect("fetch");
        assert_eq!(art.path, dest.path().join(format!("{name}-{vers}")));
        assert!(art.path.join("Cargo.toml").is_file());
        assert_eq!(art.provenance, format!("crate:{name}@{vers}"));
    }

    #[test]
    fn crates_io_fetch_selects_exact_version() {
        let name = "fixcrate2";
        let Some((bytes010, cksum010)) = make_crate_fixture(name, "0.1.0") else {
            return;
        };
        let Some((bytes020, cksum020)) = make_crate_fixture(name, "0.2.0") else {
            return;
        };
        let idx_url = idx_url_for(name);
        // both non-yanked; sorted by pubtime asc → latest = 0.2.0. Request
        // exact 0.1.0 → must NOT pick the latest 0.2.0.
        let idx_body = format!(
            "{{\"name\":\"{name}\",\"vers\":\"0.1.0\",\"yanked\":false,\"cksum\":\"{cksum010}\",\"pubtime\":\"2024-01-01T00:00:00.000Z\"}}\n\
             {{\"name\":\"{name}\",\"vers\":\"0.2.0\",\"yanked\":false,\"cksum\":\"{cksum020}\",\"pubtime\":\"2024-02-01T00:00:00.000Z\"}}\n"
        );
        let crate_url010 = format!("https://static.crates.io/crates/{name}/{name}-0.1.0.crate");
        let http = FakeHttpFetcher::new()
            .with(idx_url.as_str(), idx_body.into_bytes())
            .with(crate_url010.as_str(), bytes010);
        let src = CratesIoSource::new(name.to_string(), Some("0.1.0".into()), Arc::new(http));
        let dest = tempfile::tempdir().unwrap();
        let art = src.fetch(dest.path()).expect("fetch selects exact 0.1.0");
        assert_eq!(art.provenance, format!("crate:{name}@0.1.0"));
    }

    #[test]
    fn crates_io_fetch_latest_skips_yanked() {
        let name = "fixcrate3";
        let Some((bytes010, cksum010)) = make_crate_fixture(name, "0.1.0") else {
            return;
        };
        // 0.2.0 yanked, 0.1.0 non-yanked → latest-non-yanked = 0.1.0. The
        // yanked 0.2.0's cksum is bogus but never fetched (skipped before
        // download), so its value is irrelevant.
        let idx_url = idx_url_for(name);
        let idx_body = format!(
            "{{\"name\":\"{name}\",\"vers\":\"0.1.0\",\"yanked\":false,\"cksum\":\"{cksum010}\",\"pubtime\":\"2024-01-01T00:00:00.000Z\"}}\n\
             {{\"name\":\"{name}\",\"vers\":\"0.2.0\",\"yanked\":true,\"cksum\":\"{}\",\"pubtime\":\"2024-02-01T00:00:00.000Z\"}}\n",
            "0".repeat(64)
        );
        let crate_url010 = format!("https://static.crates.io/crates/{name}/{name}-0.1.0.crate");
        let http = FakeHttpFetcher::new()
            .with(idx_url.as_str(), idx_body.into_bytes())
            .with(crate_url010.as_str(), bytes010);
        let src = CratesIoSource::new(name.to_string(), None, Arc::new(http));
        let dest = tempfile::tempdir().unwrap();
        let art = src.fetch(dest.path()).expect("fetch picks 0.1.0 (non-yanked)");
        assert!(art.provenance.ends_with("@0.1.0"));
    }

    #[test]
    fn crates_io_fetch_checksum_mismatch_is_install_error() {
        let name = "fixcrate4";
        let Some((bytes, _real_cksum)) = make_crate_fixture(name, "0.1.0") else {
            return;
        };
        let idx_url = idx_url_for(name);
        // index claims a WRONG cksum ("00..00") → sha256 verify fails.
        let idx_body = format!(
            "{{\"name\":\"{name}\",\"vers\":\"0.1.0\",\"yanked\":false,\"cksum\":\"{}\",\"pubtime\":\"2024-01-01T00:00:00.000Z\"}}\n",
            "0".repeat(64)
        );
        let crate_url = format!("https://static.crates.io/crates/{name}/{name}-0.1.0.crate");
        let http = FakeHttpFetcher::new()
            .with(idx_url.as_str(), idx_body.into_bytes())
            .with(crate_url.as_str(), bytes);
        let src = CratesIoSource::new(name.to_string(), None, Arc::new(http));
        let dest = tempfile::tempdir().unwrap();
        let r = src.fetch(dest.path());
        assert!(matches!(r, Err(ExtensionError::Install(_))), "{r:?}");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p codesmith-extensions --lib crates_io_source_tests`
Expected: COMPILE ERROR — `cannot find type \`CratesIoSource\`` / `index_url_for_test` / `sha2` crate not found (if dep not yet added).

- [ ] **Step 4: Write minimal implementation**

Add the imports at the top of `crates/extensions/src/install_source.rs` (alongside the existing `use std::path::{Path, PathBuf}; use std::process::Command;`):

```rust
use std::sync::Arc;

use sha2::{Digest, Sha256};
```

Add the `CratesIoSource` + `IndexEntry` after the `IdentityBuilder` impl (before `#[cfg(test)] mod source_spec_tests`):

```rust
/// Crates.io install source (§F5e). Sparse-index lookup → version selection →
/// `.crate` download → sha256 verify (registry `cksum`) → `tar -xzf` extract.
/// Hands the extracted inner dir `<name>-<vers>/` to `CargoBuilder` (§F5c
/// build path). Holds an `Arc<dyn HttpFetcher>` (real = `CurlHttpFetcher`,
/// tests = `FakeHttpFetcher`).
pub struct CratesIoSource {
    pub name: String,
    pub version: Option<String>,
    pub http: Arc<dyn HttpFetcher>,
}

impl CratesIoSource {
    pub fn new(
        name: impl Into<String>,
        version: Option<String>,
        http: Arc<dyn HttpFetcher>,
    ) -> Self {
        Self {
            name: name.into(),
            version,
            http,
        }
    }

    /// Sparse-index URL for `self.name` (verified 2026-07-27). 1-3 char names
    /// use special paths; 4+ chars use `<first2>/<next2>/<name>`. Crate names
    /// are ASCII (crates.io policy) so byte-slicing `[..2]`/`[2..4]` is safe.
    fn index_url(&self) -> String {
        let n = self.name.as_str();
        let path = match n.len() {
            1 => format!("1/{n}"),
            2 => format!("2/{}/{}", &n[..1], n),
            3 => format!("3/{}/{}", &n[..1], n),
            // 4+ chars → first2/next2/name (crates.io sparse-index layout;
            // verified 2026-07-27 via curl on "serde" → se/rd/serde). ASCII
            // names → byte-slice `[..2]`/`[2..4]` is safe for len ≥ 4.
            _ => format!("{}/{}/{}", &n[..2], &n[2..4], n),
        };
        format!("https://index.crates.io/{path}")
    }

    /// Test accessor for the index URL (so tests can register the canned
    /// response keyed by the exact URL). `#[cfg(test)]` only.
    #[cfg(test)]
    pub fn index_url_for_test(&self) -> String {
        self.index_url()
    }
}

impl ExtensionSource for CratesIoSource {
    fn fetch(&self, dest: &Path) -> Result<SourceArtifact, ExtensionError> {
        // 1. sparse-index lookup
        let index_json = self.http.fetch_text(&self.index_url())?;
        // 2. parse JSON-lines → entries (serde ignores unknown fields)
        let mut entries: Vec<IndexEntry> = index_json
            .lines()
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str::<IndexEntry>)
            .collect::<Result<_, _>>()
            .map_err(|e| ExtensionError::Install(format!("parse index for {}: {e}", self.name)))?;
        // index is publish-ordered; sort by pubtime asc for deterministic tie-break
        entries.sort_by(|a, b| a.pubtime.cmp(&b.pubtime));
        // 3. select version
        let entry = match &self.version {
            Some(v) => entries
                .iter()
                .find(|e| e.vers == *v && !e.yanked)
                .ok_or_else(|| {
                    ExtensionError::Install(format!("version {v} of {} not found or yanked", self.name))
                })?,
            None => entries
                .iter()
                .rev()
                .find(|e| !e.yanked)
                .ok_or_else(|| {
                    ExtensionError::Install(format!("no non-yanked version for {}", self.name))
                })?,
        }
        .clone();
        // 4. download .crate
        let crate_file =
            dest.join(format!("{}-{}.crate", self.name, entry.vers));
        let crate_url = format!(
            "https://static.crates.io/crates/{}/{}/{}.crate",
            self.name, self.name, entry.vers
        );
        self.http.fetch_to(&crate_url, &crate_file)?;
        // 5. sha256 verify (registry cksum is mandatory; free integrity)
        let bytes = std::fs::read(&crate_file)
            .map_err(|e| ExtensionError::Install(format!("read crate {}: {e}", crate_file.display())))?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = format!("{:x}", hasher.finalize());
        if actual != entry.cksum {
            return Err(ExtensionError::Install(format!(
                "checksum mismatch for {}-{}: expected {}, got {}",
                self.name, entry.vers, entry.cksum, actual
            )));
        }
        // 6. tar -xzf extract (shell-out, like curl/git/cargo)
        let out = Command::new("tar")
            .arg("-xzf")
            .arg(&crate_file)
            .arg("-C")
            .arg(dest)
            .output()
            .map_err(|e| ExtensionError::Install(format!("spawn tar (on PATH?): {e}")))?;
        if !out.status.success() {
            return Err(ExtensionError::Install(format!(
                "tar extract {} failed: {}",
                crate_file.display(),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        // 7. inner dir <name>-<vers>/ (contains Cargo.toml for CargoBuilder)
        let inner = dest.join(format!("{}-{}", self.name, entry.vers));
        if !inner.is_dir() {
            return Err(ExtensionError::Install(format!(
                "extracted inner dir missing: {}",
                inner.display()
            )));
        }
        Ok(SourceArtifact {
            path: inner,
            provenance: format!("crate:{}@{}", self.name, entry.vers),
        })
    }
}

/// One published version row from the crates.io sparse index (§F5e). serde
/// ignores unknown fields (name/deps/features/links). `pubtime` drives the
/// deterministic latest-non-yanked tie-break.
#[derive(serde::Deserialize, Clone)]
struct IndexEntry {
    vers: String,
    cksum: String,
    yanked: bool,
    #[serde(default)]
    pubtime: String,
}
```

Add `CratesIoSource` to the `pub use install_source::{...}` re-export in `crates/extensions/src/lib.rs`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p codesmith-extensions --lib crates_io_source_tests`
Expected: `5 passed; 0 failed` (skips if `tar` not on PATH).

Full crate:
Run: `cargo test -p codesmith-extensions --lib`
Expected: `68 passed; 0 failed; 1 ignored` (63 from T3 + 5 crates_io tests).

- [ ] **Step 6: Commit**

```bash
git add crates/extensions/Cargo.toml crates/extensions/src/install_source.rs crates/extensions/src/lib.rs
git commit -m "feat(extensions): §F5e T4 CratesIoSource (sparse-index + sha256 + tar extract)

fetch(dest): curl sparse index → serde_json parse JSON-lines → select version (latest-non-yanked default OR exact @<vers>; pubtime tie-break; yanked skipped) → download .crate from static.crates.io → sha256 verify vs registry cksum (mandatory, free integrity) → tar -xzf extract → return inner <name>-<vers>/ dir for CargoBuilder. provenance = crate:<name>@<vers>. Holds Arc<dyn HttpFetcher> (CurlHttpFetcher real / FakeHttpFetcher test). Sparse-index URL path logic verified by direct curl 2026-07-27 (fields name/vers/cksum/yanked/pubtime). sha2 dep promoted from workspace (root Cargo.toml:59) — zero new external crate.

ext 63→68 (+5 crates_io_source_tests; skip-on-no-tar). Design: spec §3 Q1+sub-choice A + §4."
```

---

## Task 5: `PrebuiltDylibSource`

**Files:**
- Modify: `crates/extensions/src/install_source.rs` (add `PrebuiltDylibSource` + new test module; reuses `sha2` from T4)
- Modify: `crates/extensions/src/lib.rs` (re-export `PrebuiltDylibSource`)

- [ ] **Step 1: Write the failing tests**

Append a new test module at the end of `crates/extensions/src/install_source.rs`:

```rust
#[cfg(test)]
mod prebuilt_source_tests {
    use super::*;
    use std::sync::Arc;

    fn dylib_sha(bytes: &[u8]) -> String {
        let mut h = sha2::Sha256::new();
        sha2::Digest::update(&mut h, bytes);
        format!("{:x}", h.finalize())
    }

    #[test]
    fn prebuilt_refuses_http_url() {
        let src = PrebuiltDylibSource::new(
            "http://x/y.dylib",
            None,
            Arc::new(FakeHttpFetcher::new()),
        );
        let dest = tempfile::tempdir().unwrap();
        let r = src.fetch(dest.path());
        let Err(ExtensionError::Install(m)) = &r else { panic!("{r:?}") };
        assert!(m.contains("HTTPS"), "{m}");
    }

    #[test]
    fn prebuilt_fetch_downloads_and_provenance() {
        let url = "https://x.example/y.dylib";
        let body = b"fake-dylib-bytes".to_vec();
        let http = FakeHttpFetcher::new().with(url, body.clone());
        let src = PrebuiltDylibSource::new(url, None, Arc::new(http));
        let dest = tempfile::tempdir().unwrap();
        let art = src.fetch(dest.path()).expect("fetch");
        assert!(art.path.is_file());
        assert_eq!(std::fs::read(&art.path).unwrap(), body);
        assert_eq!(art.provenance, format!("prebuilt:{url}"));
    }

    #[test]
    fn prebuilt_checksum_supplied_verifies() {
        let url = "https://x.example/y.dylib";
        let body = b"fake-dylib-bytes".to_vec();
        let cksum = dylib_sha(&body);
        let http = FakeHttpFetcher::new().with(url, body);
        let src = PrebuiltDylibSource::new(url, Some(cksum.clone()), Arc::new(http));
        let dest = tempfile::tempdir().unwrap();
        let art = src.fetch(dest.path()).expect("fetch verifies");
        assert!(art.provenance.contains(&format!("@sha256:{}", &cksum[..7])));
    }

    #[test]
    fn prebuilt_checksum_mismatch_fails() {
        let url = "https://x.example/y.dylib";
        let body = b"fake-dylib-bytes".to_vec();
        let wrong = "0".repeat(64);
        let http = FakeHttpFetcher::new().with(url, body);
        let src = PrebuiltDylibSource::new(url, Some(wrong), Arc::new(http));
        let dest = tempfile::tempdir().unwrap();
        let r = src.fetch(dest.path());
        assert!(matches!(r, Err(ExtensionError::Install(_))), "{r:?}");
    }

    #[test]
    fn prebuilt_no_checksum_proceeds() {
        // absent checksum → fetch succeeds (tui warns after; source doesn't error)
        let url = "https://x.example/y.dylib";
        let http = FakeHttpFetcher::new().with(url, b"dylib".to_vec());
        let src = PrebuiltDylibSource::new(url, None, Arc::new(http));
        let dest = tempfile::tempdir().unwrap();
        let art = src.fetch(dest.path()).expect("proceeds without checksum");
        assert!(!art.provenance.contains("sha256"));
    }

    #[test]
    fn prebuilt_uses_url_basename_filename() {
        let url = "https://x.example/sub/path/myext.dylib";
        let http = FakeHttpFetcher::new().with(url, b"x".to_vec());
        let src = PrebuiltDylibSource::new(url, None, Arc::new(http));
        let dest = tempfile::tempdir().unwrap();
        let art = src.fetch(dest.path()).unwrap();
        assert!(art.path.ends_with("myext.dylib"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codesmith-extensions --lib prebuilt_source_tests`
Expected: COMPILE ERROR — `cannot find type \`PrebuiltDylibSource\``.

- [ ] **Step 3: Write minimal implementation**

In `crates/extensions/src/install_source.rs`, add after the `CratesIoSource` impl (before `#[cfg(test)] mod source_spec_tests`):

```rust
/// Prebuilt-cdylib install source (§F5e). HTTPS-only URL fetch → optional
/// sha256 checksum verify → return the dylib FILE as `art.path` (handed to
/// `IdentityBuilder`, which skips the build step). Trust model = §F5c-
/// consistent (install trust-agnostic, warn-only; gate at discovery);
/// HTTPS-only; checksum optional (warn-absent / refuse-mismatch). D8 temp-
/// load runs `codesmith_register_extension` on the downloaded dylib —
/// accepted per §8.1 (same risk profile as git `build.rs`).
pub struct PrebuiltDylibSource {
    pub url: String,
    pub checksum: Option<String>,
    pub http: Arc<dyn HttpFetcher>,
}

impl PrebuiltDylibSource {
    pub fn new(
        url: impl Into<String>,
        checksum: Option<String>,
        http: Arc<dyn HttpFetcher>,
    ) -> Self {
        Self {
            url: url.into(),
            checksum,
            http,
        }
    }
}

impl ExtensionSource for PrebuiltDylibSource {
    fn fetch(&self, dest: &Path) -> Result<SourceArtifact, ExtensionError> {
        // 1. HTTPS-only (refuse http://)
        if !self.url.starts_with("https://") {
            return Err(ExtensionError::Install(format!(
                "prebuilt source must be HTTPS: {}",
                self.url
            )));
        }
        // 2. derive filename from URL basename (fallback dylib.<DLL_EXT>)
        let filename = self
            .url
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("dylib.{}", std::env::consts::DLL_EXTENSION));
        let dest_file = dest.join(&filename);
        self.http.fetch_to(&self.url, &dest_file)?;
        // 3. optional checksum verify (warn-absent is tui's job; source errors
        //    only on supplied+mismatch)
        if let Some(expected) = &self.checksum {
            let bytes = std::fs::read(&dest_file)
                .map_err(|e| ExtensionError::Install(format!("read dylib: {e}")))?;
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let actual = format!("{:x}", hasher.finalize());
            if &actual != expected {
                return Err(ExtensionError::Install(format!(
                    "checksum mismatch for {}: expected {}, got {}",
                    self.url, expected, actual
                )));
            }
        }
        // 4. provenance: prebuilt:<url> (+ @sha256:<7hex> if checksum)
        let provenance = match &self.checksum {
            Some(c) => format!("prebuilt:{}@sha256:{}", self.url, &c[..7]),
            None => format!("prebuilt:{}", self.url),
        };
        Ok(SourceArtifact {
            path: dest_file,
            provenance,
        })
    }
}
```

Add `PrebuiltDylibSource` to the `pub use install_source::{...}` re-export in `crates/extensions/src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-extensions --lib prebuilt_source_tests`
Expected: `6 passed; 0 failed`.

Full crate:
Run: `cargo test -p codesmith-extensions --lib`
Expected: `74 passed; 0 failed; 1 ignored` (68 from T4 + 6 prebuilt tests).

- [ ] **Step 5: Commit**

```bash
git add crates/extensions/src/install_source.rs crates/extensions/src/lib.rs
git commit -m "feat(extensions): §F5e T5 PrebuiltDylibSource (HTTPS-only + optional sha256)

fetch(dest): HTTPS-only check (refuse http:// → Install) → derive filename from URL basename (fallback dylib.<DLL_EXT>) → curl download via HttpFetcher → optional sha256 verify (supplied+mismatch → Install; absent → proceeds, tui warns) → return dylib FILE as art.path (handed to IdentityBuilder, skips build). provenance = prebuilt:<url> (+ @sha256:<7hex> if checksum). Trust = §F5c-consistent (install trust-agnostic, warn-only, gate at discovery); D8 temp-load code-exec accepted per §8.1 (same risk as git build.rs). Reuses sha2 from T4.

ext 68→74 (+6 prebuilt_source_tests). Design: spec §3 Q2 + §5."
```

---

## Task 6: tui `install_precheck` removal + real source construction

**Files:**
- Modify: `crates/tui/src/commands/extension_commands.rs` (`install_precheck` `:241-262`, `install` `:281-333`, tests `:391-415`)

- [ ] **Step 1: Write the failing tests**

In `crates/tui/src/commands/extension_commands.rs`, in the `tests` module (`:365`), rewrite the 2 "not_yet_implemented" tests + add 1 checksum test. Replace the existing `install_precheck_crate_kind_is_not_yet_implemented` (`:391-402`) + `install_precheck_prebuilt_kind_is_not_yet_implemented` (`:404-409`) with:

```rust
    #[test]
    fn install_precheck_crate_kind_proceeds() {
        // §F5e: crate: now proceeds (real CratesIoSource impl) — no longer
        // rejected by precheck.
        assert!(install_precheck("crate:serde").is_none());
        assert!(install_precheck("crate:serde@1.0.204").is_none());
    }

    #[test]
    fn install_precheck_prebuilt_kind_proceeds() {
        // §F5e: prebuilt: now proceeds (real PrebuiltDylibSource impl).
        assert!(install_precheck("prebuilt:https://x/y.dylib").is_none());
        // with --checksum (valid 64-hex) also proceeds
        assert!(install_precheck(
            "prebuilt:https://x/y.dylib --checksum \
             d1bb2d9926b9bd18e51fc8edd663e311ff3b1fb96c9d4689854f8686f7c6c216"
        )
        .is_none());
    }

    #[test]
    fn install_precheck_bad_checksum_is_error() {
        // invalid --checksum (not 64 lowercase hex) → parse error surfaces
        let r = install_precheck("prebuilt:https://x/y.dylib --checksum abc");
        assert!(r.is_some());
        let r = r.unwrap();
        assert!(r.is_error);
        assert!(
            r.message.as_deref().unwrap().contains("invalid --checksum"),
            "got: {:?}",
            r.message
        );
    }
```

Keep `install_precheck_missing_arg_is_usage_error` (`:382-389`) + `install_precheck_git_path_proceeds_none` (`:411-415`) unchanged (they still pass; the Usage message text changes but the test only asserts `contains("Usage")`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codesmith-tui --bin codesmith-tui install_precheck`
Expected: the 2 new `proceeds` tests FAIL — `install_precheck("crate:serde")` returns `Some(error "§F5c-later...")` (current early-return still active).

- [ ] **Step 3: Write minimal implementation**

In `crates/tui/src/commands/extension_commands.rs`, replace the `install_precheck` fn (`:238-262`) with:

```rust
/// Pre-App validation for `/extension install`: parse + arg check (§F5c R4
/// + §F5e). Returns `Some(error)` for bad args / invalid spec; `None` to
/// proceed with the `App`. No `App` access needed → unit-testable. §F5e
/// dropped the §F5c `crate:`/`prebuilt:` "not-yet-implemented" early-return
/// (real `CratesIoSource`/`PrebuiltDylibSource` impls now wired in `install`).
fn install_precheck(arg: &str) -> Option<CommandResult> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Some(CommandResult::error(
            "Usage: /extension install <kind>:<body>[@<ref>] [--global] [--checksum <sha256>]  (kinds: git, path, crate, prebuilt)",
        ));
    }
    if let Err(e) = codesmith_extensions::SourceSpec::parse(arg) {
        return Some(CommandResult::error(format!("Invalid source spec: {e}")));
    }
    None
}
```

Replace the `install` fn (`:281-333`) with:

```rust
fn install(app: &mut App, arg: &str) -> CommandResult {
    if let Some(err) = install_precheck(arg) {
        return err;
    }
    // Precheck passed → spec is valid (one of git/path/crate/prebuilt).
    let spec = codesmith_extensions::SourceSpec::parse(arg).expect("precheck validated");
    let root = extensions_root_for(spec.scope, &app.workspace);
    // §F5e: HttpFetcher (curl shell-out) injected into crate/prebuilt sources.
    let http: std::sync::Arc<dyn codesmith_extensions::HttpFetcher> =
        std::sync::Arc::new(codesmith_extensions::CurlHttpFetcher::new());
    let source: Box<dyn codesmith_extensions::ExtensionSource> = match spec.kind {
        codesmith_extensions::SourceKind::Git => Box::new(
            codesmith_extensions::GitSource::new(spec.body.clone(), spec.ref_.clone()),
        ),
        codesmith_extensions::SourceKind::Path => Box::new(
            codesmith_extensions::LocalPathSource::new(spec.body.clone()),
        ),
        codesmith_extensions::SourceKind::CratesIo => Box::new(
            codesmith_extensions::CratesIoSource::new(
                spec.body.clone(),
                spec.ref_.clone(),
                http.clone(),
            ),
        ),
        codesmith_extensions::SourceKind::Prebuilt => Box::new(
            codesmith_extensions::PrebuiltDylibSource::new(
                spec.body.clone(),
                spec.checksum.clone(),
                http.clone(),
            ),
        ),
    };
    // CargoBuilder needs a temp target-dir whose TempDir guard outlives
    // install() (kept alive to fn end). IdentityBuilder for prebuilt (no build).
    let build_target = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => return CommandResult::error(format!("tempdir for build: {e}")),
    };
    let builder: Box<dyn codesmith_extensions::ExtensionBuilder> = match spec.kind {
        codesmith_extensions::SourceKind::Prebuilt => {
            Box::new(codesmith_extensions::IdentityBuilder)
        }
        _ => Box::new(codesmith_extensions::CargoBuilder::new(
            build_target.path().to_path_buf(),
        )),
    };
    let installer =
        codesmith_extensions::Installer::new(source.as_ref(), builder.as_ref(), root.clone());
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
        codesmith_extensions::InstallScope::Project => {
            crate::config::is_workspace_trusted(&app.workspace)
        }
    };
    let trust_note = if will_load {
        String::new()
    } else {
        "\n⚠ won't load until the workspace is trusted (accept the trust prompt or /trust, then /extension reload)."
            .to_string()
    };
    // §F5e: prebuilt checksum-absent warn (integrity unverified).
    let checksum_note = match spec.kind {
        codesmith_extensions::SourceKind::Prebuilt if spec.checksum.is_none() => {
            "\n⚠ no checksum supplied; dylib integrity unverified (pass --checksum <sha256> to verify)."
        }
        _ => "",
    };
    CommandResult::message(format!(
        "Installed extension '{}' (v{}) to {}.\nprovenance: {}\nRun /extension reload to load it.{}{}",
        report.id,
        report.version,
        report.path.display(),
        report.provenance,
        trust_note,
        checksum_note,
    ))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codesmith-tui --bin codesmith-tui install_precheck`
Expected: all install_precheck tests pass (the 2 rewritten `proceeds` + the new `bad_checksum` + the unchanged `missing_arg` + `git_path_proceeds_none`).

Full tui suite (report honestly — 26 pre-existing `runtime_api` env-fails):
Run: `cargo test -p codesmith-tui --bin codesmith-tui 2>&1 | tail -5`
Expected: `2867 passed; 26 failed (pre-existing runtime_api); 2 ignored` (2866 baseline + 1 new `install_precheck_bad_checksum`; the 2 rewrites keep count). The 26 `runtime_api::tests` fails are pre-existing env-fails (HTTP-server won't bind) — NOT §F5e regressions.

Also confirm no extension-crate regression:
Run: `cargo test -p codesmith-extensions --lib 2>&1 | tail -3`
Expected: `74 passed; 0 failed; 1 ignored` (unchanged from T5).

- [ ] **Step 5: Commit**

```bash
git add crates/tui/src/commands/extension_commands.rs
git commit -m "feat(tui): §F5e T6 install_precheck drops crate/prebuilt stub + real source construction

install_precheck: drop the §F5c 'crate:/prebuilt: → §F5c-later not-yet-implemented' early-return (R4 stub); now only validates args+spec parse (incl. --checksum hex). install(): construct real source per spec.kind — GitSource/LocalPathSource (CargoBuilder), CratesIoSource (CargoBuilder + Arc<CurlHttpFetcher>), PrebuiltDylibSource (IdentityBuilder + Arc<CurlHttpFetcher>); the unreachable!('install_precheck rejected crate/prebuilt') is gone. New §F5e prebuilt checksum-absent warn ('integrity unverified; pass --checksum'). §F5c trust-warn unchanged. build_target TempDir guard kept alive to fn end (CargoBuilder path-only; IdentityBuilder for prebuilt skips it).

Tests: 2 install_precheck_*_is_not_yet_implemented → install_precheck_*_kind_proceeds (crate+prebuilt now None); +1 install_precheck_bad_checksum_is_error. tui 2866→2867 (+1); 26 pre-existing runtime_api env-fail unchanged (NOT §F5e). ext 74 unchanged. Design: spec §7."
```

---

## Task 7: Docs — `EXTENSIONS.md` + `ROADMAP.md`

**Files:**
- Modify: `docs/EXTENSIONS.md` (intro `:40-41`, install row `:80`, §F5c section `:298-307`)
- Modify: `ROADMAP.md` (§F5c "By-design gaps" `:2633-2634` + §F5c "next focus" `:2640` + add `### F5e` progress block after the §F5d block ~`:2663`)

- [ ] **Step 1: Update `EXTENSIONS.md`**

At `docs/EXTENSIONS.md:40-41`, the intro currently says:
```
> `installed[]` provenance write) landed in §F5c — `crate:`/`prebuilt:` stub
> to "§F5c-later". §F5d (done) wires extension tools + slash commands live
```
Change to:
```
> `installed[]` provenance write) landed in §F5c. §F5e (done) adds the real
> `crate:`/`prebuilt:` source impls (was §F5c "§F5c-later" stub). §F5d (done)
> wires extension tools + slash commands live
```

At `docs/EXTENSIONS.md:80` (the install row), the current cell ends:
```
`crate:`/`prebuilt:` return "§F5c-later". Warns if project + untrusted; `/extension reload` to load.
```
Change to:
```
`crate:` fetches from crates.io (sparse-index → version → sha256-verified `.crate` → `tar` extract → build); `prebuilt:<https-url>` fetches a prebuilt cdylib (HTTPS-only, optional `--checksum <sha256>`); both warn if project + untrusted; `/extension reload` to load.
```

At `docs/EXTENSIONS.md:306-307`, the §F5c section currently says:
```
for untrusted sources. `crate:`/`prebuilt:` sources stay deferred
(`install_precheck` returns "§F5c-later"). §F5d (done) wires extension tools +
```
Change to:
```
for untrusted sources. `crate:`/`prebuilt:` sources shipped in §F5e (real
`CratesIoSource`/`PrebuiltDylibSource` impls; was §F5c "§F5c-later" stub).
§F5d (done) wires extension tools +
```

- [ ] **Step 2: Update `ROADMAP.md` §F5c gaps + next-focus**

At `ROADMAP.md:2634`, the §F5c "By-design gaps" line:
```
- `CratesIo`/`Prebuilt` source impls（nice-to-have，command `install_precheck` 早返回 "§F5c-later"）。
```
Strikethrough-correct (mirrors the established ROADMAP pattern `~~…~~ → slice N 复核 stale：…`):
```
- ~~`CratesIo`/`Prebuilt` source impls（nice-to-have，command `install_precheck` 早返回 "§F5c-later"）~~ → §F5e 复核 stale：已落地（见下 §F5e 进度块）。
```

At `ROADMAP.md:2640`, the §F5c "next focus" line:
```
- 残项（按需）：P2 doc drift + §E4 follow-up + `CratesIo`/`Prebuilt` impl + 真卸载——均 on-demand / 非阻塞。
```
Update to mark `CratesIo`/`Prebuilt` + 真卸载 done (真卸载 done in §F5d):
```
- 残项（按需）：P2 doc drift + §E4 follow-up——均 on-demand / 非阻塞。（~~`CratesIo`/`Prebuilt` impl~~ → §F5e 已落地；~~真卸载~~ → §F5d 已落地。）
```

- [ ] **Step 3: Add the `### F5e` progress block to `ROADMAP.md`**

After the §F5d progress block (ends ~`:2663`), insert a new `### F5e` subsection mirroring §F5d's format (`:2644-2663`). Use this content (fill `[REAL COUNTS]` from T6's actual `cargo test` output — the executor runs the tests + records the real numbers before commit):

```markdown
**进度（2026-07-27 §F5e CratesIo + Prebuilt INSTALL source impls——§F5c-deferred 残项闭合：`crate:`/`prebuilt:` real source impls + flow through `Installer::install`，`feat/f5e-cratesio-prebuilt`）：**

接 §F5c（INSTALL 侧 stubbed crate/prebuilt）+ §F5d（wiring + 真卸载，留 crate/prebuilt deferred）。§F5e 闭合：`CratesIoSource`（sparse-index → version select → `.crate` download → sha256 verify[registry `cksum`] → `tar -xzf` extract → CargoBuilder）+ `PrebuiltDylibSource`（HTTPS-only → curl download → optional sha256 → IdentityBuilder skip build）+ `IdentityBuilder`（no-op `ExtensionBuilder`）+ `HttpFetcher` trait（`CurlHttpFetcher` curl shell-out / `FakeHttpFetcher` test）+ `SourceSpec`（`checksum` field + `--checksum <hex>` + kind-dependent `@`-split）+ tui `install_precheck` drop "§F5c-later" early-return + `install()` real source+builder per kind + prebuilt checksum-absent warn。curl shell-out（3rd after git/cargo）+ `tar -xzf` + `sha2` workspace dep（zero new external crate）。spec：`docs/superpowers/specs/2026-07-27-codesmith-extension-system-slice-5e-design.md`；plan：`docs/superpowers/plans/2026-07-27-codesmith-extension-system-slice-5e.md`。

**By-design gaps（显式 out-of-scope）：**
- `--features`/`--offline`/`--target` flags（§F5c YAGNI；default `--release --locked` 不变）。
- `--message-format=json` multi-cdylib（§F5c R2 dir-scan 不变；CratesIo single-cdylib 假设）。
- CratesIo alternate registries（private/cargo-registry/git-index）—— `index.crates.io` + `static.crates.io` only。
- Prebuilt signature verification（sigstore/gpg）—— sha256 = integrity only。
- tui-level install e2e（§F5 precedent）。

**设计决策（brainstorm Q1-Q3 + sub-choices A/B）：**
- Q1 HTTP = curl shell-out（§F5c-style 3rd shell-out；zero new external crate；`sha2` workspace dep promoted）。Rejected: reqwest-in-extensions（heavy async-HTTP into pure crate）/ trait-DI-via-tui（wiring asymmetry）。
- Q2 prebuilt trust = §F5c-consistent（trust-agnostic install warn-only，gate at discovery；HTTPS-only；optional checksum warn-absent/refuse-mismatch；CratesIo checksum auto from registry）。Rejected: require-checksum（inconsistent w/ git/crate）/ interactive prompt（couples install+trust）。
- Q3 build-skip = `IdentityBuilder` no-op（trait-DI；Installer stays kind-agnostic per R4）。Rejected: separate `install_prebuilt()` / Installer detects kind（couple Installer to SourceKind）。
- Sub-choice A: CratesIo version = latest-non-yanked default + `@<version>` exact（provenance records resolved vers）。Rejected: require `@<version>` always（over-strict）。
- Sub-choice B: checksum syntax = `--checksum <hex>` flag + `SourceSpec.checksum` field。Rejected: inline `@sha256:<hex>`（overloads `@`）。

**测试/验证：** `cargo build --workspace` 全绿；`codesmith-extensions --lib` 51→[REAL EXT COUNT]（+T1 2 http_fetcher +T2 8 source_spec +T3 2 identity +T4 5 crates_io +T5 6 prebuilt；1 `#[ignore]` network curl）；`codesmith-agent --lib` 98（不变）；`codesmith-agent-runtime --lib` 1165+2（不变；flaky `streamable_http_stale_session...` 隔离重跑绿）；`codesmith-tui --bin codesmith-tui` 2866→[REAL TUI COUNT] pass/26 pre-existing `runtime_api` env-fail/2 ignored（+1 `install_precheck_bad_checksum`；2 rewrite 不增）。grep：`CratesIoSource`/`PrebuiltDylibSource`/`IdentityBuilder`/`HttpFetcher`/`CurlHttpFetcher` in extensions ≥1 each、`sha2` in `crates/extensions/Cargo.toml` ≥1、`--checksum` in `SourceSpec::parse` ≥1、`install_precheck` 无 "§F5c-later"、`extension_commands.rs` 无 `unreachable!("install_precheck rejected crate/prebuilt")`；§F5c/§F5d 不变项（`GitSource`/`LocalPathSource`/`CargoBuilder`/`Placer`/`Installer` ≥1、`libloading`/`toml` in extensions `Cargo.toml`、`loader.rs`/`manifest.rs`/`build.rs` 存在、`discover_dylib` in `engine.rs` ≥1、`codesmith_register_extension` in fixture ≥1、`clear_tools`/`clear_commands`/`drain_libraries_to_pending`/`drop_pending` in `runner.rs` ≥1、`host_executor .emit`=16、`TrustReason::FirstLoad` in tui=1）。
```

- [ ] **Step 4: Verify + record real counts**

Run the full verification suite (report honestly):
```bash
cargo build --workspace 2>&1 | tail -3
cargo test -p codesmith-extensions --lib 2>&1 | tail -3
cargo test -p codesmith-agent --lib 2>&1 | tail -3
cargo test -p codesmith-agent-runtime --lib 2>&1 | tail -3
cargo test -p codesmith-tui --bin codesmith-tui 2>&1 | tail -5
```
Expected: build green; ext `[REAL EXT COUNT] passed; 0 failed; 1 ignored`; agent `98 passed`; agent-runtime `1165 passed; 2 ignored`; tui `[REAL TUI COUNT] passed; 26 failed (pre-existing runtime_api); 2 ignored`.

Record the real ext + tui counts, then edit the ROADMAP §F5e block to replace `[REAL EXT COUNT]` + `[REAL TUI COUNT]` with the actual numbers.

- [ ] **Step 5: Commit**

```bash
git add docs/EXTENSIONS.md ROADMAP.md
git commit -m "docs: §F5e T7 EXTENSIONS.md + ROADMAP.md (crate:/prebuilt: now working; §F5e progress block)

EXTENSIONS.md: intro + install row + §F5c section — drop '§F5c-later' for crate/prebuilt; document crate: (sparse-index+sha256+tar+build) + prebuilt: (HTTPS-only + optional --checksum). ROADMAP.md: §F5c 'By-design gaps' strikethrough-correct CratesIo/Prebuilt → done (§F5e); §F5c 'next focus' mark CratesIo/Prebuilt + 真卸载 done; add ### F5e progress block (decisions Q1-Q3+A/B, by-design gaps, test deltas ext 51→[REAL]/tui 2866→[REAL], grep gates). docs-only — ext/agent/agent-runtime/tui counts unchanged by T7.

Real counts (from T7 step 4): ext [REAL EXT] / agent 98 / agent-runtime 1165+2 ignored / tui [REAL TUI] pass/26 pre-existing runtime_api fail/2 ignored. Design: spec §8 + §9."
```

---

## Self-Review (run after writing all tasks)

**1. Spec coverage:** Every spec section maps to a task:
- §3 Q1 (curl) → T1; Q2 (trust) → T6 warns; Q3 (IdentityBuilder) → T3; sub-choice A → T4; sub-choice B → T2. ✓
- §4 CratesIoSource → T4; §5 PrebuiltDylibSource → T5; §6 SourceSpec → T2; §7 tui wiring → T6; §8 testing → each task's tests; §9 file map → File Structure table; §10 verification gate → T7 step 4; §11 honest-test → T6 step 4 note. ✓
- §1 in-scope items 1-7 → T1/T2/T3/T4/T5/T6/T7. ✓

**2. Placeholder scan:** The `[REAL EXT COUNT]` / `[REAL TUI COUNT]` in T7 are intentional fill-at-execution tokens (T7 step 4 records them) — the executor replaces them with real numbers before the T7 commit. This is NOT a plan placeholder (the step that fills them is explicit). All code blocks contain real Rust. ✓

**3. Type consistency:** `HttpFetcher` (T1) → held as `Arc<dyn HttpFetcher>` in `CratesIoSource` (T4) + `PrebuiltDylibSource` (T5) + injected in `install()` (T6). `IdentityBuilder` (T3) → used in `install()` prebuilt arm (T6). `SourceSpec.checksum` (T2) → read in `install()` (T6). `CratesIoSource::index_url_for_test` (T4) → used in T4 tests. `sha2::Sha256`/`Digest::update`/`format!("{:x}", h.finalize())` consistent across T4 impl + T4 test + T5. ✓

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-27-codesmith-extension-system-slice-5e.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration. Best for this plan: T1/T2/T3 are independent (parallelizable); T4/T5 after T1; T6 after all; T7 last.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints for review.

Which approach?
