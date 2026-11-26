// Opt-in builtin loop — the diagnostics-on-write loop shape (omp's
// diagnosticsOnWrite), as a plain llm → tools cycle.
// Known limitation: it carries NO write-time verification. A script loop
// does not inherit the builtin loop's LSP flush, and a script-authored
// `run_tools` call cannot put a result in front of the model — no
// `tool_use` precedes it, so the wire sanitizer drops the orphan result.
// (The first version polled the `diagnostics` tool after every edit batch:
// an environment probe the model never saw, left unpaired in the
// transcript, and blocked by the loop guard from the third batch.)
// Write-time feedback lives in the builtin executor (`turn_executor =
// "builtin"`), whose LSP hook rides the turn.
// Enable (all three keys): [mods] builtin = ["verify-loop"],
// turn_executor = "rhai", [rhai_loop] mod = "verify-loop".

register_loop("main");

fn main() {
    // Locals, not top-level consts: the loop engine calls `main` with a
    // fresh scope, where top-level constants are not visible.
    let max_steps = 24;
    let step = 0;
    while step < max_steps && !cancelled() {
        compact_now();
        let round = llm_request();
        if round.interrupted {
            return;
        }
        if round.tool_uses.len() == 0 {
            return;
        }
        let out = run_tools(round.tool_uses);
        if out.halt != () {
            return;
        }
        step += 1;
    }
}
