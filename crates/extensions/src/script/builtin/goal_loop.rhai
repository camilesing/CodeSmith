// Opt-in builtin loop — the dsh goal state machine as a script shape.
// Ported from deepseek-harness @8bd07339 (articles/CodingAgent/Compare/
// HarnessEngineering.md decision point 2): a long-task progressor with
// an explicit phase (active → complete | blocked | paused) and a
// revision counter in the mod's KV. The objective is the transcript's
// first user message (the host already carries it); what the machine
// adds is the stopping discipline: a step budget, per-round context care
// (`compact_now()` before every request — pull compaction), and a
// blocked phase when three consecutive tool batches fail.
// Enable (all three keys): [mods] builtin = ["goal-loop"],
// turn_executor = "rhai", [rhai_loop] mod = "goal-loop".

register_loop("main");

fn set_phase(phase) {
    let rev = mod_state_get("goal_rev") ?? 0;
    mod_state_set("goal_rev", rev + 1);
    mod_state_set("phase", phase);
    note(`goal-loop phase: ${phase} (rev ${rev + 1})`);
}

fn main() {
    // Locals, not top-level consts: the loop engine calls `main` with a
    // fresh scope, where top-level constants are not visible.
    let max_steps = 24;
    let fail_streak_limit = 3;
    set_phase("active");
    let step = 0;
    let fail_streak = 0;
    while step < max_steps && !cancelled() {
        compact_now();
        let round = llm_request();
        if round.interrupted {
            set_phase("paused");
            return;
        }
        if round.tool_uses.len() == 0 {
            set_phase("complete");
            return;
        }
        let out = run_tools(round.tool_uses);
        if out.halt != () {
            set_phase("blocked");
            return;
        }
        if out.errors > 0 {
            fail_streak += 1;
        } else {
            fail_streak = 0;
        }
        if fail_streak >= fail_streak_limit {
            set_phase("blocked");
            note("goal-loop: three consecutive failing batches — pausing for a course change");
            return;
        }
        step += 1;
    }
    set_phase("paused");
}
