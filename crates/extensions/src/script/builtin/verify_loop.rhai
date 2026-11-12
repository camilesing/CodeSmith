// Opt-in builtin loop — write-time verification, omp diagnosticsOnWrite
// as a loop shape. Ported from oh-my-pi @33ea58aa (articles/CodingAgent/
// Compare/CodingTools.md decision point 4): after any batch that edited
// files (edit_file / write_file / apply_patch), the loop immediately
// runs the workspace `diagnostics` tool so millisecond-level feedback
// rides the same turn instead of waiting for the next model round.
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
        const EDIT_TOOLS = ["edit_file", "write_file", "apply_patch"];
        let edited = false;
        for c in round.tool_uses {
            if EDIT_TOOLS.contains(c.name) {
                edited = true;
            }
        }
        let out = run_tools(round.tool_uses);
        if out.halt != () {
            return;
        }
        if edited {
            // Write-time feedback: one diagnostics batch right after the
            // edit batch, before the next model round.
            let verify_call = #{
                id: "verify-after-edit",
                name: "diagnostics",
                input: #{},
            };
            let verify = run_tools([verify_call]);
            if verify.halt != () {
                return;
            }
        }
        step += 1;
    }
}
