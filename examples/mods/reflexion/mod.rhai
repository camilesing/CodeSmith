// Reflexion (Shinn et al., 2023) as a runtime-swappable Rhai mod.
//
// The loop itself stays the host's; this mod owns only the *policy*: what
// counts as a failure, how long a lesson survives, and where it is
// re-injected. Failure signals observed on the MAIN turn only
// (`ctx.agent.main` filters out sub-agent turns):
//
//   tool-result (is_error)        — a failing tool call
//   result-verified (verify-fail) — the result-claim verifier disproved a
//                                   "tests pass" style claim
//
// Lessons accumulate in the mod's persistent KV (`reflections`), ride the
// next attempt as one injected user message (the `before-agent-start`
// waterfall — the host builds the message, so no wire-shape risk), and are
// cleared by the first clean turn (no-tool-calls end, zero errors in the
// turn). `dirty` gates injection to the first attempt after new failures.
//
// Known limitation (deliberate): reflection text is the error excerpt
// itself, not a model-generated self-critique — a script cannot call the
// model. The fact ledger in the compaction summary remains the place where
// the model's own lessons live.

// Rhai note: a named top-level fn cannot see top-level `const`s (its call
// state is fresh); consts ARE visible inside event callbacks (closures
// capture them). Everything a named fn needs is passed in as a parameter.
const MAX_REFLECTIONS = 10;
const MAX_EXCERPT_CHARS = 300;

fn head(text, n) {
    if text.len() <= n { text } else { text.sub_string(0, n) + "…" }
}

fn record(lesson, max) {
    let r = mod_state_get("reflections") ?? [];
    // Dedup: the same failing excerpt twice adds noise, not signal.
    if r.index_of(lesson) < 0 {
        r.push(lesson);
        while r.len() > max { r.remove(0); }   // keep the newest
    }
    mod_state_set("reflections", r);
    mod_state_set("dirty", true);
}

on("tool-result", |e, ctx| {
    if !ctx.agent.main || !e.is_error { return (); }
    mod_state_set("errors_this_turn", (mod_state_get("errors_this_turn") ?? 0) + 1);
    record(
        "[" + e.name + "] failed: " + head(e.content, MAX_EXCERPT_CHARS),
        MAX_REFLECTIONS,
    );
    ()
});

on("result-verified", |e, ctx| {
    if !ctx.agent.main || e.verdict != "verified-fail" { return (); }
    mod_state_set("errors_this_turn", (mod_state_get("errors_this_turn") ?? 0) + 1);
    let cmd = e.command ?? "(no command re-run)";
    record("claim \"" + e.claim + "\" was disproved by re-running: " + cmd);
    ()
});

on("turn-start", |e, ctx| {
    if !ctx.agent.main { return (); }
    mod_state_set("errors_this_turn", 0);
    ()
});

on("turn-end", |e, ctx| {
    if !ctx.agent.main { return (); }
    let errors = mod_state_get("errors_this_turn") ?? 0;
    if e.reason == "no-tool-calls" && errors == 0 {
        // Clean turn — the attempt succeeded; lessons expire (Reflexion).
        mod_state_set("reflections", []);
    }
    ()
});

on("before-agent-start", |e, ctx| {
    if !ctx.agent.main { return (); }
    let dirty = mod_state_get("dirty") ?? false;
    let r = mod_state_get("reflections") ?? [];
    if !dirty || r.is_empty() { return (); }

    let text = "### Reflections from earlier attempts\n" +
        "Avoid repeating these failures:\n";
    for lesson in r {
        text += "- " + lesson + "\n";
    }
    mod_state_set("dirty", false);
    transform(#{inject_message: text})
});
