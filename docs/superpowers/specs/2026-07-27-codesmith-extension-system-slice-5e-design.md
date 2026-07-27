# §F5e — CratesIo + Prebuilt INSTALL source impls Design

- **Date:** 2026-07-27
- **Branch:** `feat/f5e-cratesio-prebuilt` (created from `main` HEAD `2d66279b` before any code)
- **Predecessor:** §F5d (extension tool/command host wiring + true unload), merged to `main` HEAD `2d66279b` (ff-merge; §F5d spec tracked at `docs/superpowers/specs/2026-07-24-codesmith-extension-system-slice-5d-design.md`).
- **Spec:** this file (APPROVED 2026-07-27 — see STATE banner below)
- **Plan (to be written):** `docs/superpowers/plans/2026-07-27-codesmith-extension-system-slice-5e.md`
- **Authoritative scope source:** this session's code verification on `main` HEAD `2d66279b` + §F5c spec `docs/superpowers/specs/2026-07-23-codesmith-extension-system-slice-5c-design.md` §3 "Out of scope" + Q7 (deferred `CratesIoSource`/`PrebuiltDylibSource` as `UnimplementedSource`-style stub, "各 ~1 task + 新 dep") + `docs/EXTENSIONS.md` + `ROADMAP.md` §F5c "By-design gaps".

> **STATE — approved 2026-07-27.** Design re-presented (brainstorming step 5) + user-approved with two sub-choices locked: **(A) CratesIo version selection = default latest-non-yanked + `@<version>` exact pin** (provenance records resolved version); **(B) checksum syntax = `--checksum <hex>` flag + `SourceSpec.checksum` field** (matches `--global` precedent; URLs with `@` userinfo don't collide). §2 code findings verified against `main` HEAD `2d66279b`. Three foundational decisions locked via Q&A: **(Q1) HTTP = curl shell-out** (§F5c-style, zero new runtime crate dep; `sha2` workspace dep added to extensions Cargo.toml); **(Q2) prebuilt trust = §F5c-consistent** (trust-agnostic install, HTTPS-only, optional checksum); **(Q3) build-skip = `IdentityBuilder` no-op** (trait-DI, Installer stays kind-agnostic). Crates.io sparse-index shape verified by direct curl (fields: `name`/`vers`/`cksum`/`yanked`/`pubtime`/`deps`/`features`). This draft is now the **spec** (self-reviewed inline; pending the user's spec-file review). Next: user reviews → `writing-plans` → TDD execute. **No plan, no code exists yet.** Per §F5d precedent, this spec is **tracked** (committed on the feature branch).

---

## 0. Origin — closing the §F5c-deferred "nice-to-have" stubs

§F5c (slice 5c) shipped the INSTALL half of the dylib machine (fetch → build → place → manifest → `installed[]` provenance) for `git:`/`path:` sources, but explicitly **deferred** `CratesIoSource`/`PrebuiltDylibSource` (§3 "Out of scope" + Q7: "ROADMAP 标 nice-to-have；CratesIo 需 registry HTTP+version+checksum，Prebuilt 需 HTTP fetch+sha256 verify，各 ~1 task + 新 dep"). The deferral was implemented as a **tui-layer `install_precheck` early-return** (R4 reconciliation — NOT the spec §4 diagram's `UnimplementedSource`-in-`install_source.rs` approach): `crates/tui/src/commands/extension_commands.rs:241` `install_precheck` returns `Some(error)` "§F5c-later: ... not yet implemented" for `crate:`/`prebuilt:` kinds; called at `:282` (install entry); `:295` `unreachable!("install_precheck rejected crate/prebuilt")` proves these kinds never reach `Installer::install`.

§F5d (slice 5d) shipped the host wiring + true-unload half and explicitly left `crate:`/`prebuilt:` source impls as "remain §F5c deferred" (§F5d §1 "Out of scope"). **This slice (§F5e) closes that deferral:** real `CratesIoSource` + `PrebuiltDylibSource` impls + flow through `Installer::install` end-to-end.

The deferral's stated blocker ("新 dep") is now **moot**: `sha2 = "0.10"` is already a workspace dep (root `Cargo.toml:59`); `reqwest 0.13.1` is also workspace (root `:46`) but §F5e chose curl shell-out instead (§3 Q1) — so §F5e adds **zero new external runtime crate dep** (only promotes `sha2` from a workspace-declared dep into `codesmith-extensions/Cargo.toml`).

## 1. Goal & scope

**In scope:**
1. `CratesIoSource` (real `ExtensionSource` impl): sparse-index lookup → version selection → `.crate` download → sha256 verify (registry `cksum`) → `tar -xzf` extract → hand to `CargoBuilder` (reuse §F5c build path).
2. `PrebuiltDylibSource` (real `ExtensionSource` impl): HTTPS-only URL fetch → optional sha256 checksum verify → return dylib file → hand to `IdentityBuilder` (skip build).
3. `IdentityBuilder` (no-op `ExtensionBuilder` impl): `build(src)` returns `src` as-is (prebuilt path).
4. `HttpFetcher` trait + `CurlHttpFetcher` real impl (curl shell-out) + `FakeHttpFetcher` (test-only) — trait-DI for no-network unit tests.
5. `SourceSpec` parser: add `checksum: Option<String>` field + `--checksum <hex>` flag; kind-dependent `@`-split (git/crate split; path/prebuilt don't).
6. tui `install_precheck`: drop the `crate:`/`prebuilt:` early-return; wire real source+builder construction into `/extension install`.
7. Tests: `CratesIoSource`/`PrebuiltDylibSource`/`IdentityBuilder`/`CurlHttpFetcher` unit (via `FakeHttpFetcher`) + `install_precheck` test deltas.

**Out of scope (remain deferred):**
- `--features`/`--offline`/`--target` flags for `cargo build` (§F5c YAGNI; default `--release --locked` unchanged).
- `--message-format=json` multi-cdylib support (§F5c R2 dir-scan stays; CratesIo single-cdylib assumption).
- CratesIo alternate registries (private / cargo-registry / git-index) — `index.crates.io` + `static.crates.io` only.
- Prebuilt signature verification (sigstore / gpg) beyond sha256 — out of scope; sha256 = integrity only (matches CratesIo).
- tui-level install e2e (§F5 precedent; `EngineHost` + `run_tui` + real-trust fixture disproportionate).
- `abi_stable` (§2.4, never).
- Committing §F5a/5b/5c untracked specs (separate spec-hygiene task; not mixed into this feature slice).

## 2. Code-verified findings (VERIFIED on `main` HEAD `2d66279b` — all line numbers exact)

- **`SourceKind`** (`crates/extensions/src/install_source.rs:58-63`): `Git`/`Path`/`CratesIo`/`Prebuilt` variants; `CratesIo`/`Prebuilt` carry "§F5c stubbed (nice-to-have)" comments (`:61-62`). **No `CratesIoSource`/`PrebuiltDylibSource` structs exist yet** — this slice adds them.
- **`SourceSpec::parse`** (`:80-126`): handles `crate:`/`prebuilt:` prefixes (`:104-105`); **always `rsplit_once('@')` on `rest`** (`:112`) — breaks prebuilt URLs with `@` userinfo (this slice makes `@`-split kind-dependent). `--global` detected as single-token flag (`:81-85`); spec_token = first non-`--` token (`:86-93`).
- **`UnimplementedSource`** (`:39-46`): placeholder, returns `ExtensionError::Install("install requires the dylib loader (§F5)")`. **Not used** for crate/prebuilt in practice (R4 tui-layer stub instead) — kept for any future placeholder need; §F5e does NOT remove it (harmless, still referenced by §F1's `/extension install` historical contract).
- **Real impls (mirrorable)**: `GitSource` (`:128-174`, `git clone --depth 1 [--branch <ref>]`, stderr→Install on fail), `LocalPathSource` (`:176-202`, recursive copy), `CargoBuilder` (`:230-285`, R2 dir-scan NOT JSON-parse — comment `:225-229`: "No JSON parse (R2: avoids a serde_json dep). Robust for single-cdylib crates; errors on 0 or >1 cdylib"), `Placer` (`:287-321`, `default_dylib_filename(id)` rename).
- **`Installer::install`** (`crates/extensions/src/installer.rs:60-96`): `fetch → build → D8 temp-load metadata → Placer → write manifest`. R4 comment (`:61-62`): "§F5c short-circuits crate/prebuilt in the tui command before constructing this — R4". `manifest_kind` (`:118-125`) already maps `CratesIo=>"crate"`/`Prebuilt=>"prebuilt"` — manifest write ready, no change.
- **e2e pattern** (`installer.rs:127-256`): `FakeSource` (`:162-172`) + `FakeBuilder` (`:173-180`, returns fixed dylib path = `CODESMITH_FIXTURE_DYLIB`) → real Placer + real manifest → discover → load → `fixture_echo` bound. This is the **trait-DI pattern §F5e mirrors** for `HttpFetcher`.
- **tui stub** (`crates/tui/src/commands/extension_commands.rs`): `install_precheck(arg) -> Option<CommandResult>` at `:241`; crate/prebuilt early-return at `:257` ("§F5c-later: {:?} source not yet implemented (this slice supports git/path only)"); called at install entry `:282`; `unreachable!("install_precheck rejected crate/prebuilt")` at `:295`. **4 tests**: `install_precheck_missing_arg` (`:383`), `install_precheck_crate_kind_is_not_yet_implemented` (`:392`), `install_precheck_prebuilt_kind_is_not_yet_implemented` (`:405`), `install_precheck_git_path_proceeds_none` (`:412`).
- **Workspace deps** (root `Cargo.toml`): `reqwest 0.13.1` (`:46`, json/rustls/socks), `rig-core 0.39` (`:51`), `sha2 = "0.10"` (`:59`, workspace dep), `tower-http 0.6` (`:60`). **`codesmith-extensions/Cargo.toml` has NO reqwest/sha2** (deps: anyhow/async-trait/futures-util/codesmith-agent/codesmith-tools/inventory/serde/serde_json/tempfile/toml/thiserror/tokio/tokio-util/tracing/libloading). **tui has both** reqwest (`crates/tui/Cargo.toml:66`, blocking+json+rustls+...) + sha2 (`:100`). §F5e adds `sha2` to `crates/extensions/Cargo.toml` (workspace dep, already declared at root) — **zero new external crate**.
- **Crates.io sparse index (verified by direct curl 2026-07-27)**: `https://index.crates.io/<2c>/<2c>/<name>` (1-4 char names use special paths `/1/{c}`, `/2/{c}/{cc}`, `/3/{c}/{ccc}/`, `{c}/{cccc}/`; ≥5 chars → `/{first2}/{next2}/{name}`). Returns JSON-lines (one object per published version). Fields: `name`, `vers`, `cksum` (sha256 hex), `yanked` (bool), `pubtime`, `deps`, `features`, `links` (nullable). Index is append-ordered by publish time → last non-yanked = latest. `.crate` download: `https://static.crates.io/crates/<name>/<name>-<vers>.crate`. Requires a `User-Agent` header (curl `-A`).

## 3. Locked decisions (brainstorm Q1-Q3 + sub-choices A/B)

### Q1 — HTTP = curl shell-out (§F5c-style)
- **Decision**: `curl` via `std::process::Command` (3rd shell-out after `GitSource`'s `git` + `CargoBuilder`'s `cargo`); `tar -xzf` shell-out for `.crate` extraction; `sha2` (workspace dep) for sha256. Zero new runtime crate dep.
- **Rationale**: matches the user's "最小可信、与 CargoBuilder `std::process::Command` 风格一致" hint; consistent with §F5c `GitSource`/`CargoBuilder` precedent (shell-out + parse exit/stderr); `sha2` already workspace. curl TLS is trustworthy (default cert verification; `-fS` for error propagation).
- **Testability**: a `HttpFetcher` trait (`: Send + Sync`, defined in extensions — the bound is required so `Arc<dyn HttpFetcher>` is `Send+Sync` for the tui→Installer handoff) holds the curl shell-out; `CratesIoSource`/`PrebuiltDylibSource` own an `Arc<dyn HttpFetcher>`; tests inject `FakeHttpFetcher` → no-network unit tests (better than §F5c's "test error paths + real binary" approach for git/cargo).
- **Rejected**: reqwest-in-extensions (Rust-native + robust, but a heavy async-HTTP dep into the currently-pure extensions crate); trait-DI-via-tui (clean but adds wiring + real-impl-in-tui asymmetry).

### Q2 — Prebuilt trust = §F5c-consistent + HTTPS-only + optional checksum
- **Decision**: trust-agnostic install (warn-only for untrusted project, like git/path); HTTPS-only (refuse `http://`); optional user-supplied checksum (warn if absent, refuse if supplied+mismatch); §F5b discovery trust gate still gates LOADING. D8 temp-load code-exec accepted per §8.1 (same risk profile as git `build.rs`).
- **Rationale**: §F5c Q5 rejected install+trust coupling ("拒绝 untrusted project install: 耦合 install 与 trust state"); prebuilt = same risk profile as git/crate (user typed the URL = trusts it; arbitrary code runs at install per §8.1). CratesIo checksum is mandatory+free (registry `cksum`); prebuilt has no registry → user checksum optional.
- **Rejected**: require-checksum-for-prebuilt (inconsistent with git/crate; prebuilt isn't categorically higher-risk than git-clone-and-build, since git also runs arbitrary `build.rs`); interactive trust prompt (couples install+trust, breaks §F5c symmetry, slash-command interactive-prompt complexity).

### Q3 — Build-skip = `IdentityBuilder` no-op (trait-DI)
- **Decision**: add `IdentityBuilder` impl `ExtensionBuilder` whose `build(src) -> src` (no-op); `PrebuiltDylibSource.fetch` returns the downloaded dylib FILE as `art.path`; tui injects `IdentityBuilder` for prebuilt kind, `CargoBuilder` for git/path/crate. Installer stays kind-agnostic (R4 invariant preserved).
- **Rationale**: mirrors the existing `FakeBuilder` pattern (`installer.rs:173`); trivially testable; no `Installer` code-path branching. Minor stretch: `build()`'s `src_dir` param receives a file path for prebuilt (documented in the impl).
- **Rejected**: separate `Installer::install_prebuilt()` (couples Installer to kind + 2nd method to maintain); `Installer::install` detects kind (couples Installer to `SourceKind`, violates R4).

### Sub-choice A — CratesIo version selection = latest-default + `@<version>` exact
- **Decision**: `crate:<name>` → latest non-yanked (last non-yanked index entry; `pubtime` tie-break if ordering ambiguous); `crate:<name>@<version>` → exact version (must exist + be non-yanked, else `Install("version <v> yanked or not found")`). Provenance records the resolved version (`crate:<name>@<vers>`).
- **Rationale**: ergonomic (user needn't know version) + reproducible-enough (provenance pins what was installed; checksum guarantees integrity regardless of how the version was selected). `pubtime` available for deterministic tie-break.
- **Rejected**: require `@<version>` always (supply-chain pinning like `--locked`; but `--locked` already pins the build's dep tree + the registry checksum guarantees integrity, so forcing an explicit user version is over-strict).

### Sub-choice B — Checksum syntax = `--checksum <hex>` flag
- **Decision**: `SourceSpec.checksum: Option<String>` field; parsed from `--checksum <sha256-hex>` flag (2-token: skip `--checksum` + consume next token as value, unlike single-token `--global`). Kind-agnostic (git/path/crate ignore it; prebuilt uses it). Hex validation: 64 lowercase hex chars, else `Install("invalid --checksum: expected 64 hex chars")`.
- **Rationale**: matches `--global` flag precedent; URLs with `@` userinfo don't collide (prebuilt body = full URL, no `@`-split). Inline `@sha256:<hex>` would overload `@` (already ref/version for git/crate) + a 64-hex-char token is ambiguous with version strings.
- **Rejected**: inline `@sha256:<hex>` (overloads `@`, collides with version syntax, mangles URLs containing `@`).

## 4. CratesIoSource design

Spec `crate:<name>` | `crate:<name>@<version>` → `CratesIoSource { name: String, version: Option<String>, http: Arc<dyn HttpFetcher> }` (constructed by tui with `name = spec.body`, `version = spec.ref_`).

`fetch(dest: &Path) -> Result<SourceArtifact, ExtensionError>`:
1. Resolve sparse-index path from `name` length: 1 char → `/1/{c}`; 2 chars → `/2/{c}/{cc}`; 3 chars → `/3/{c}/{ccc}`; 4 chars → `/{c}/{cccc}`; ≥5 chars → `/{first2}/{next2}/{name}`. URL = `https://index.crates.io/<path>`.
2. `http.fetch_text(url)` → JSON-lines string. serde_json parse each non-empty line → `IndexEntry { vers, cksum, yanked, pubtime, .. }`.
3. Select version: if `version` (i.e. `spec.ref_`) supplied → find exact match; if that entry is yanked → `Install("version <v> of <name> is yanked")`; if not found → `Install("version <v> not found for <name>")`. Else → last non-yanked entry by index order (`pubtime` tie-break); if all yanked → `Install("no non-yanked version for <name>")`.
4. Download `.crate`: `https://static.crates.io/crates/<name>/<name>-<vers>.crate` → `dest/<name>-<vers>.crate` via `http.fetch_to(url, dest_file)`.
5. sha256 verify: compute `sha2::Sha256` of the downloaded `.crate` file; compare hex digest to `cksum`. Mismatch → `Install("checksum mismatch for <name>-<vers>: expected <cksum>, got <actual>")`. **Mandatory** (registry provides `cksum`).
6. Extract: `tar -xzf <crate-file> -C <dest>` (`std::process::Command`, like curl/git/cargo). A `.crate` is a gzipped tar that extracts to `dest/<name>-<vers>/` (inner dir containing `Cargo.toml`).
7. Return `SourceArtifact { path: dest.join(format!("{name}-{vers}")), provenance: format!("crate:{name}@{vers}") }`.

Then `CargoBuilder.build(inner_dir)` → cdylib → D8 temp-load → Placer → manifest (§F5c path, unchanged). `manifest_kind` writes `type = "crate"`, `ref = "<resolved-vers>"`.

## 5. PrebuiltDylibSource design

Spec `prebuilt:<https-url>` [--checksum <hex>] → `PrebuiltDylibSource { url: String, checksum: Option<String>, http: Arc<dyn HttpFetcher> }` (constructed by tui with `url = spec.body`, `checksum = spec.checksum`).

`fetch(dest: &Path) -> Result<SourceArtifact, ExtensionError>`:
1. HTTPS-only: if `!url.starts_with("https://")` → `Install("prebuilt source must be HTTPS: <url>")`.
2. Download: derive filename from the URL path basename (`url.rsplit('/').next()`); if empty/no-extension → fallback `dylib.<DLL_EXTENSION>`. `http.fetch_to(url, dest.join(filename))`.
3. If `checksum` supplied: sha256 verify downloaded bytes vs `checksum`. Mismatch → `Install("checksum mismatch for <url>: expected <checksum>, got <actual>")`. Absent → (tui warns after; fetch proceeds, no error from the source).
4. Return `SourceArtifact { path: dest.join(filename) /* the dylib FILE */, provenance: format!("prebuilt:{url}") (+ format!("@sha256:{}", &checksum[..7]) if checksum) }`.

Then `IdentityBuilder.build(dylib_file)` → returns `dylib_file` → D8 temp-load → Placer → manifest. `manifest_kind` writes `type = "prebuilt"` (no `ref`).

**Trust + checksum warns (tui-layer, R1)**: after `Installer::install` returns for prebuilt, if `spec.checksum.is_none()` → warn "no checksum supplied; dylib integrity unverified (pass `--checksum <sha256>` to verify)". Plus the §F5c Q5 untrusted-project warn (unchanged: "won't load until workspace trusted (accept trust prompt or /trust, then /extension reload)").

## 6. SourceSpec parser changes

- New field: `pub checksum: Option<String>` on `SourceSpec` (`install_source.rs:69-74`).
- Parse `--checksum <hex>`: iterate whitespace tokens; `--global` → `Global` scope; `--checksum` → consume next token as checksum value (validate 64 lowercase hex chars); the first non-`--`, non-consumed-as-checksum-value token is `spec_token`. (Differs from the current single-pass `find(|t| !t.starts_with("--"))` which would mis-grab the checksum hex value as the spec token.)
- Kind-dependent `@`-split: only `git` + `crate` split `rest.rsplit_once('@')` (→ ref / version on `ref_`); `path` + `prebuilt` take `body = rest` whole (prebuilt URL may contain `@` userinfo; path with `@` is rare but now treated literally). This is a behavior change for `path:`/`prebuilt:` specs containing `@` — more correct (the old behavior mangled URLs).
- For `crate`, the `@<version>` becomes `ref_` (existing field, repurposed as version for the crate kind) — no new field needed for version.
- `manifest_kind` (`installer.rs:118-125`) already maps kinds → no change.

## 7. tui `install_precheck` removal + source construction

- `install_precheck(arg)` (`extension_commands.rs:241`): drop the `SourceKind::CratesIo`/`Prebuilt` arms (`:257`) that early-return `Some(error) "§F5c-later"`. Keep: missing-arg check, malformed-spec (no `:`) check, unknown-kind check. Returns `None` for all 4 valid kinds now (proceed to install).
- The `unreachable!("install_precheck rejected crate/prebuilt")` at `:295` → real match arms constructing the source per `spec.kind`:
  - `Git` → `GitSource::new(spec.body, spec.ref_)` + `CargoBuilder`
  - `Path` → `LocalPathSource::new(spec.body)` + `CargoBuilder`
  - `CratesIo` → `CratesIoSource { name: spec.body, version: spec.ref_, http }` + `CargoBuilder`
  - `Prebuilt` → `PrebuiltDylibSource { url: spec.body, checksum: spec.checksum, http }` + `IdentityBuilder`
  - run `Installer { source, builder, root }.install(&spec)`.
- `http` = `Arc<dyn HttpFetcher>` backed by `CurlHttpFetcher` (the trait + `CurlHttpFetcher` type are **defined in extensions** + re-exported via `lib.rs`; the tui command **constructs** `Arc::new(CurlHttpFetcher)` once per install and clones it into `CratesIoSource`/`PrebuiltDylibSource`).
- Trust + checksum warns (§5) emitted by tui after install (R1: extensions is pure).

## 8. Testing strategy

| Test | crate | content |
|---|---|---|
| `SourceSpec::parse` checksum + kind-`@`-split | extensions | `--checksum <hex>` flag parse + hex validation (reject short/non-hex); prebuilt URL with `@` not split; crate `@version` split → `ref_`; git `@ref` split; path no split; `--checksum` + `--global` + spec token ordering |
| `IdentityBuilder` | extensions | `build(src) -> src` no-op returns input path (a file path for prebuilt) |
| `CurlHttpFetcher` | extensions | curl shell-out; skip if `curl` not on PATH (mirror `cargo_builder_*_tests:475` skip pattern); real `https://` fetch of a tiny known file `#[ignore]` (network) |
| `CratesIoSource` (FakeHttp) | extensions | canned index JSON (multi-version, one yanked) → version selection (latest / exact / yanked-skip / not-found); canned `.crate` bytes with known sha256 → checksum verify (match + mismatch-fail); tar extraction → inner dir path; provenance `crate:<name>@<vers>` |
| `PrebuiltDylibSource` (FakeHttp) | extensions | HTTPS-only refusal (`http://` → Install error); canned dylib bytes → download; checksum verify (supplied+match / supplied+mismatch-fail / absent-proceeds); provenance `prebuilt:<url>[@sha256:<hex7>]` |
| `install_precheck_*` (tui) | tui | the 4 existing tests shift: `crate:`/`prebuilt:` no longer "not-yet-implemented" → proceed (None from precheck); replaced/augmented by real-source-construction + checksum/trust-warn assertions |
| e2e (existing, unchanged) | extensions | `installer::e2e_tests:127-256` `FakeSource`+`FakeBuilder` build→place→manifest→discover→load half already covers the post-fetch pipeline; CratesIo/Prebuilt fetch is unit-tested in isolation (FakeHttp). A full `CratesIoSource`→real-`CargoBuilder`→fixture e2e = `#[ignore]` (network+cargo). |

**No tui e2e** (§F5 precedent). Mirror §F5c §9 testing strategy (trait-DI + unit + fixture e2e).

## 9. File / component map

- `crates/extensions/Cargo.toml` (change): add `sha2.workspace = true` (workspace dep, already declared at root `:59`). **No other new dep.**
- `crates/extensions/src/install_source.rs` (change): add `CratesIoSource` + `PrebuiltDylibSource` (impl `ExtensionSource`) + `IdentityBuilder` (impl `ExtensionBuilder`) + `HttpFetcher` trait + `CurlHttpFetcher` (curl shell-out) + `FakeHttpFetcher` (test-only, `#[cfg(test)]`); extend `SourceSpec` (`checksum` field, kind-dependent `@`-split, `--checksum` parse); update `SourceKind::CratesIo`/`Prebuilt` comments (drop "stubbed"). New test modules `crates_io_source_tests`/`prebuilt_source_tests`/`identity_builder_tests`/`http_fetcher_tests`.
- `crates/extensions/src/installer.rs` (no change): `Installer::install` + `manifest_kind` already kind-aware; `IdentityBuilder` is just another `ExtensionBuilder` impl.
- `crates/extensions/src/lib.rs` (change): re-export `CratesIoSource`/`PrebuiltDylibSource`/`IdentityBuilder`/`HttpFetcher`/`CurlHttpFetcher`.
- `crates/tui/src/commands/extension_commands.rs` (change): drop `install_precheck` crate/prebuilt early-return; construct real sources+builders per `spec.kind`; emit trust+checksum warns.
- `docs/EXTENSIONS.md` (change): update the install source-kind section (drop "not yet implemented" for crate/prebuilt; document `crate:`/`prebuilt:` syntax + `--checksum` + HTTPS-only + trust stance).
- `ROADMAP.md` (change): §F5c progress block "next focus" mark crate/prebuilt done + new `### F5e` subsection (status + still-deferred) + §F5e progress block (decisions, test deltas, by-design gaps).

## 10. Verification gate (slice end, pre-merge)

- `cargo build --workspace` green.
- `cargo test -p codesmith-extensions --lib`: 51 baseline + §F5e new (`SourceSpec` checksum/`@`-split, `IdentityBuilder`, `CratesIoSource` FakeHttp, `PrebuiltDylibSource` FakeHttp, `CurlHttpFetcher`) → record real count.
- `cargo test -p codesmith-agent`: 98 (unchanged — install is tui-side).
- `cargo test -p codesmith-agent-runtime`: 1165+2 ignored (unchanged; flaky `streamable_http_stale_session_reconnects_and_retries_tool_call` isolate-rerun green).
- `cargo test -p codesmith-tui --bin codesmith-tui`: 2866 pass baseline + §F5e `install_precheck` test shifts / 26 pre-existing `runtime_api::tests` env-fail / 2 ignored (report "N pass/26 pre-existing runtime_api fail/2 ignored"; never "green", never attributed to §F5e).
- **grep (§F5e new)**: `CratesIoSource`/`PrebuiltDylibSource`/`IdentityBuilder`/`HttpFetcher`/`CurlHttpFetcher` in `crates/extensions/src` ≥1 each; `sha2` in `crates/extensions/Cargo.toml` ≥1; `--checksum` parse in `SourceSpec::parse` ≥1; `install_precheck` no longer contains "§F5c-later"; `crate:`/`prebuilt:` real arms in `extension_commands.rs` (no `unreachable!("install_precheck rejected crate/prebuilt")`).
- **grep (§F5c/§F5d unchanged)**: `GitSource`/`LocalPathSource`/`CargoBuilder`/`Placer`/`Installer` ≥1 each; `libloading` in extensions Cargo.toml ≥1; `loader.rs`/`manifest.rs`/`build.rs` exist; `discover_dylib` in `engine.rs` ≥1; `codesmith_register_extension` in fixture ≥1; `clear_tools`/`clear_commands`/`drain_libraries_to_pending`/`drop_pending` in `runner.rs` ≥1 (§F5d intact); `host_executor .emit`=16 (unchanged); `TrustReason::FirstLoad` in tui=1 (unchanged).

## 11. Honest-test red-line (§F5c/§F5d precedent)

- tui 26 `runtime_api::tests` PRE-EXISTING env-fail (HTTP-server won't bind / connection-refused; no panic; pre-§F5b base `7a6819a7` isolate-rerun fails same). Report "N pass/26 pre-existing runtime_api fail/2 ignored", **not** green, **not** attributed to §F5e.
- `agent-runtime` `streamable_http_stale_session_reconnects_and_retries_tool_call` flaky (HTTP-server-bind); isolate-rerun green on failure.
- §F5e doesn't touch `host_executor.rs` (install is tui-command-side) → `.emit`=16 unchanged; `TrustReason::FirstLoad` in tui=1 unchanged.
- CratesIo/Prebuilt unit tests use `FakeHttpFetcher` → no network; real-network tests (`CurlHttpFetcher` real fetch, `CratesIoSource`→cargo e2e) `#[ignore]` (skip in CI without network/cargo).

## 12. References

- §F5c spec: `docs/superpowers/specs/2026-07-23-codesmith-extension-system-slice-5c-design.md` (§3 Out-of-scope + Q7 deferral).
- §F5c plan: `docs/superpowers/plans/2026-07-23-codesmith-extension-system-slice-5c.md`.
- §F5d spec: `docs/superpowers/specs/2026-07-24-codesmith-extension-system-slice-5d-design.md` (§1 Out-of-scope "remain §F5c deferred").
- §F5c commits: `98b3a12f`→`2eba6e9c` (T1 `install_source.rs` → T7 docs).
- §F5d commits: `2d66279b` (P2 doc-drift follow-up, HEAD).
- ROADMAP §F5c "By-design gaps" (~lines 2633-2637); `docs/EXTENSIONS.md` install source-kind section.
- Crates.io sparse index protocol: `https://doc.rust-lang.org/cargo/reference/registry-index.html` + `https://doc.rust-lang.org/cargo/reference/registry-web-api.html`.
- Toolchain: plain `cargo` (rustc 1.90.0 / edition 2024 default).

---

## Status — next steps (2026-07-27)

1. ✅ Context recovered from files (git: `main` = `2d66279b`; §F5c T1-T7 + §F5d present; working tree clean except untracked `.zcode/` + `docs/superpowers/`).
2. ✅ §2 code findings verified against `main` HEAD `2d66279b` (`install_source.rs` line numbers, `installer.rs`, `extension_commands.rs` `install_precheck`, workspace deps, sparse-index shape via direct curl).
3. ✅ Design re-presented + user-approved (brainstorming step 5) with sub-choices A (latest + `@version` exact) + B (`--checksum` flag) locked.
4. ✅ Spec written + self-reviewed inline (this file). Tracked per §F5d precedent (committed on feature branch `feat/f5e-cratesio-prebuilt`).
5. ⏭ Next: **user reviews this spec file** → invoke `writing-plans` skill → TDD execute (Red→Green→commit per task; commit messages carry real test counts + decision provenance). Plain `cargo`; 26 tui `runtime_api` fails pre-existing/environmental; `agent-runtime` flaky isolate-rerun.
