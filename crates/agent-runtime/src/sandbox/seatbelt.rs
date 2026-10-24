//! macOS Seatbelt (sandbox-exec) profile generation.
//!
//! Seatbelt is Apple's mandatory access control framework that uses the
//! Scheme-based policy language to define what system resources a process
//! can access. This module generates sandbox profiles dynamically based
//! on the configured `SandboxPolicy`.
//!
//! # How it works
//!
//! 1. We generate a Seatbelt policy string in the SBPL format
//! 2. We invoke `/usr/bin/sandbox-exec -p <policy>` to run the command
//! 3. The kernel enforces the policy, blocking unauthorized operations
//!
//! # References
//!
//! - Apple's sandbox(7) man page
//! - <https://reverse.put.as/wp-content/uploads/2011/09/Apple-Sandbox-Guide-v1.0.pdf>

// `unsafe` rationale: Seatbelt probes macOS kernel facilities via `libc`
// (`confstr(_CS_DARWIN_USER_CACHE_DIR)`) and mutates process-global env vars
// (`HOME`/`CARGO_HOME`/`NPM_CONFIG_CACHE`) in tests. Each `unsafe` block is
// documented at its site. Mirrors the `child_env` file-local allow pattern.
#![allow(unsafe_code)]

// Note: cfg(target_os = "macos") is already applied at the module level in mod.rs

use super::{SandboxPolicy, WritableRoot};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Path to the sandbox-exec binary on macOS.
pub const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// Base seatbelt policy that provides minimal process functionality.
///
/// This policy:
/// - Denies everything by default
/// - Allows process execution and forking
/// - Allows signals within the same sandbox
/// - Allows reading user preferences (needed by many tools)
/// - Allows basic process introspection
/// - Allows writing to /dev/null
/// - Allows reading sysctl values
/// - Allows POSIX semaphores and pseudo-TTY operations
const SEATBELT_BASE_POLICY: &str = r#"
(version 1)
(deny default)

; Core process operations
; process-exec is intentionally unrestricted: Agent mode runs arbitrary
; developer tooling (cargo, npm, git, curl-style helpers) inside the
; sandbox, and any subpath/literal filter would break that contract. The
; filesystem-write rules remain the actual containment boundary.
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))

; User preferences (needed by many CLI tools)
(allow user-preference-read)

; Basic I/O to /dev/null
(allow file-write-data
  (require-all
    (path "/dev/null")
    (vnode-type CHARACTER-DEVICE)))

; System information
(allow sysctl-read)

; IPC primitives
(allow ipc-posix-sem)
(allow ipc-posix-shm-read*)
(allow ipc-posix-shm-write-create)
(allow ipc-posix-shm-write-data)
(allow ipc-posix-shm-write-unlink)

; Terminal support (essential for shell commands)
(allow pseudo-tty)
(allow file-read* file-write* file-ioctl (literal "/dev/ptmx"))
(allow file-read* file-write* file-ioctl (regex #"^/dev/ttys[0-9]+$"))

; macOS-specific device access
(allow file-read* (literal "/dev/urandom"))
(allow file-read* (literal "/dev/random"))
(allow file-ioctl (literal "/dev/dtracehelper"))

; Mach IPC — intentionally unrestricted: macOS CLI tools have hard-to-enumerate
; implicit Mach dependencies (DNS resolution, os_log, system configuration).
; Tightening this to a service whitelist is a known follow-up that requires
; real-world regression testing on macOS.
(allow mach-lookup)
"#;

/// Network access policy additions.
///
/// Outbound-only by design: `SandboxPolicy::has_network_access` is documented
/// as "whether outbound network connections are permitted", so inbound
/// listeners and port binds are never granted here.
const SEATBELT_NETWORK_POLICY: &str = r"
; Network access (outbound only — matches the policy's contract)
(allow network-outbound)
(allow system-socket)
";

/// Check if sandbox-exec is available and permitted on this system.
///
/// Only definitive answers latch into the process-lifetime cache: a missing
/// binary or a completed probe (success or clean failure) is definitive; a
/// transient spawn error (fork/exec hiccup, resource pressure) returns
/// `false` for THIS call without latching, so a later call retries instead
/// of the whole process permanently downgrading to "no sandbox".
pub fn is_available() -> bool {
    static SEATBELT_AVAILABLE: OnceLock<bool> = OnceLock::new();

    if let Some(cached) = SEATBELT_AVAILABLE.get() {
        return *cached;
    }

    if !Path::new(SANDBOX_EXEC_PATH).exists() {
        // Definitive: nothing to retry.
        let _ = SEATBELT_AVAILABLE.set(false);
        return false;
    }

    let probed = Command::new(SANDBOX_EXEC_PATH)
        .args(["-p", "(version 1)(allow default)", "--", "/usr/bin/true"])
        .output();

    match probed {
        Ok(result) => {
            let available = result.status.success();
            let _ = SEATBELT_AVAILABLE.set(available);
            available
        }
        // Transient spawn failure: do NOT latch — report unavailable for
        // this call and let the next call retry.
        Err(_) => false,
    }
}

/// Env- and filesystem-derived inputs shared by [`generate_policy`] and
/// [`generate_params`], resolved ONCE per generated profile so the policy
/// text and the `-D` param table can never diverge: a `(param "KEY")` the
/// args never define makes sandbox-exec refuse to load the profile, and
/// every source below (env vars, `exists()`/`canonicalize()` probes,
/// TMPDIR) can change between two independent resolutions.
struct ProfileContext {
    writable_roots: Vec<WritableRoot>,
    cargo_home: Option<PathBuf>,
    npm_cache_dir: Option<PathBuf>,
    darwin_user_cache_dir: PathBuf,
}

fn resolve_profile_context(policy: &SandboxPolicy, cwd: &Path) -> ProfileContext {
    ProfileContext {
        writable_roots: policy.get_writable_roots(cwd),
        cargo_home: resolve_cargo_home(),
        npm_cache_dir: resolve_npm_cache_dir(),
        darwin_user_cache_dir: get_darwin_user_cache_dir()
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|home| PathBuf::from(format!("{home}/Library/Caches")))
            })
            .unwrap_or_else(|| PathBuf::from("/private/var/empty")),
    }
}

/// Create the command-line arguments for sandbox-exec.
///
/// Returns a Vec of arguments that should be prepended to the command.
/// The format is: `sandbox-exec -p <policy> -D KEY=VALUE ... -- <original command>`
pub fn create_seatbelt_args(
    command: Vec<String>,
    policy: &SandboxPolicy,
    sandbox_cwd: &Path,
) -> Vec<String> {
    let ctx = resolve_profile_context(policy, sandbox_cwd);
    let full_policy = generate_policy(policy, &ctx);
    let params = generate_params(&ctx);

    let mut args = vec!["-p".to_string(), full_policy];

    // Add parameter definitions for variable substitution
    for (key, value) in params {
        args.push(format!("-D{}={}", key, value.to_string_lossy()));
    }

    // Separator between sandbox-exec args and the actual command
    args.push("--".to_string());
    args.extend(command);

    args
}

/// Generate the complete Seatbelt policy string for the given policy.
fn generate_policy(policy: &SandboxPolicy, ctx: &ProfileContext) -> String {
    let mut full_policy = SEATBELT_BASE_POLICY.to_string();

    // Add read access policy
    if SandboxPolicy::has_full_disk_read_access() {
        full_policy.push_str("\n; Full filesystem read access\n(allow file-read*)");
    }

    // Add write access policy
    let file_write_policy = generate_write_policy(policy, ctx);
    if !file_write_policy.is_empty() {
        full_policy.push_str("\n\n; Write access policy\n");
        full_policy.push_str(&file_write_policy);
    }

    // Add network policy if enabled
    if policy.has_network_access() {
        full_policy.push('\n');
        full_policy.push_str(SEATBELT_NETWORK_POLICY);
    }

    // Add Darwin user cache directory access (needed by many macOS tools).
    // Read always; write only for policies that allow any write at all —
    // ReadOnly keeps the cache directory read-only, mirroring the cargo and
    // npm cache grants below.
    full_policy.push_str("\n\n; Darwin user cache directory\n");
    full_policy.push_str(r#"(allow file-read* (subpath (param "DARWIN_USER_CACHE_DIR")))"#);
    if !matches!(policy, SandboxPolicy::ReadOnly) {
        full_policy.push('\n');
        full_policy.push_str(r#"(allow file-write* (subpath (param "DARWIN_USER_CACHE_DIR")))"#);
    }

    // Add common macOS directories that tools often need
    full_policy.push_str("\n\n; Common macOS directories\n");
    full_policy.push_str(r#"(allow file-read* (subpath "/usr/lib"))"#);
    full_policy.push('\n');
    full_policy.push_str(r#"(allow file-read* (subpath "/usr/share"))"#);
    full_policy.push('\n');
    full_policy.push_str(r#"(allow file-read* (subpath "/System/Library"))"#);
    full_policy.push('\n');
    full_policy.push_str(r#"(allow file-read* (subpath "/Library/Preferences"))"#);
    full_policy.push('\n');
    full_policy.push_str(r#"(allow file-read* (subpath "/private/var/db"))"#);

    // Cargo home (#558): cargo build/test/publish reach into ~/.cargo/registry
    // and ~/.cargo/git for crate metadata, downloaded tarballs, and unpacked
    // sources. Sandboxed workspace-write was previously rejecting these,
    // making `cargo publish` unrunnable from inside the shell tool.
    // Read access is always allowed; write access is granted whenever the
    // policy allows any write at all (the registry caches need to be
    // mutable for `cargo build` to populate them on a cache miss). Skipped
    // entirely when neither `CARGO_HOME` nor `HOME` is set — without one of
    // those we have no path to plumb into the policy params.
    if ctx.cargo_home.is_some() {
        full_policy.push_str("\n\n; Cargo home (~/.cargo) — registry/index/git caches\n");
        full_policy.push_str(r#"(allow file-read* (subpath (param "CARGO_HOME")))"#);
        if !matches!(policy, SandboxPolicy::ReadOnly) {
            full_policy.push('\n');
            full_policy.push_str(r#"(allow file-write* (subpath (param "CARGO_HOME_REGISTRY")))"#);
            full_policy.push('\n');
            full_policy.push_str(r#"(allow file-write* (subpath (param "CARGO_HOME_GIT")))"#);
        }
    }

    // npm cache (#1267): npx-based MCP servers write to ~/.npm when downloading
    // packages on first run. Without write access the npx subprocess fails
    // immediately with "Stdio transport closed", making all stdio MCP servers
    // broken on macOS under the default workspace-write policy.
    // Read access is always allowed; write access mirrors the cargo pattern —
    // granted for all policies that allow any write, skipped for ReadOnly.
    // Skipped entirely when neither `NPM_CONFIG_CACHE` nor `HOME` is set.
    if ctx.npm_cache_dir.is_some() {
        full_policy.push_str("\n\n; npm cache (~/.npm) — npx package downloads\n");
        full_policy.push_str(r#"(allow file-read* (subpath (param "NPM_CACHE_DIR")))"#);
        if !matches!(policy, SandboxPolicy::ReadOnly) {
            full_policy.push('\n');
            full_policy.push_str(r#"(allow file-write* (subpath (param "NPM_CACHE_DIR")))"#);
        }
    }

    full_policy
}

/// Resolve the user's cargo home — `CARGO_HOME` if set, else `$HOME/.cargo`.
/// Returns `None` only on hosts where neither env var is set (essentially
/// never on a real macOS user account; can happen in CI containers without
/// `HOME` exported).
fn resolve_cargo_home() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("CARGO_HOME")
        && !explicit.trim().is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".cargo"))
}

/// Resolve the npm cache directory — `NPM_CONFIG_CACHE` if set, else `$HOME/.npm`.
/// Returns `None` only on hosts where neither env var is set.
fn resolve_npm_cache_dir() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("NPM_CONFIG_CACHE")
        && !explicit.trim().is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".npm"))
}

/// Generate the write access portion of the Seatbelt policy.
fn generate_write_policy(policy: &SandboxPolicy, ctx: &ProfileContext) -> String {
    // Full disk write access
    if policy.has_full_disk_write_access() {
        return r#"(allow file-write* (regex #"^/"))"#.to_string();
    }

    // Read-only - no write policy needed
    if matches!(policy, SandboxPolicy::ReadOnly) {
        return String::new();
    }

    // Workspace write - enumerate allowed paths
    let writable_roots = &ctx.writable_roots;
    if writable_roots.is_empty() {
        return String::new();
    }

    let mut policies = Vec::new();

    for (index, root) in writable_roots.iter().enumerate() {
        let root_param = format!("WRITABLE_ROOT_{index}");

        if root.read_only_subpaths.is_empty() {
            // Simple case: entire subtree is writable
            policies.push(format!("(subpath (param \"{root_param}\"))"));
        } else {
            // Complex case: writable with read-only exceptions
            // Use require-all to combine subpath with require-not for each exception
            let mut parts = vec![format!("(subpath (param \"{}\"))", root_param)];

            for (subpath_index, _) in root.read_only_subpaths.iter().enumerate() {
                let ro_param = format!("WRITABLE_ROOT_{index}_RO_{subpath_index}");
                parts.push(format!("(require-not (subpath (param \"{ro_param}\")))"));
            }

            policies.push(format!("(require-all {})", parts.join(" ")));
        }
    }

    if policies.is_empty() {
        return String::new();
    }

    // SBPL ANDs every filter inside a single `allow`, so disjoint roots
    // joined under one statement would require a path to be under all of
    // them at once — silently denying every write. Emit one allow
    // statement per root (or per root+exceptions group) instead.
    policies
        .iter()
        .map(|p| format!("(allow file-write* {p})"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Generate parameter definitions for variable substitution in the policy.
///
/// sandbox-exec allows -DKEY=VALUE to substitute `(param "KEY")` in the policy.
/// All values come from the caller's [`ProfileContext`] snapshot so they stay
/// in sync with the policy text.
fn generate_params(ctx: &ProfileContext) -> Vec<(String, PathBuf)> {
    let mut params = Vec::new();

    // Add writable root parameters
    let writable_roots = &ctx.writable_roots;

    for (index, root) in writable_roots.iter().enumerate() {
        // Resolve via deepest-existing-ancestor canonicalization: SBPL
        // `subpath` filters match kernel-resolved vnode paths, so a
        // raw-spelled root behind macOS symlinks (`/tmp` → `/private/tmp`,
        // `/var` → `/private/var`) or a not-yet-existing root would never
        // match and every write into it would fail with a confusing EPERM.
        let canonical = super::normalize_for_containment(&root.root);
        params.push((format!("WRITABLE_ROOT_{index}"), canonical.clone()));

        // Add parameters for read-only subpaths
        for (subpath_index, subpath) in root.read_only_subpaths.iter().enumerate() {
            // Fail closed: SBPL `subpath` filters match kernel-resolved
            // paths, so an exception that falls back to its raw form
            // (deleted/racy path, `..`, unresolved symlink) silently stops
            // matching and writes into the "read-only" area stay allowed.
            // Derive the exception from the canonicalized root instead.
            // `PathBuf::join` with an ABSOLUTE path replaces the base
            // entirely, so an absolute subpath that failed to canonicalize
            // would point the exception at an arbitrary path (fail-open);
            // when the derivation cannot be proven to sit under the root,
            // pin the exception to the root itself — the whole root becomes
            // read-only rather than the protected area losing protection.
            let canonical_subpath = match subpath.canonicalize() {
                Ok(p) if p.starts_with(&canonical) => p,
                Ok(_) => canonical.clone(),
                Err(_) => {
                    let derived =
                        canonical.join(subpath.strip_prefix(&root.root).unwrap_or(subpath));
                    if derived.starts_with(&canonical) {
                        derived
                    } else {
                        canonical.clone()
                    }
                }
            };
            params.push((
                format!("WRITABLE_ROOT_{index}_RO_{subpath_index}"),
                canonical_subpath,
            ));
        }
    }

    // Add Darwin user cache directory. The policy text ABOVE references
    // `(param "DARWIN_USER_CACHE_DIR")` unconditionally, so the param must
    // ALWAYS be defined — an undefined param makes sandbox-exec refuse to
    // load the profile and every sandboxed command fails (the exact
    // policy/param desync the cargo/npm blocks guard against). Neither
    // confstr nor HOME resolving happens in hardened/launchd contexts and
    // HOME-less containers, hence the final harmless-directory fallback
    // (applied in `resolve_profile_context`).
    params.push((
        "DARWIN_USER_CACHE_DIR".to_string(),
        ctx.darwin_user_cache_dir.clone(),
    ));

    // Cargo home (#558): paired with the policy lines emitted by
    // `generate_policy` when `resolve_cargo_home()` succeeds. Both helpers
    // use the same fallback chain so the policy text and the -DKEY=VALUE
    // params stay in sync — emit one without the other and sandbox-exec
    // refuses to load the profile.
    if let Some(home) = &ctx.cargo_home {
        let canonical_home = home.canonicalize().unwrap_or_else(|_| home.clone());
        params.push((
            "CARGO_HOME_REGISTRY".to_string(),
            canonical_home.join("registry"),
        ));
        params.push(("CARGO_HOME_GIT".to_string(), canonical_home.join("git")));
        params.push(("CARGO_HOME".to_string(), canonical_home));
    }

    // npm cache (#1267): paired with the policy lines emitted by
    // `generate_policy` when `resolve_npm_cache_dir()` succeeds. Both helpers
    // use the same fallback chain so the policy text and the -DKEY=VALUE
    // params stay in sync.
    if let Some(npm_cache) = &ctx.npm_cache_dir {
        let canonical = npm_cache
            .canonicalize()
            .unwrap_or_else(|_| npm_cache.clone());
        params.push(("NPM_CACHE_DIR".to_string(), canonical));
    }

    params
}

/// Get the Darwin user cache directory using confstr.
///
/// This returns the per-user cache directory that macOS assigns,
/// typically something like /var/folders/xx/xxx.../C/
fn get_darwin_user_cache_dir() -> Option<PathBuf> {
    // Use libc to call confstr for _CS_DARWIN_USER_CACHE_DIR
    let mut buf = vec![0i8; (libc::PATH_MAX as usize) + 1];

    // Safety: `buf` is a writable buffer sized to PATH_MAX + 1 for confstr.
    let len =
        unsafe { libc::confstr(libc::_CS_DARWIN_USER_CACHE_DIR, buf.as_mut_ptr(), buf.len()) };

    if len == 0 {
        return None;
    }

    // Convert the C string to a Rust PathBuf
    // Safety: confstr guarantees a NUL-terminated string in `buf` when len > 0.
    let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    let path_str = cstr.to_str().ok()?;
    let path = PathBuf::from(path_str);

    // Try to canonicalize, but return the raw path if that fails
    path.canonicalize().ok().or(Some(path))
}

/// Detect sandbox denial from command output.
///
/// Returns true if the output suggests the sandbox blocked an operation.
pub fn detect_denial(exit_code: i32, stderr: &str) -> bool {
    if exit_code == 0 {
        return false;
    }

    // Common sandbox denial messages. The bare binary name "sandbox-exec"
    // is deliberately absent — it over-matched any stderr line mentioning
    // the binary (e.g. a usage message); `SandboxManager::was_denied`
    // already gates on the command having actually run under sandbox-exec,
    // so EPERM ("Operation not permitted") stays corroborated.
    let denial_patterns = ["Operation not permitted", "deny(", "Sandbox: "];

    denial_patterns.iter().any(|p| stderr.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests that mutate HOME/CARGO_HOME use crate::test_support::lock_test_env()
    // so they don't race with sibling tests in this crate that read those vars.
    #[test]
    fn test_is_available() {
        // This test just checks the function doesn't panic
        // On macOS it should return true, on other platforms false
        let _ = is_available();
    }

    #[test]
    fn test_generate_policy_default() {
        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let ctx = resolve_profile_context(&policy, cwd);
        let result = generate_policy(&policy, &ctx);

        assert!(result.contains("(version 1)"));
        assert!(result.contains("(deny default)"));
        assert!(result.contains("(allow file-read*)"));
        assert!(result.contains("file-write*"));
        // Default policy has no network
        assert!(!result.contains("network-outbound"));
    }

    #[test]
    fn test_generate_policy_with_network() {
        let policy = SandboxPolicy::workspace_with_network();
        let cwd = Path::new("/tmp/test");
        let ctx = resolve_profile_context(&policy, cwd);
        let result = generate_policy(&policy, &ctx);

        assert!(result.contains("network-outbound"));
        // The policy only promises outbound connectivity; inbound listeners
        // and port binds must never be granted.
        assert!(!result.contains("network-inbound"));
        assert!(!result.contains("network-bind"));
    }

    #[test]
    fn test_generate_policy_read_only() {
        let policy = SandboxPolicy::ReadOnly;
        let cwd = Path::new("/tmp/test");
        let ctx = resolve_profile_context(&policy, cwd);
        let result = generate_policy(&policy, &ctx);

        assert!(result.contains("(allow file-read*)"));
        // Should not have workspace write rules
        assert!(!result.contains("WRITABLE_ROOT"));
        // The Darwin user cache stays readable but, like the cargo/npm
        // caches, must not be writable under a read-only policy.
        assert!(result.contains(r#"(allow file-read* (subpath (param "DARWIN_USER_CACHE_DIR")))"#));
        assert!(
            !result.contains(r#"(allow file-write* (subpath (param "DARWIN_USER_CACHE_DIR")))"#),
            "ReadOnly must not grant cache-directory writes: {result}"
        );

        // Any write-capable policy keeps the cache writable.
        let writable = SandboxPolicy::default();
        let writable_ctx = resolve_profile_context(&writable, cwd);
        let writable_text = generate_policy(&writable, &writable_ctx);
        assert!(
            writable_text
                .contains(r#"(allow file-write* (subpath (param "DARWIN_USER_CACHE_DIR")))"#)
        );
    }

    #[test]
    fn test_multi_root_write_policy_emits_one_allow_per_root() {
        // SBPL ANDs every filter inside a single `allow` statement, so
        // disjoint writable roots joined under one allow would deny ALL
        // writes (a path cannot sit under two roots at once). Each root
        // must get its own `(allow file-write* …)`.
        let policy = SandboxPolicy::workspace_with_roots(
            vec![
                PathBuf::from("/tmp/codesmith-seatbelt-root-a"),
                PathBuf::from("/tmp/codesmith-seatbelt-root-b"),
            ],
            false,
        );
        let cwd = Path::new("/tmp/codesmith-seatbelt-cwd");
        let ctx = resolve_profile_context(&policy, cwd);
        let write_policy = generate_write_policy(&policy, &ctx);

        // Two explicit roots + the cwd itself.
        assert_eq!(
            write_policy.matches("(allow file-write*").count(),
            3,
            "expected one allow statement per writable root, got:\n{write_policy}"
        );
    }

    #[test]
    fn test_generate_params() {
        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let ctx = resolve_profile_context(&policy, cwd);
        let params = generate_params(&ctx);

        // Should have at least the cache dir param
        assert!(params.iter().any(|(k, _)| k == "DARWIN_USER_CACHE_DIR"));
    }

    #[test]
    fn test_read_only_subpath_params_are_kernel_resolved() {
        // The (require-not (subpath …)) exceptions must match the same
        // kernel-resolved vnode paths as the writable root (`/tmp` →
        // `/private/tmp` on macOS); a raw-form fallback would silently stop
        // excluding the "read-only" area.
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".codesmith")).expect("protected dir");

        let policy = SandboxPolicy::workspace_with_roots(vec![tmp.path().to_path_buf()], false);
        let ctx = resolve_profile_context(&policy, tmp.path());
        let params = generate_params(&ctx);

        let root_param = params
            .iter()
            .find(|(k, _)| k == "WRITABLE_ROOT_0")
            .expect("root param");
        let ro_param = params
            .iter()
            .find(|(k, _)| k == "WRITABLE_ROOT_0_RO_0")
            .expect("read-only subpath param");
        // The exception must live under the canonicalized root — never the
        // raw tempdir spelling.
        assert!(
            ro_param.1.starts_with(&root_param.1),
            "RO param {} must sit under canonical root {}",
            ro_param.1.display(),
            root_param.1.display()
        );
        assert!(ro_param.1.ends_with(".codesmith"));
    }

    /// #558: cargo publish reaches into ~/.cargo/registry; the seatbelt has
    /// to allow read+write inside it. Both the policy text and the param
    /// table must be in sync — emitting one without the other makes
    /// sandbox-exec refuse to load the profile.
    #[test]
    fn test_cargo_home_paths_emitted_in_policy_and_params_when_home_set() {
        let _guard = crate::test_support::lock_test_env();

        // SAFETY: HOME / CARGO_HOME are process-global. lock_test_env
        // serializes tests that mutate them, and we always restore the
        // prior value before returning.
        let saved_home = std::env::var_os("HOME");
        let saved_cargo = std::env::var_os("CARGO_HOME");
        unsafe {
            std::env::set_var("HOME", "/tmp/seatbelt-cargo-test");
            std::env::remove_var("CARGO_HOME");
        }

        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let ctx = resolve_profile_context(&policy, cwd);

        let policy_text = generate_policy(&policy, &ctx);
        assert!(policy_text.contains(r#"(allow file-read* (subpath (param "CARGO_HOME")))"#));
        assert!(policy_text.contains("CARGO_HOME_REGISTRY"));
        assert!(policy_text.contains("CARGO_HOME_GIT"));

        let params = generate_params(&ctx);
        assert!(params.iter().any(|(k, _)| k == "CARGO_HOME"));
        assert!(params.iter().any(|(k, _)| k == "CARGO_HOME_REGISTRY"));
        assert!(params.iter().any(|(k, _)| k == "CARGO_HOME_GIT"));

        // Read-only policy should still emit CARGO_HOME read rule but skip writes.
        let read_only_ctx = resolve_profile_context(&SandboxPolicy::ReadOnly, cwd);
        let read_only_text = generate_policy(&SandboxPolicy::ReadOnly, &read_only_ctx);
        assert!(
            read_only_text.contains(r#"(allow file-read* (subpath (param "CARGO_HOME")))"#),
            "read-only mode should still allow reading the cargo registry: {read_only_text}"
        );
        assert!(
            !read_only_text
                .contains(r#"(allow file-write* (subpath (param "CARGO_HOME_REGISTRY")))"#),
            "read-only mode must NOT grant write access to the cargo registry"
        );

        // Restore.
        // SAFETY: restoring the prior value the test stashed at entry.
        unsafe {
            match saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match saved_cargo {
                Some(v) => std::env::set_var("CARGO_HOME", v),
                None => std::env::remove_var("CARGO_HOME"),
            }
        }
    }

    /// #558: if neither `CARGO_HOME` nor `HOME` is set, the cargo lines and
    /// their params must both be omitted — emitting one without the other
    /// would crash sandbox-exec on profile load.
    #[test]
    fn test_cargo_home_skipped_when_no_env() {
        let _guard = crate::test_support::lock_test_env();

        let saved_home = std::env::var_os("HOME");
        let saved_cargo = std::env::var_os("CARGO_HOME");
        // SAFETY: HOME/CARGO_HOME are process-global; lock_test_env serializes
        // mutations here and we restore the prior values before returning.
        unsafe {
            std::env::remove_var("HOME");
            std::env::remove_var("CARGO_HOME");
        }

        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let ctx = resolve_profile_context(&policy, cwd);
        let policy_text = generate_policy(&policy, &ctx);
        let params = generate_params(&ctx);

        assert!(!policy_text.contains("CARGO_HOME"));
        assert!(!params.iter().any(|(k, _)| k.starts_with("CARGO_HOME")));

        // Restore.
        // SAFETY: restoring the prior values the test stashed at entry.
        unsafe {
            match saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match saved_cargo {
                Some(v) => std::env::set_var("CARGO_HOME", v),
                None => std::env::remove_var("CARGO_HOME"),
            }
        }
    }

    /// #1267: npx MCP servers write to ~/.npm on first run; the seatbelt must
    /// allow writes to the npm cache directory. Both the policy text and the
    /// param table must be in sync — emitting one without the other makes
    /// sandbox-exec refuse to load the profile.
    #[test]
    fn test_npm_cache_paths_emitted_in_policy_and_params_when_home_set() {
        let _guard = crate::test_support::lock_test_env();

        let saved_home = std::env::var_os("HOME");
        let saved_npm = std::env::var_os("NPM_CONFIG_CACHE");
        // SAFETY: HOME/NPM_CONFIG_CACHE are process-global; lock_test_env
        // serializes mutations here, and we always restore the prior value.
        unsafe {
            std::env::set_var("HOME", "/tmp/seatbelt-npm-test");
            std::env::remove_var("NPM_CONFIG_CACHE");
        }

        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let ctx = resolve_profile_context(&policy, cwd);

        let policy_text = generate_policy(&policy, &ctx);
        assert!(
            policy_text.contains(r#"(allow file-read* (subpath (param "NPM_CACHE_DIR")))"#),
            "npm cache read rule missing from policy"
        );
        assert!(
            policy_text.contains(r#"(allow file-write* (subpath (param "NPM_CACHE_DIR")))"#),
            "npm cache write rule missing from default policy"
        );

        let params = generate_params(&ctx);
        assert!(
            params.iter().any(|(k, _)| k == "NPM_CACHE_DIR"),
            "NPM_CACHE_DIR param missing"
        );

        // ReadOnly policy: read access allowed, write access must be absent.
        let read_only_ctx = resolve_profile_context(&SandboxPolicy::ReadOnly, cwd);
        let read_only_text = generate_policy(&SandboxPolicy::ReadOnly, &read_only_ctx);
        assert!(
            read_only_text.contains(r#"(allow file-read* (subpath (param "NPM_CACHE_DIR")))"#),
            "read-only mode should allow reading the npm cache"
        );
        assert!(
            !read_only_text.contains(r#"(allow file-write* (subpath (param "NPM_CACHE_DIR")))"#),
            "read-only mode must NOT grant write access to the npm cache"
        );

        // Restore.
        // SAFETY: restoring the prior values the test stashed at entry.
        unsafe {
            match saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match saved_npm {
                Some(v) => std::env::set_var("NPM_CONFIG_CACHE", v),
                None => std::env::remove_var("NPM_CONFIG_CACHE"),
            }
        }
    }

    /// #1267: if neither `NPM_CONFIG_CACHE` nor `HOME` is set, the npm lines
    /// and their param must both be omitted.
    #[test]
    fn test_npm_cache_skipped_when_no_env() {
        let _guard = crate::test_support::lock_test_env();

        let saved_home = std::env::var_os("HOME");
        let saved_npm = std::env::var_os("NPM_CONFIG_CACHE");
        // SAFETY: HOME/NPM_CONFIG_CACHE are process-global; lock_test_env
        // serializes mutations here and we restore the prior values before returning.
        unsafe {
            std::env::remove_var("HOME");
            std::env::remove_var("NPM_CONFIG_CACHE");
        }

        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let ctx = resolve_profile_context(&policy, cwd);
        let policy_text = generate_policy(&policy, &ctx);
        let params = generate_params(&ctx);

        assert!(!policy_text.contains("NPM_CACHE_DIR"));
        assert!(!params.iter().any(|(k, _)| k == "NPM_CACHE_DIR"));

        // Restore.
        // SAFETY: restoring the prior values the test stashed at entry.
        unsafe {
            match saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match saved_npm {
                Some(v) => std::env::set_var("NPM_CONFIG_CACHE", v),
                None => std::env::remove_var("NPM_CONFIG_CACHE"),
            }
        }
    }

    #[test]
    fn test_create_seatbelt_args() {
        let policy = SandboxPolicy::default();
        let cwd = Path::new("/tmp/test");
        let command = vec!["echo".to_string(), "hello".to_string()];

        let args = create_seatbelt_args(command, &policy, cwd);

        // Should start with -p and the policy
        assert_eq!(args[0], "-p");
        assert!(args[1].contains("(version 1)"));

        // Should contain the separator
        assert!(args.contains(&"--".to_string()));

        // Should end with the original command
        assert!(args.contains(&"echo".to_string()));
        assert!(args.contains(&"hello".to_string()));
    }

    #[test]
    fn test_detect_denial() {
        assert!(detect_denial(1, "Operation not permitted"));
        assert!(detect_denial(1, "Sandbox: ls denied file-write*"));
        assert!(!detect_denial(0, "Operation not permitted"));
        assert!(!detect_denial(1, "File not found"));
    }
}
