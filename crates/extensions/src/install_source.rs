//! Install-source abstraction **traits** (spec §6.4). Impls are §F5
//! (dylib loading). Slice 1 ships only the trait shapes so the §F1
//! `/extension install` stub (Task 8) can reference
//! [`ExtensionError::Install`](codesmith_agent::extension::ExtensionError)
//! and so the ROADMAP §F5 entry has a stable contract to point at.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use codesmith_agent::extension::ExtensionError;
use sha2::{Digest, Sha256};

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
    CratesIo, // §F5e: real CratesIoSource impl (was §F5c stub)
    Prebuilt, // §F5e: real PrebuiltDylibSource impl (was §F5c stub)
}

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
                )));
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
        let entry = entry.map_err(|e| ExtensionError::Install(format!("dir entry: {e}")))?;
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
                    ExtensionError::Install(format!(
                        "version {v} of {} not found or yanked",
                        self.name
                    ))
                })?,
            None => entries.iter().rev().find(|e| !e.yanked).ok_or_else(|| {
                ExtensionError::Install(format!("no non-yanked version for {}", self.name))
            })?,
        }
        .clone();
        // 4. download .crate
        let crate_file = dest.join(format!("{}-{}.crate", self.name, entry.vers));
        let crate_url = format!(
            "https://static.crates.io/crates/{}/{}-{}.crate",
            self.name, self.name, entry.vers
        );
        self.http.fetch_to(&crate_url, &crate_file)?;
        // 5. sha256 verify (registry cksum is mandatory; free integrity)
        let bytes = std::fs::read(&crate_file).map_err(|e| {
            ExtensionError::Install(format!("read crate {}: {e}", crate_file.display()))
        })?;
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
        // .crate no longer needed post-extract; tidy the working dir
        let _ = std::fs::remove_file(&crate_file);
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
    #[serde(default)]
    yanked: bool,
    #[serde(default)]
    pubtime: String,
}

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
            let bytes = std::fs::read(&dest_file).map_err(|e| {
                ExtensionError::Install(format!("read dylib {}: {e}", dest_file.display()))
            })?;
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
        std::fs::copy(artifact, &dest).map_err(|e| {
            ExtensionError::Install(format!("copy dylib to {}: {e}", dest.display()))
        })?;
        Ok(dest)
    }
}

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
        let body = self.responses.get(url).ok_or_else(|| {
            ExtensionError::Install(format!("FakeHttp: no canned response for {url}"))
        })?;
        std::fs::write(dest, body)
            .map_err(|e| ExtensionError::Install(format!("FakeHttp write {url}: {e}")))?;
        Ok(())
    }

    fn fetch_text(&self, url: &str) -> Result<String, ExtensionError> {
        let body = self.responses.get(url).ok_or_else(|| {
            ExtensionError::Install(format!("FakeHttp: no canned response for {url}"))
        })?;
        Ok(String::from_utf8_lossy(body).into_owned())
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
        assert!(
            matches!(r, Err(ExtensionError::Install(_))),
            "uppercase rejected: {r:?}"
        );
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
        assert!(
            art.provenance.starts_with("path:"),
            "provenance: {}",
            art.provenance
        );
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
        assert_eq!(
            std::fs::read(dst.path().join("a/b/c.txt")).unwrap(),
            b"deep"
        );
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
        let expected = root
            .path()
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

#[cfg(test)]
mod crates_io_source_tests {
    use super::*;
    use std::sync::Arc;

    /// Build a canned `.crate` (gzipped tar) in a temp dir: creates
    /// `<name>-<vers>/Cargo.toml` then `tar -czf`. Returns `(bytes, sha256_hex)`.
    /// Returns `None` (skips the test) if `tar` not on PATH.
    fn make_crate_fixture(name: &str, vers: &str) -> Option<(Vec<u8>, String)> {
        if std::process::Command::new("tar")
            .arg("--version")
            .output()
            .is_err()
        {
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
        assert!(
            out.status.success(),
            "tar: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = std::fs::read(&crate_file).ok()?;
        let mut h = sha2::Sha256::new();
        sha2::Digest::update(&mut h, &bytes);
        let cksum = format!("{:x}", h.finalize());
        Some((bytes, cksum))
    }

    /// Throwaway index URL for a name (constructs a source with a no-op
    /// FakeHttp just to read `index_url_for_test`).
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
        let Some((_bytes020, cksum020)) = make_crate_fixture(name, "0.2.0") else {
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
        let art = src
            .fetch(dest.path())
            .expect("fetch picks 0.1.0 (non-yanked)");
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
        let src =
            PrebuiltDylibSource::new("http://x/y.dylib", None, Arc::new(FakeHttpFetcher::new()));
        let dest = tempfile::tempdir().unwrap();
        let r = src.fetch(dest.path());
        let Err(ExtensionError::Install(m)) = &r else {
            panic!("{r:?}")
        };
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

    #[test]
    fn prebuilt_trailing_slash_url_uses_fallback_filename() {
        // URL ending in '/' → basename empty → fallback dylib.<DLL_EXT>.
        // (Placer renames to default_dylib_filename(id) regardless, so the
        // temp filename shape only affects the pre-rename download.)
        let url = "https://x.example/path/";
        let http = FakeHttpFetcher::new().with(url, b"x".to_vec());
        let src = PrebuiltDylibSource::new(url, None, Arc::new(http));
        let dest = tempfile::tempdir().unwrap();
        let art = src.fetch(dest.path()).unwrap();
        assert_eq!(
            art.path.file_name().unwrap().to_str().unwrap(),
            format!("dylib.{}", std::env::consts::DLL_EXTENSION)
        );
    }
}
