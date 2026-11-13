// Opt-in builtin harness guard — runaway detection, minimax V1 style.
// Ported from a comparative study of popular coding agents
// (decision point 3): the runaway-guard's `exact_action_repeat` signal at
// remind-not-stop strength — three consecutive identical tool calls (same
// name + same input fingerprint) get a denial that names the loop and the
// way out.
//
// The streak is a message projection — a fold of the session log — NOT
// mod KV: per-session by construction (rebuilt from the transcript on
// reload/compaction), nothing written to disk on the tool-dispatch hot
// path, and full tool inputs never leave the session log. The fold
// assigns each tool_use id its streak position, so the guard only reads
// `pos[id]`: parallel identical calls in one batch block on the third,
// not the first.
//
// Does not do: sub-agent turns run on a hub-less history (VecChatHistory),
// so their ids never fold and the guard abstains there — sub-agent loop
// discipline is the host LoopGuard's. The study's other signals
// (same-error-family, abab cycles, polling, unchanged progress) need
// result-body awareness; unchanged-progress also needs file state the log
// does not carry and stays with the host LoopGuard either way.
// Enable: [mods] builtin = ["runaway-guard"].

fn trunc(s, n) {
    if s.len() <= n {
        return s;
    }
    let out = "";
    for ch in s {
        out += ch;
        if out.len() >= n {
            break;
        }
    }
    out
}

register_message_projection(
    "runaway",
    #{ seq: [], pos: #{}, ids: [], last_pos: 0 },
    |state, m| {
        for block in m.content {
            if block.type == "tool_use" {
                // Fingerprint: name + truncated input display. Only the
                // fold computes fingerprints (the guard reads positions by
                // id), so display stability has to hold within one fold
                // chain, not across script and host.
                let f = `${block.name}:${trunc(`${block.input}`, 256)}`;
                let last = if state.seq.len() > 0 {
                    state.seq[state.seq.len() - 1]
                } else {
                    ""
                };
                let n = if last == f { state.last_pos + 1 } else { 1 };
                state.pos[block.id] = n;
                state.last_pos = n;
                state.seq.push(f);
                if state.seq.len() > 12 {
                    state.seq.remove(0);
                }
                state.ids.push(block.id);
                if state.ids.len() > 32 {
                    let old = state.ids.remove(0);
                    state.pos.remove(old);
                }
            }
        }
        state
    },
);

on("tool-call", |e| {
    let s = projection_state("runaway");
    let n = s.pos[e.id] ?? 0;
    if n >= 3 {
        return block(`runaway-guard: this is call ${n} with the identical arguments — the result will not change. NEVER retry the same call again; change the approach (narrow the pattern, fix the path, or ask the user) or stop and report what blocked you.`);
    }
    proceed()
});
