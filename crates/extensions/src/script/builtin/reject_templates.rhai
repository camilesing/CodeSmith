// Opt-in builtin tool hardening — critical-pattern rejection templates.
// Ported from a comparative study of popular coding agents (harness
// feedback rules): a deny-only guard over the destructive shell patterns,
// where each denial carries the corrective next step (the harness feedback
// rule: an error code without a remedy gets retried). The set is omp's
// machine-destroying core — rm at the root, force-push, reset --hard,
// fork bomb, mkfs, raw-device dd/redirect, /dev/tcp reverse shells —
// every entry has no legitimate build idiom (curl|bash is deliberately
// absent: two of the studied agents demote it to approval for nvm/
// homebrew/rustup's sake; the approval ladder owns it here). The approval
// ladder and sandbox stay the host's.
// Note: a tool replacement cannot wrap the incumbent's error path (the
// protocol is delegate-or-complete), so the templates ride the deny-only
// guard seam instead.
// Enable: [mods] builtin = ["reject-templates"].

register_guard(|e| {
    // The tools that carry a command. `exec_shell_wait` takes a `task_id`
    // only — named here once, it read `command` as `()` and raised on every
    // poll (fail-open, with an error-level "never bound" log each time);
    // `task_shell_start` runs the same shell and was not covered at all.
    if e.name != "exec_shell" && e.name != "task_shell_start" {
        return ();
    }
    let cmd = e.input.get("command");
    // Nothing to inspect: abstain. A missing or non-string `command` is the
    // tool's error to raise, not a guard failure.
    if cmd.type_of() != "string" {
        return ();
    }
    if cmd.contains("rm -rf /") || cmd.contains("rm -rf /*") {
        return "refused: `rm -rf` at the filesystem root. NEVER retry verbatim. List the concrete files (`ls`/`glob`) and delete them individually, or use a trash-preserving command if the user asked for cleanup.";
    }
    if cmd.contains("git push --force") || cmd.contains("git push -f") {
        return "refused: force-push rewrites published history. NEVER retry verbatim. Confirm with the user first, or push a new commit reverting the change; if the branch is truly yours alone, ask the user to run it.";
    }
    if cmd.contains("git reset --hard") {
        return "refused: `git reset --hard` discards uncommitted work. NEVER retry verbatim. Stash or commit first (`git stash` / `git add -A && git commit`), or use `git restore <path>` on the specific files.";
    }
    // Fork bomb: `:(){ :|:& };:` and friends — no output, just death.
    if cmd.contains(":|:&") || cmd.contains(":(){") {
        return "refused: fork bomb. NEVER retry. State what you actually wanted (parallel work? use the task/background shell tools) and do that instead.";
    }
    // Filesystem formatting: no legitimate build touches mkfs.
    if cmd.contains("mkfs") {
        return "refused: `mkfs` destroys a filesystem. NEVER retry. If a disk image must be formatted, name the exact image file (not a device) and ask the user to confirm.";
    }
    // Raw-device writes via dd — of=/dev/null (the benchmark idiom) stays.
    if cmd.contains("dd ") && cmd.contains("of=/dev/") && !cmd.contains("of=/dev/null") {
        return "refused: `dd` onto a raw device destroys it. NEVER retry. Write to a file image instead, or ask the user to run the device write themselves.";
    }
    // Raw-device redirects: `> /dev/sda`, `> /dev/nvme0n1`, `> /dev/diskN`.
    if cmd.contains("> /dev/sd") || cmd.contains("> /dev/nvme") || cmd.contains("> /dev/disk") {
        return "refused: redirecting onto a raw device destroys it. NEVER retry. Redirect to a regular file instead.";
    }
    // bash /dev/tcp reverse shells: no legitimate build idiom opens one.
    if cmd.contains("/dev/tcp/") {
        return "refused: `/dev/tcp/` opens a raw network socket from the shell — the canonical reverse-shell payload. NEVER retry. Use `curl`/`fetch_url` for HTTP and `ssh` for shells.";
    }
    ()
});
