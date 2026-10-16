//! Linux Landlock sandbox implementation.
//!
//! Landlock is a security mechanism introduced in Linux kernel 5.13 that allows
//! processes to restrict their own access rights. Unlike Seatbelt on macOS which
//! uses an external sandbox-exec wrapper, Landlock applies restrictions directly
//! to the current process.
//!
//! # Requirements
//!
//! - Linux kernel 5.13 or later with Landlock enabled
//! - The kernel must be compiled with `CONFIG_SECURITY_LANDLOCK=y`
//!
//! # How it works
//!
//! 1. Create a landlock ruleset with desired restrictions
//! 2. Add rules to allow specific file paths
//! 3. Restrict the process using the ruleset
//!
//! Note: Once restricted, the process cannot gain more privileges.

// `unsafe` rationale: Landlock drives Linux kernel syscalls
// (`landlock_create_ruleset`, `landlock_add_rule`, `landlock_restrict_self`,
// `prctl`, `open`, `close`) via `libc`. Each `unsafe` block is documented at
// its site. Mirrors the `child_env` file-local allow pattern.
#![allow(unsafe_code)]

use super::SandboxPolicy;
use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};

/// Check if Landlock is available on this system.
pub fn is_available() -> bool {
    // Check if the landlock syscall is available
    #[cfg(target_os = "linux")]
    {
        // Try to create a minimal ruleset to test availability
        // Landlock ABI version check
        // Safety: syscall uses a null ruleset pointer for ABI probing and does not dereference it.
        unsafe {
            let result = libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<libc::c_void>(),
                0usize,
                LANDLOCK_CREATE_RULESET_VERSION,
            );
            result >= 0
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Get the Landlock ABI version supported by the kernel.
#[cfg(target_os = "linux")]
pub fn get_abi_version() -> Option<i32> {
    // Safety: syscall uses a null ruleset pointer for ABI probing and does not dereference it.
    unsafe {
        let result = libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        );
        if result >= 0 {
            i32::try_from(result).ok()
        } else {
            None
        }
    }
}

// Landlock syscall constants (not yet in libc crate). The access-right list
// mirrors the kernel uapi; rights not folded into the READ/WRITE combinations
// below stay as ABI reference (they are never granted by our rule
// translation — see `from_policy`).
#[cfg(target_os = "linux")]
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;

#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
#[cfg(target_os = "linux")]
#[allow(dead_code)]
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
#[cfg(target_os = "linux")]
#[allow(dead_code)]
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
#[cfg(target_os = "linux")]
#[allow(dead_code)]
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
#[cfg(target_os = "linux")]
#[allow(dead_code)]
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
#[cfg(target_os = "linux")]
#[allow(dead_code)]
const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;

// Combinations
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_READ: u64 = LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR;

#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_WRITE: u64 = LANDLOCK_ACCESS_FS_WRITE_FILE
    | LANDLOCK_ACCESS_FS_REMOVE_DIR
    | LANDLOCK_ACCESS_FS_REMOVE_FILE
    | LANDLOCK_ACCESS_FS_MAKE_DIR
    | LANDLOCK_ACCESS_FS_MAKE_REG
    | LANDLOCK_ACCESS_FS_MAKE_SYM
    | LANDLOCK_ACCESS_FS_TRUNCATE;

/// Landlock ruleset attribute structure
#[cfg(target_os = "linux")]
#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

/// Landlock path beneath attribute structure
#[cfg(target_os = "linux")]
#[repr(C)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// Rule type constants
#[cfg(target_os = "linux")]
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

/// A configured Landlock sandbox
///
/// Crate-private by design: `apply()` irreversibly restricts the calling
/// thread and everything it subsequently spawns, so the only legitimate
/// call site is between fork and exec — the `pre_exec` closure installed by
/// `install_sandbox_preexec` (shell_manager) and reached through the
/// `ExecEnv::landlock` handle built by `SandboxManager::prepare_landlock`.
/// The ruleset is created and fully populated with rules in the parent by
/// [`from_policy`](Self::from_policy); only `prctl` +
/// `landlock_restrict_self` run post-fork, both async-signal-safe raw
/// syscalls. Keeping the surface crate-private prevents a caller from
/// applying it to the long-lived agent runtime thread by mistake.
#[cfg(target_os = "linux")]
pub(crate) struct LandlockSandbox {
    /// Owned descriptor: `Drop` closes it on every path — including the
    /// `try_from` error branch in the constructor — instead of relying on
    /// a manual `Drop` impl plus ad-hoc closes.
    ruleset_fd: std::os::fd::OwnedFd,
}

#[cfg(target_os = "linux")]
impl LandlockSandbox {
    /// Create a new Landlock sandbox from policy, translating the policy
    /// into path-beneath rules:
    ///
    /// - `/` gets read + execute: every policy grants full-disk read, and
    ///   the child must be able to exec its own binary, the dynamic
    ///   loader, and read `/etc/ld.so.cache` (this rule is what keeps the
    ///   sandbox from bricking the process the old zero-rules draft would
    ///   have produced).
    /// - `/dev/null` gets write: shell redirections to `/dev/null` survive.
    /// - each of the policy's writable roots
    ///   ([`SandboxPolicy::get_writable_roots`], resolved against `cwd`)
    ///   gets read + write.
    ///
    /// # Known limitations
    ///
    /// - **Network access is not restricted.** This backend handles
    ///   filesystem rights only; a policy that denies network (e.g.
    ///   `ReadOnly`) still allows it under Landlock. bwrap — selected
    ///   automatically whenever installed — unshares the network
    ///   namespace and enforces the denial.
    /// - **Read-only subpaths inside a writable root are not expressible.**
    ///   Landlock path-beneath rules only ever ADD access (rule union), so
    ///   the protected control-plane subpaths
    ///   ([`WritableRoot::read_only_subpaths`]) stay writable inside a
    ///   workspace-writable root. Seatbelt and bwrap enforce them.
    /// - Paths that do not exist are skipped (skip-missing semantics in
    ///   `add_rule`); every other rule failure propagates, and the caller
    ///   (`SandboxManager::prepare`) turns it into a refusal to spawn.
    pub(crate) fn from_policy(policy: &SandboxPolicy, cwd: &Path) -> std::io::Result<Self> {
        // Determine what filesystem access to handle (restrict).
        // TRUNCATE (folded into LANDLOCK_ACCESS_FS_WRITE) requires ABI v3
        // (kernel 5.19+); on ABI v1/v2 kernels landlock_create_ruleset
        // returns EINVAL and the whole sandbox becomes unavailable. Mask
        // it off there — unhandled rights are simply unrestricted, which
        // is the documented best-effort approach.
        let mut handled_access =
            LANDLOCK_ACCESS_FS_EXECUTE | LANDLOCK_ACCESS_FS_READ | LANDLOCK_ACCESS_FS_WRITE;
        if get_abi_version().unwrap_or(0) < 3 {
            handled_access &= !LANDLOCK_ACCESS_FS_TRUNCATE;
        }

        let attr = LandlockRulesetAttr {
            handled_access_fs: handled_access,
        };

        // Create the ruleset
        // Safety: `attr` is a valid pointer for the syscall duration and size is correct.
        let ruleset_fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &raw const attr,
                std::mem::size_of::<LandlockRulesetAttr>(),
                0u32,
            )
        };

        if ruleset_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let ruleset_fd = i32::try_from(ruleset_fd).map_err(|e| {
            std::io::Error::other(format!(
                "Failed to create Landlock ruleset: file descriptor out of range: {e}"
            ))
        })?;

        let sandbox = Self {
            // Takes ownership: any error past this point still drops (and
            // closes) the descriptor.
            ruleset_fd: std::os::fd::OwnedFd::from_raw_fd(ruleset_fd),
        };

        // Full-disk read + execute for every policy, plus the write floor
        // every shell-style child needs.
        sandbox.allow_read(Path::new("/"))?;
        sandbox.allow_write(Path::new("/dev/null"))?;

        match policy {
            SandboxPolicy::ReadOnly => {}
            SandboxPolicy::WorkspaceWrite { .. } => {
                for root in policy.get_writable_roots(cwd) {
                    sandbox.allow_write(&root.root)?;
                }
            }
            // Never selected for confinement (`should_sandbox()` is
            // false); if a forced path reaches here anyway, keep the
            // read-only floor rather than granting writes no rule set
            // was derived for.
            SandboxPolicy::DangerFullAccess | SandboxPolicy::ExternalSandbox { .. } => {}
        }

        Ok(sandbox)
    }

    /// Add a read-only rule for a path
    pub(crate) fn allow_read(&self, path: &Path) -> std::io::Result<()> {
        self.add_rule(path, LANDLOCK_ACCESS_FS_READ | LANDLOCK_ACCESS_FS_EXECUTE)
    }

    /// Add a read-write rule for a path
    pub(crate) fn allow_write(&self, path: &Path) -> std::io::Result<()> {
        self.add_rule(
            path,
            LANDLOCK_ACCESS_FS_READ | LANDLOCK_ACCESS_FS_WRITE | LANDLOCK_ACCESS_FS_EXECUTE,
        )
    }

    /// Add a path rule to the ruleset
    fn add_rule(&self, path: &Path, access: u64) -> std::io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let path_cstr = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "Path contains NUL byte")
        })?;

        // Open the path to get a file descriptor
        // Safety: `path_cstr` is NUL-terminated and lives for the duration of the call.
        let fd = unsafe { libc::open(path_cstr.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };

        if fd < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::NotFound {
                // Path doesn't exist — documented skip-missing semantics.
                return Ok(());
            }
            // Every other failure (EACCES, ENOTDIR, ELOOP, …) must
            // propagate: silently dropping the rule would produce a
            // sandbox that DENIES access the policy explicitly granted,
            // with no diagnostic.
            return Err(err);
        }

        let attr = LandlockPathBeneathAttr {
            allowed_access: access,
            parent_fd: fd,
        };

        // Safety: `attr` is a valid pointer for the syscall duration.
        let result = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                self.ruleset_fd.as_raw_fd(),
                LANDLOCK_RULE_PATH_BENEATH,
                &raw const attr,
                0u32,
            )
        };

        // Safety: `fd` is a valid file descriptor from libc::open.
        unsafe {
            libc::close(fd);
        }

        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(())
    }

    /// Apply the sandbox to the current process
    ///
    /// WARNING: This is irreversible for the current process! Intended to
    /// run between fork and exec (or inside a `pre_exec` closure) — never
    /// on a long-lived runtime thread.
    pub(crate) fn apply(&self) -> std::io::Result<()> {
        // First, drop privileges using prctl
        // Safety: prctl call uses constant arguments and does not access memory.
        let result = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }

        // Now restrict the process
        // Safety: syscall uses a valid ruleset fd and no pointer arguments.
        let result = unsafe {
            libc::syscall(
                libc::SYS_landlock_restrict_self,
                self.ruleset_fd.as_raw_fd(),
                0u32,
            )
        };

        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(())
    }
}

#[cfg(target_os = "linux")]
/// `Drop` for the `OwnedFd` ruleset descriptor closes it on every path;
/// no manual impl is needed (and none could cover error returns inside
/// the constructor the way `OwnedFd` does).

/// Detect if a failure was caused by Landlock or seccomp denial.
///
/// Checks both Landlock-specific patterns (EACCES/EPERM) and seccomp-specific
/// patterns (Bad system call / SIGSYS). Seccomp violations are reported through
/// the same `was_denied` path so callers don't need to distinguish which layer
/// blocked the operation.
#[cfg(target_os = "linux")]
pub fn detect_denial(exit_code: i32, stderr: &str) -> bool {
    if exit_code == 0 {
        return false;
    }

    // Landlock denials typically result in EACCES or EPERM.
    let landlock_denial = stderr.contains("Permission denied")
        || stderr.contains("Operation not permitted")
        || stderr.contains("EACCES")
        || stderr.contains("EPERM");

    // Seccomp denials (#2182): SIGSYS — exit code 31, or 128+31=159 when a
    // shell parent reports the signal death, or "Bad system call".
    let seccomp_denial = exit_code == 31
        || exit_code == 128 + 31
        || stderr.contains("Bad system call")
        || stderr.contains("bad system call")
        || stderr.contains("SIGSYS")
        || stderr.contains("seccomp");

    landlock_denial || seccomp_denial
}

// Stub implementations for non-Linux platforms
#[cfg(not(target_os = "linux"))]
pub fn get_abi_version() -> Option<i32> {
    None
}

#[cfg(not(target_os = "linux"))]
pub fn detect_denial(_exit_code: i32, _stderr: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_available() {
        // This test will pass regardless of platform
        let _ = is_available();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_get_abi_version() {
        // May or may not be available depending on kernel
        let _ = get_abi_version();
    }

    #[test]
    fn test_detect_denial() {
        #[cfg(target_os = "linux")]
        {
            assert!(detect_denial(1, "Permission denied"));
            assert!(detect_denial(1, "Operation not permitted"));
            assert!(!detect_denial(0, "Success"));
            // SIGSYS death reported by a shell parent: 128 + 31.
            assert!(detect_denial(159, ""));
        }
    }

    /// Rule translation builds (and drops) a real ruleset on kernels that
    /// expose Landlock. Kernel-behavior enforcement (a confined child
    /// actually being denied) is NOT covered here — that evidence requires
    /// a Linux environment running confined children.
    #[test]
    #[cfg(target_os = "linux")]
    fn from_policy_builds_ruleset_when_landlock_available() {
        if !is_available() {
            // Kernel without Landlock: unavailability is the honest answer
            // and `decide_sandbox` refuses sandboxed policies.
            return;
        }
        let sandbox = LandlockSandbox::from_policy(&SandboxPolicy::ReadOnly, Path::new("/tmp"))
            .expect("ruleset construction succeeds on an available kernel");
        drop(sandbox);

        let workspace = LandlockSandbox::from_policy(
            &SandboxPolicy::workspace_with_roots(vec![PathBuf::from("/tmp")], false),
            Path::new("/tmp"),
        )
        .expect("workspace-write rule translation succeeds");
        drop(workspace);
    }
}
