# Sandbox threat model

CodeSmith executes shell commands spawned by AI reasoning. The sandbox
module restricts what those commands can do to the host system. This
document describes what each platform's sandbox actually enforces,
what is best-effort, and what is explicitly out of scope.

## Platform overview

| Mechanism | Platform | Type | Status |
|---|---|---|---|
| Seatbelt | macOS | Mandatory access control | Enforced |
| Landlock | Linux | Filesystem access control | Enforced (kernel 5.13+, filesystem-only) |
| seccomp BPF | Linux | Syscall filter | Removed |
| Process hardening | Linux | Kernel prctl / rlimit | Present, not invoked |
| Bubblewrap (bwrap) | Linux | Namespace isolation | Enforced when installed |
| Windows sandbox | Windows | — | Not implemented (stub); fail-closed refusal |

## Threat model: what each layer addresses

### 1. Process hardening (Linux only)

**Status:** Implemented in `process_hardening.rs` but not wired into
startup — `apply_process_hardening()` has no call site, so the hardening
below is never applied to running processes.

**What the module does (when invoked):**

- `PR_SET_DUMPABLE=0` — prevents ptrace, makes `/proc/<pid>/` root-owned
- `PR_SET_NO_NEW_PRIVS=1` — irreversible; no child can ever gain privileges
- `RLIMIT_CORE=0` — no core dumps, so sensitive data never hits disk

**What it protects against:**
- Process inspection via ptrace/strace/gdb
- Privilege escalation via setuid/setgid/fscaps
- Core dumps leaking API keys, tokens, prompt content

**What it does NOT protect against:**
- A compromised child reading its parent's `/proc/<pid>/mem` (already blocked
  by `PR_SET_DUMPABLE=0` making `/proc/<pid>/` root-owned)
- Kernel exploits that bypass prctl

### 2. Landlock (Linux, kernel 5.13+)

**Status:** Enforced. `prepare_landlock` builds the ruleset in the parent
(policy translated into path-beneath rules) and carries it on the
`ExecEnv`; every `std::process::Command` spawn site applies it between
fork and exec (`install_sandbox_preexec`, a `pre_exec` closure running
`PR_SET_NO_NEW_PRIVS` + `landlock_restrict_self`). `enforces_isolation()`
returns true and `decide_sandbox` marks a Landlock selection enforcing.

**Rule translation:**
- `/` gets read + execute (every policy grants full-disk read; the child
  can exec its binary and the loader)
- `/dev/null` gets write (shell redirections survive)
- each policy writable root gets read + write
- Handles: `EXECUTE`, `READ_FILE`, `READ_DIR`, `WRITE_FILE`, `REMOVE_DIR`,
  `REMOVE_FILE`, `MAKE_DIR`, `MAKE_REG`, `MAKE_SYM`, `TRUNCATE`

**What it protects against:**
- Writing to system directories (`/usr`, `/bin`, `/lib`, `/etc`)
- Creating, deleting, or truncating files outside the writable roots
  (read-only policies have no writable roots at all)

**What it does NOT protect against (known limitations):**
- Network access — Landlock here handles filesystem rights only; a policy
  that denies network (e.g. `read-only`) still allows it under this
  backend. bwrap (selected automatically whenever installed) unshares the
  network namespace and enforces the denial.
- Read-only subpaths inside a writable root — Landlock rules only ever add
  access, so the protected control-plane paths (`.codesmith`, `AGENTS.md`,
  …) stay writable inside a workspace-writable root. Seatbelt and bwrap
  enforce them.
- PTY (`tty: true`) spawns — the PTY spawn path has no pre-exec hook, so
  a Landlock-confined TTY request is refused (`SANDBOX_UNAVAILABLE`) with
  the remedy (rerun without tty or install bubblewrap) instead of spawning
  unconfined.
- Process inspection
- Reading files that are already mapped (Landlock applies at `open()` time)

**Detection:** `detect_denial()` checks stderr for `Permission denied`,
`Operation not permitted`, `EACCES`, `EPERM`.

### 3. seccomp BPF (Linux only) — removed

The hand-rolled seccomp BPF filter module was deleted from the codebase:
it had zero call sites when removed (Landlock + bwrap are the actual Linux
defenses), and its hand-maintained syscall whitelist was x86_64-only. The
only surviving pieces are the SIGSYS / exit-code-31 detection heuristics
inside `landlock.rs`'s `detect_denial()` (see section 2).

### 4. Bubblewrap / bwrap (Linux, optional)

**When it runs:** Whenever `/usr/bin/bwrap` is present — bwrap wins the
Linux backend selection, because its namespace isolation additionally
denies network (Landlock here is filesystem-only) and keeps the protected
control-plane subpaths read-only inside writable roots, which Landlock's
rule-union semantics cannot express. The `prefer_bwrap` key (legacy
top-level and `[sandbox]`) is therefore deprecated and ignored. Runs as
an outer wrapper around the child command.

**What it does:**
- Creates a new mount namespace with `--unshare-all`
- Read-only bind-mounts the entire root filesystem
- Bind-mounts the workspace directory with read-write access
- Changes into the workspace with `--chdir`

**What it protects against:**
- Any filesystem write outside the workspace (enforced at the namespace
  level, not just filesystem access)
- Accidental modification of system files

**What it does NOT protect against:**
- Network access when the policy grants it — `--unshare-all` implies
  `--unshare-net`, and `--share-net` is appended explicitly only when the
  policy grants network, so network under bwrap is policy-gated
- Process inspection
- Memory attacks

**Installation:** User must install bubblewrap themselves:
- Ubuntu/Debian: `apt install bubblewrap`
- Fedora: `dnf install bubblewrap`
- Arch: `pacman -S bubblewrap`

CodeSmith does NOT vendor bwrap.

**Fallback:** If bwrap is not installed, Landlock (kernel 5.13+) is the
enforcing backend — filesystem-only, see its known limitations above. If
neither backend exists, the decision fails closed by default (see
[Fail-closed semantics](#fail-closed-semantics)); `[sandbox]
fail_if_unavailable = false` (or
`CODESMITH_SANDBOX_FAIL_IF_UNAVAILABLE=false`) is the explicit degraded
opt-out that runs the command unconfined.

### 5. Seatbelt (macOS)

**When it runs:** Applied via the `sandbox-exec` wrapper command. The
seatbelt profile is generated dynamically based on the `SandboxPolicy`.

**What it does:**
- Restricts filesystem access based on the policy profile
- Can restrict network access (when `network_access: false`)

**What it protects against:**
- Reading/writing files outside allowed paths
- Network connections (when configured)

**What it does NOT protect against:**
- Process inspection (Seatbelt does not block ptrace)
- Syscall-level attacks

**Detection:** `detect_denial()` checks stderr for `Operation not
permitted`, `sandbox-exec`, `deny(`, and `Sandbox: `. (`file-write` and
`network` appear only in the human-readable denial message, not in
detection.)

### 6. Windows — not implemented

CodeSmith does not advertise an in-process Windows sandbox. `windows.rs`
is a stub: `is_available()` returns false, and no Job Object, memory-cap,
or restricted-token code exists. **Windows sandbox policy is not
enforced**: a policy that demands confinement is decided
`SANDBOX_UNAVAILABLE` and — under the default fail-closed posture — the
command is refused (diagnosable, naming the alternatives) rather than
silently run unconfined. Escape hatches on Windows today: disable the
sandbox in config (`[sandbox] enabled = false`), set
`fail_if_unavailable = false` explicitly (degraded: commands run
unconfined), or run in an unconfined mode. Future Windows support must
run commands through a dedicated helper that provides process-tree
containment with a Job Object and `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`,
and must not claim filesystem, network, registry, or AppContainer
isolation until those guarantees are implemented and tested separately.

## Fail-closed semantics

A sandbox policy that demands confinement (`read-only`,
`workspace-write`) is a security claim, not a wish: when no backend on
the platform can enforce it, CodeSmith refuses to run the command. The
refusal is diagnosable — the reason carries the `SANDBOX_UNAVAILABLE`
marker, what is missing on the platform, and the alternatives (install an
enforcing backend; opt out explicitly via `[sandbox] enabled = false` or
`fail_if_unavailable = false`; or run in an unconfined mode). Explicit
configuration choices (`enabled = false`, `excluded_commands`) are
recorded on every result but never silently substitute an unconfined run
for an enforcing backend's failure to appear.

`fail_if_unavailable` defaults to `true`. Setting it to `false` restores
the old run-unconfined fallback — the refusal reason stays on the result
record (`sandbox_unavailable_reason`, `sandbox_effective = false`) so the
degraded run is never misreported as sandboxed.

## One-shot sandbox escalation goes through approval

A strictly wider one-shot sandbox policy (e.g. retrying a blocked command
under `danger-full-access`) only ever takes effect through the approval
gate: the host sends an `ApprovalDecision::RetryWithPolicy` carrying the
policy, and the turn loop re-dispatches the tool through the dispatcher's
`sandbox_override` wire-in. That wire-in rejects any policy that is not a
**strict widening** of the current baseline (`ReadOnly < WorkspaceWrite <
DangerFullAccess`; adding roots/network or clearing temp-dir exclusions
widens; nothing widens `DangerFullAccess`; `ExternalSandbox` is not
comparable), so a wider policy cannot arrive by any other path and an
"escalation" can never narrow.

## Defense in depth

On Linux both local backends enforce — bwrap (mount namespaces) whenever
installed, Landlock (filesystem rules, kernel 5.13+) otherwise. The
prctl/rlimit process-hardening module exists in the codebase but is not
wired into startup, and the seccomp filter was removed:

```
bwrap (namespaces + network + subpaths)  ← preferred, when installed
Landlock (filesystem-only)              ← enforcing fallback, kernel 5.13+
```

With neither backend available on Linux — or on Windows, where no helper
exists — `decide_sandbox` reports unavailable and the command is refused
under the default fail-closed posture. On macOS, Seatbelt is the
enforcing layer.

## Configuration

Relevant config keys in `~/.codesmith/config.toml`:

```toml
# Sandbox policy mode
sandbox_mode = "workspace-write"  # read-only | workspace-write | danger-full-access | external-sandbox

# Linux bubblewrap — DEPRECATED: bwrap is now selected whenever the
# `bubblewrap` package is installed; this key is parsed for compatibility
# and ignored.
# prefer_bwrap = false

# Structured runtime policy. These keys are optional and layer on top of
# the legacy flat keys above.
[sandbox]
enabled = true
fail_if_unavailable = true        # default: refuse when a sandbox-demanding policy cannot be enforced (SANDBOX_UNAVAILABLE); false = run unconfined (degraded)
enabled_platforms = ["macos", "linux"]
excluded_commands = []            # program names or command prefixes
auto_allow_bash_if_sandboxed = true
# prefer_bwrap — deprecated & ignored (see above)

[sandbox.filesystem]
mode = "workspace-write"          # read-only | workspace-write | danger-full-access | external-sandbox
writable_roots = []
allow_read = []
deny_read = []
allow_write = []
deny_write = []
exclude_tmpdir = true            # temp dirs are excluded by default; set to false to opt back in
exclude_slash_tmp = true

[sandbox.network]
enabled = true
allow_managed_domains_only = false # local OS sandboxes cannot enforce host allow-lists
allow = []
deny = []

# External sandbox backend
sandbox_backend = "none"          # "none" or "opensandbox"
sandbox_url = "http://localhost:8080"
sandbox_api_key = "YOUR_API_KEY"
```

Environment variable overrides:

- `CODESMITH_SANDBOX_MODE` → `sandbox_mode`
- `CODESMITH_PREFER_BWRAP=true` → `prefer_bwrap` (deprecated & ignored — bwrap
  is selected automatically whenever installed)
- `CODESMITH_SANDBOX_BACKEND` → `sandbox_backend`
- `CODESMITH_SANDBOX_URL` → `sandbox_url`
- `CODESMITH_SANDBOX_API_KEY` → `sandbox_api_key`
- `CODESMITH_SANDBOX_ENABLED=true|false` → `[sandbox].enabled`
- `CODESMITH_SANDBOX_FAIL_IF_UNAVAILABLE=true|false` → `[sandbox].fail_if_unavailable`
- `CODESMITH_SANDBOX_ENABLED_PLATFORMS=macos,linux` → `[sandbox].enabled_platforms`
- `CODESMITH_SANDBOX_EXCLUDED_COMMANDS=cmd1,cmd2` → `[sandbox].excluded_commands`
- `CODESMITH_AUTO_ALLOW_BASH_IF_SANDBOXED=true|false` → `[sandbox].auto_allow_bash_if_sandboxed`

## Detecting sandbox denials

When a command fails, the sandbox manager checks for denial patterns. Shell tool
metadata also reports whether sandboxing was requested and whether it was
actually effective (`sandbox_requested`, `sandbox_effective`,
`sandbox_backend`, `sandbox_unavailable_reason`, `sandbox_fallback_allowed`,
`sandbox_excluded_command`, and `sandbox_fail_closed`).

| Platform | Denial mechanism | Exit code | Stderr patterns |
|---|---|---|---|
| macOS Seatbelt | sandbox-exec violation | non-zero | `Operation not permitted`, `sandbox-exec`, `deny(`, `Sandbox: ` |
| Linux Landlock | EACCES / EPERM | non-zero | `Permission denied`, `Operation not permitted`, `EACCES`, `EPERM` |
| Linux SIGSYS heuristics | leftover seccomp-era detection (shared Linux path) | 31 | `Bad system call`, `bad system call`, `SIGSYS`, `seccomp` |
| Linux bwrap | Mount/namespace failure | non-zero | varies |
| Windows | Access denied / privilege | non-zero | `Access is denied`, `ERROR_PRIVILEGE_NOT_HELD` |

The `was_denied()` method on `SandboxManager` aggregates all platform-specific
checks. The `denial_message()` method returns a human-readable explanation.

## Limitations

### What the sandbox does NOT protect against

- **Network attacks** — macOS Seatbelt can block network locally, and Linux
  bwrap gates network on policy (`--unshare-net` unless the policy grants
  network); on Windows nothing is enforced.
  `[sandbox.network].allow_managed_domains_only = true` is therefore
  treated as deny-network for local OS sandboxes and is passed through to
  external sandbox requests for backend-side enforcement.
- **Git hook/fsmonitor execution** — CodeSmith injects empty `core.fsmonitor`
  and `core.hooksPath` values into shell git config when no explicit indexed
  `GIT_CONFIG_*` environment is already provided, preventing workspace git
  configuration from launching host-side helpers during sandboxed tool calls.
- **Memory attacks** — no platform prevents a child process from reading
  its own memory or exploiting memory corruption bugs
- **Timing side channels** — no local sandbox layer defends against
  timing-based information leaks
- **Resource exhaustion** — no local backend limits CPU, memory, file
  descriptors, or disk I/O
- **Kernel vulnerabilities** — if the kernel itself has a vulnerability,
  the sandbox cannot prevent exploitation (this applies to all platforms)
- **Supply chain** — if the child process downloads and executes untrusted
  code, the sandbox limits what that code can do, but does not prevent the
  download

### Platform-specific gaps

- **Linux:** without bubblewrap, Landlock enforces filesystem access only —
  network is unrestricted and protected control-plane subpaths stay
  writable inside writable roots; PTY spawns are refused rather than run
  unconfined. With neither backend, sandbox-demanding commands are refused
  (fail-closed default). Host allow-list network policy is not enforced
  locally.
- **macOS:** Seatbelt profiles are generated at runtime. A misconfigured
  profile could be too permissive.
- **Windows:** No sandbox is implemented; sandbox-demanding commands are
  refused under the default fail-closed posture (see section 6).

## Related

- `crates/cli/src/sandbox/` — host-side policy preparation, runtime wiring, external backend (`opensandbox.rs`)
- `crates/agent-runtime/src/sandbox/` — enforcement helpers (`seatbelt.rs`, `landlock.rs`, `windows.rs`, `bwrap.rs`, `process_hardening.rs`; re-exported by the `crates/cli/src/sandbox/` shim)
- `crates/config/src/lib.rs` — config keys
- `crates/tool-impls/src/tools/diagnostics.rs` — `diagnostics` tool reports
  `sandbox_available`, `sandbox_type`, `bwrap_available`, `cgroup_version`
- `config.example.toml` — annotated config reference
