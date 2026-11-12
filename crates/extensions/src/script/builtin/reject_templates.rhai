// Opt-in builtin tool hardening — critical-pattern rejection templates.
// Ported from oh-my-pi @33ea58aa (articles/CodingAgent/Compare/
// CodingTools.md / HarnessEngineering.md): a deny-only guard over the
// destructive shell patterns, where each denial carries the corrective
// next step (the harness feedback rule: an error code without a remedy
// gets retried). omp's full 21-pattern registry is trimmed to the
// destructive core; the approval ladder and sandbox stay the host's.
// Note: a tool replacement cannot wrap the incumbent's error path (the
// protocol is delegate-or-complete), so the templates ride the deny-only
// guard seam instead.
// Enable: [mods] builtin = ["reject-templates"].

register_guard(|e| {
    if e.name != "exec_shell" && e.name != "exec_shell_wait" {
        return ();
    }
    let cmd = e.input.get("command");
    if cmd.contains("rm -rf /") || cmd.contains("rm -rf /*") {
        return "refused: `rm -rf` at the filesystem root. NEVER retry verbatim. List the concrete files (`ls`/`glob`) and delete them individually, or use a trash-preserving command if the user asked for cleanup.";
    }
    if cmd.contains("git push --force") || cmd.contains("git push -f") {
        return "refused: force-push rewrites published history. NEVER retry verbatim. Confirm with the user first, or push a new commit reverting the change; if the branch is truly yours alone, ask the user to run it.";
    }
    if cmd.contains("git reset --hard") {
        return "refused: `git reset --hard` discards uncommitted work. NEVER retry verbatim. Stash or commit first (`git stash` / `git add -A && git commit`), or use `git restore <path>` on the specific files.";
    }
    ()
});
