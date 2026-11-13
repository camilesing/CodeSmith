// Opt-in builtin harness guard — runaway detection, minimax V1 style.
// Ported from a comparative study of popular coding agents
// (decision point 3): the runaway-guard's
// `exact_action_repeat` signal at remind-not-stop strength — three
// consecutive identical tool calls (same name + same input fingerprint)
// get a denial that names the loop and the way out. The other five
// signals (same-error-family, abab cycles, polling, unchanged progress)
// need result-body hashing across turns; that rides the host's LoopGuard
// today — extending the script surface to them is future work (noted in
// the IntegrationPlan).
// Enable: [mods] builtin = ["runaway-guard"].

on("tool-call", |e| {
    let repeat_limit = 3;
    // The map's Display form is the fingerprint (debug() prints maps
    // opaquely); same tool + same arguments = same string.
    let fingerprint = `${e.name}:${e.input}`;
    let last = mod_state_get("last_fingerprint") ?? "";
    if fingerprint == last {
        let n = mod_state_get("repeat_count") ?? 1;
        n += 1;
        if n >= repeat_limit {
            mod_state_set("repeat_count", n);
            return block(`runaway-guard: this is call ${n} with the identical arguments — the result will not change. NEVER retry the same call again; change the approach (narrow the pattern, fix the path, or ask the user) or stop and report what blocked you.`);
        }
        mod_state_set("repeat_count", n);
    } else {
        mod_state_set("last_fingerprint", fingerprint);
        mod_state_set("repeat_count", 1);
    }
    proceed()
});
