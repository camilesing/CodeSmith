// Voyager (Wang et al., 2023) skill accumulation as a runtime-swappable
// Rhai mod — the capture half only. The skill LIBRARY is the host's
// existing skill system (`register_skill`, ≤ 16 per load); this mod feeds
// it: it watches the main turn's action sequence, and when a turn ends
// clean (no-tool-calls, zero errors) with at least MIN_SEQ actions, the
// sequence is distilled into a skill body and stored in the mod's
// persistent KV. A failing tool call discards the in-progress sequence
// (Voyager encodes SUCCESSFUL sub-task solutions only).
//
// At load, every stored skill is re-registered — so a captured flow
// becomes model-visible again after `/mods reload`. Newly captured skills
// within a running session wait for the next reload (registration changes
// land at the next load boundary, the same discipline as tool changes).
//
// Known limitation (deliberate): there is no embedding index and no
// verification of the captured flow — the description is the tool-name
// sequence, so retrieval is by catalog text like any other skill.

// Rhai note: a named top-level fn cannot see top-level `const`s (its call
// state is fresh); consts ARE visible inside event callbacks (closures
// capture them). Everything a named fn needs is passed in as a parameter.
const MIN_SEQ = 3;
const MAX_SKILLS = 16;        // register_skill cap per load
const MAX_STEP_CHARS = 200;

// Action-class tools only — reads/searches alone are not a reusable flow.
const ACTION_TOOLS = [
    "write_file", "edit_file", "apply_patch", "exec_shell",
    "exec_shell_wait", "exec_shell_interact", "run_tests", "worktree_enter",
];

fn is_action(name, tools) {
    tools.index_of(name) >= 0
}

fn step_line(step, max_chars) {
    let input = step.input.to_string();
    if input.len() > max_chars {
        input = input.sub_string(0, max_chars) + "…";
    }
    "- `" + step.name + "` " + input
}

on("tool-call", |e, ctx| {
    if !ctx.agent.main || !is_action(e.name, ACTION_TOOLS) { return (); }
    let seq = mod_state_get("seq") ?? [];
    seq.push(#{name: e.name, input: e.input});
    mod_state_set("seq", seq);
    ()
});

on("tool-result", |e, ctx| {
    if !ctx.agent.main { return (); }
    if e.is_error {
        mod_state_set("errors", (mod_state_get("errors") ?? 0) + 1);
        // A failed action also poisons the in-progress sequence.
        mod_state_set("seq", []);
    }
    ()
});

on("turn-end", |e, ctx| {
    if !ctx.agent.main { return (); }
    let errors = mod_state_get("errors") ?? 0;
    let seq = mod_state_get("seq") ?? [];
    if e.reason == "no-tool-calls" && errors == 0 && seq.len() >= MIN_SEQ {
        let skills = mod_state_get("skills") ?? [];
        if skills.len() < MAX_SKILLS {
            let names = "";
            for i in 0..seq.len() {
                if i > 0 { names += " -> "; }
                names += seq[i].name;
            }
            let body = "Repeated workflow that completed a turn cleanly:\n";
            for step in seq {
                body += step_line(step, MAX_STEP_CHARS) + "\n";
            }
            skills.push(#{
                name: "captured-flow-" + skills.len(),
                description: "Action sequence " + names +
                    " (auto-captured from a clean turn)",
                body: body,
            });
            mod_state_set("skills", skills);
        }
    }
    mod_state_set("seq", []);
    mod_state_set("errors", 0);
    ()
});

// Re-register the accumulated library at load.
let stored = mod_state_get("skills") ?? [];
for skill in stored {
    register_skill(#{
        name: skill.name,
        description: skill.description,
        body: skill.body,
    });
}
