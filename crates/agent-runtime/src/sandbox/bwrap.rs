//! Bubblewrap (bwrap) passthrough for Linux sandbox (#2184).
//!
//! Bubblewrap is a setuid-less container runtime used by Flatpak and other
//! projects. It creates a new mount namespace with configurable bind mounts,
//! providing filesystem isolation without requiring root privileges.
//!
//! # How it works
//!
//! When `/usr/bin/bwrap` is present and either the legacy `prefer_bwrap = true`
//! key or `[sandbox] prefer_bwrap = true` is set, exec_shell commands are routed
//! through bwrap instead of relying solely on Landlock. The bwrap invocation
//! looks like:
//!
//! ```text
//! bwrap \
//!   --ro-bind / / \
//!   --dev /dev \
//!   --proc /proc \
//!   --dir /sys \
//!   --die-with-parent \
//!   --bind <cwd> <cwd> \
//!   --chdir <cwd> \
//!   --unshare-all \
//!   [--share-net] \
//!   -- <program> <args>
//! ```
//!
//! This creates a read-only view of the entire filesystem with write access
//! limited to the working directory. The `--dev`/`--proc`/`--dir /sys`
//! overrides are stacked after the root ro-bind (bwrap applies mounts in
//! argument order) so the sandbox gets a fresh minimal `/dev` tmpfs, a
//! `/proc` bound to the new PID namespace rather than the host's, and an
//! empty `/sys` — the host device tree and host PID/environ view stay
//! hidden. `--unshare-all` implies `--unshare-net`, so `--share-net` is
//! appended explicitly when the policy grants network access (Agent mode
//! grants it by default; without this flag bwrap would silently cut the
//! network the policy promised).
//!
//! # Important
//!
//! We do NOT vendor bwrap. The user must install it themselves:
//!
//! - Ubuntu/Debian: `apt install bubblewrap`
//! - Fedora: `dnf install bubblewrap`
//! - Arch: `pacman -S bubblewrap`
//!
//! If bwrap is not installed, we fall back to Landlock.

/// Canonical path to the bubblewrap binary.
#[cfg(target_os = "linux")]
pub const BWRAP_PATH: &str = "/usr/bin/bwrap";

/// Check if bubblewrap is installed and executable.
#[cfg(target_os = "linux")]
pub fn is_available() -> bool {
    std::path::Path::new(BWRAP_PATH).exists()
}

#[cfg(not(target_os = "linux"))]
pub fn is_available() -> bool {
    false
}

/// Build a bwrap command that wraps the given program and arguments.
///
/// The returned command vector is suitable for use as `ExecEnv.command` —
/// it replaces the normal program+args with a bwrap invocation that sets
/// up a read-only root filesystem with write access only to the specified
/// working directory.
///
/// # Arguments
///
/// - `cwd` — working directory that gets writable bind-mount
/// - `program` — the program to run inside the container
/// - `args` — arguments to pass to the program
///
/// # Returns
///
/// A `Vec<String>` representing the full bwrap invocation.
#[cfg(target_os = "linux")]
pub fn build_bwrap_command(
    cwd: &std::path::Path,
    program: &str,
    args: &[String],
    policy: &super::SandboxPolicy,
) -> Vec<String> {
    let writable_roots = policy.get_writable_roots(cwd);
    let mut cmd: Vec<String> = Vec::with_capacity(16 + args.len() + writable_roots.len() * 3);

    cmd.push(BWRAP_PATH.to_string());

    // Tear the sandbox down with us so orphaned children cannot outlive the
    // agent while still holding workspace write access.
    cmd.push("--die-with-parent".to_string());

    // Read-only bind-mount the entire root filesystem.
    cmd.push("--ro-bind".to_string());
    cmd.push("/".to_string());
    cmd.push("/".to_string());

    // Override the host's /dev, /proc and /sys that the root ro-bind just
    // pulled in. bwrap applies mounts in argument order, so these later
    // mounts win: /dev becomes a minimal tmpfs, /proc is a fresh instance
    // tied to the new PID namespace (no host process/environ view), and /sys
    // is an empty directory.
    cmd.push("--dev".to_string());
    cmd.push("/dev".to_string());
    cmd.push("--proc".to_string());
    cmd.push("/proc".to_string());
    cmd.push("--dir".to_string());
    cmd.push("/sys".to_string());

    // Re-bind only policy writable roots as read-write.
    for writable_root in writable_roots {
        let root = writable_root.root.to_string_lossy().to_string();
        cmd.push("--bind".to_string());
        cmd.push(root.clone());
        cmd.push(root);

        // Re-bind protected control-plane subpaths read-only after the parent
        // bind so bubblewrap's later mount wins for the narrower path.
        for read_only in writable_root.read_only_subpaths {
            let path = read_only.to_string_lossy().to_string();
            cmd.push("--ro-bind".to_string());
            cmd.push(path.clone());
            cmd.push(path);
        }
    }

    // Change to the working directory inside the container.
    let cwd_str = cwd.to_string_lossy().to_string();
    cmd.push("--chdir".to_string());
    cmd.push(cwd_str);

    // Unshare all namespaces for maximum isolation.
    cmd.push("--unshare-all".to_string());

    // --unshare-all implies --unshare-net; re-enable networking only when the
    // policy grants it (Agent mode grants network by default).
    if policy.has_network_access() {
        cmd.push("--share-net".to_string());
    }

    // Separator between bwrap args and the command to run.
    cmd.push("--".to_string());

    // The actual program and its arguments.
    cmd.push(program.to_string());
    cmd.extend(args.iter().cloned());

    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_available_does_not_panic() {
        let _ = is_available();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_build_bwrap_command_structure() {
        let cwd = std::path::Path::new("/home/user/project");
        let cmd = build_bwrap_command(
            cwd,
            "sh",
            &["-c".to_string(), "echo hi".to_string()],
            &super::SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![],
                network_access: false,
                exclude_tmpdir: true,
                exclude_slash_tmp: true,
            },
        );

        // Should start with bwrap
        assert_eq!(cmd[0], "/usr/bin/bwrap");

        // Should have ro-bind for root
        assert!(cmd.contains(&"--ro-bind".to_string()));

        // Host /dev, /proc and /sys must be overridden after the root bind.
        let ro_bind_root = cmd
            .windows(3)
            .position(|w| w == &["--ro-bind".to_string(), "/".to_string(), "/".to_string()])
            .expect("root ro-bind");
        let dev_at = cmd
            .windows(2)
            .position(|w| w == &["--dev".to_string(), "/dev".to_string()])
            .expect("--dev /dev");
        let proc_at = cmd
            .windows(2)
            .position(|w| w == &["--proc".to_string(), "/proc".to_string()])
            .expect("--proc /proc");
        let sys_at = cmd
            .windows(2)
            .position(|w| w == &["--dir".to_string(), "/sys".to_string()])
            .expect("--dir /sys");
        assert!(dev_at > ro_bind_root);
        assert!(proc_at > ro_bind_root);
        assert!(sys_at > ro_bind_root);

        // Die with the parent so sandboxed children cannot outlive us.
        assert!(cmd.contains(&"--die-with-parent".to_string()));

        // No network grant in this policy: --unshare-all must stand alone.
        assert!(!cmd.contains(&"--share-net".to_string()));

        // Should have --chdir
        assert!(cmd.contains(&"--chdir".to_string()));

        // Should end with the command
        assert_eq!(cmd[cmd.len() - 1], "echo hi");
        assert_eq!(cmd[cmd.len() - 2], "-c");
        assert_eq!(cmd[cmd.len() - 3], "sh");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_build_bwrap_command_share_net_with_network_policy() {
        let cwd = std::path::Path::new("/home/user/project");
        let cmd = build_bwrap_command(
            cwd,
            "sh",
            &["-c".to_string(), "curl example.com".to_string()],
            &super::SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![],
                network_access: true,
                exclude_tmpdir: true,
                exclude_slash_tmp: true,
            },
        );

        // Network-granting policies must get --share-net after --unshare-all.
        let unshare_at = cmd
            .iter()
            .position(|a| a == "--unshare-all")
            .expect("--unshare-all");
        let share_net_at = cmd
            .iter()
            .position(|a| a == "--share-net")
            .expect("--share-net for network policy");
        assert!(share_net_at > unshare_at);
    }
}
