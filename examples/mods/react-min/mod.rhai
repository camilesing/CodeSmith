// react-min — the builtin loop's ReAct shape, written as a script.
//
// With `turn_executor = "rhai"` and this mod active, the turn runs this
// entry instead of the compiled loop. Every capability call executes the
// same host machinery the builtin loop runs: `llm_request()` streams,
// retries transparently and fires the request waterfalls; `run_tools()`
// gates through approval + sandbox + guards. The script owns only the
// SHAPE: llm → tools → repeat until the model stops calling tools.
//
// Capability surface (loop-engine globals):
//   llm_request()   -> #{text, tool_uses: [#{id, name, input}], stop_reason,
//                       usage: #{input_tokens, output_tokens}, interrupted}
//                       (steers queued mid-turn are drained into the
//                       transcript ahead of each request, like the builtin
//                       loop's top-of-step drain)
//   run_tools(calls)-> #{halt, plan_approved, errors}
//   cancelled()     -> bool
//   note(text)       record a script decision on the session log path

register_loop("main");

fn main() {
    note("react-min: turn start");
    while !cancelled() {
        let round = llm_request();
        if round.interrupted { return (); }
        if round.tool_uses.is_empty() { return (); }
        let batch = run_tools(round.tool_uses);
        if batch.halt != () {
            note("react-min: loop guard halt — " + batch.halt);
            return ();
        }
    }
    ()
}
