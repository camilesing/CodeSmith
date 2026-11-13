// Opt-in builtin harness guard — runaway detection, minimax V1 style.
// Ported from a comparative study of popular coding agents
// (decision point 3) at remind-not-stop strength. Five of the six study
// signals are folds over the session log:
//
//   exact_action_repeat — 3 consecutive identical tool calls (same name +
//                         same input fingerprint) get a denial naming the
//                         loop and the way out.
//   same-error-family   — 3 consecutive tool results with the same error
//                         signature; retrying the exact call that just
//                         failed the same way gets a denial naming the
//                         error and the fix.
//   abab cycles         — the last four calls alternate between two
//                         argument sets (A-B-A-B) on the same tool.
//   polling             — 6 consecutive calls to one tool name with
//                         changing arguments reads like waiting for an
//                         external change.
//
// unchanged-progress (the sixth) needs file state the log does not carry
// and stays with the host LoopGuard.
//
// The state is a message projection — a fold of the session log — NOT mod
// KV: per-session by construction (rebuilt from the transcript on
// reload/compaction), nothing written to disk on the tool-dispatch hot
// path, and full tool inputs never leave the session log. The fold
// assigns each tool_use id its streak positions; the guard only reads
// them by id, so parallel identical calls in one batch block on the
// third, not the first.
//
// Does not do: sub-agent turns run on a hub-less history (VecChatHistory),
// so their ids never fold and the guard abstains there — sub-agent loop
// discipline is the host LoopGuard's.
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
    #{
        seq: [],
        pos: #{},
        fp: #{},
        run: #{},
        ids: [],
        last_pos: 0,
        last_name: "",
        last_run: 0,
        err_sig: "",
        err_count: 0,
        err_fp: "",
    },
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
                let rn = if state.last_name == block.name {
                    state.last_run + 1
                } else {
                    1
                };
                state.run[block.id] = rn;
                state.last_name = block.name;
                state.last_run = rn;
                state.fp[block.id] = f;
                state.ids.push(block.id);
                if state.ids.len() > 32 {
                    let old = state.ids.remove(0);
                    state.pos.remove(old);
                    state.fp.remove(old);
                    state.run.remove(old);
                }
            } else if block.type == "tool_result" {
                let bad = block.is_error ?? false;
                if bad {
                    let sig = trunc(block.content, 128);
                    if sig == state.err_sig {
                        state.err_count += 1;
                    } else {
                        state.err_sig = sig;
                        state.err_count = 1;
                    }
                    state.err_fp = state.fp[block.tool_use_id] ?? "";
                } else {
                    // Any success resets the error family.
                    state.err_count = 0;
                    state.err_sig = "";
                }
            }
        }
        state
    },
);

on("tool-call", |e| {
    let s = projection_state("runaway");
    // exact_action_repeat
    let n = s.pos[e.id] ?? 0;
    if n >= 3 {
        return block(`runaway-guard: this is call ${n} with the identical arguments — the result will not change. NEVER retry the same call again; change the approach (narrow the pattern, fix the path, or ask the user) or stop and report what blocked you.`);
    }
    let myfp = s.fp[e.id] ?? "";
    if myfp != "" {
        // same-error-family: retrying the exact call that just failed the
        // same way three times.
        if s.err_count >= 3 && myfp == s.err_fp {
            return block(`runaway-guard: this exact call has just failed ${s.err_count} times with the same error — repeating it will not change the result. Read the error, fix the cause (path, argument, or permission), or ask the user for the missing piece.`);
        }
        // abab cycles: the last four calls alternate between two argument
        // sets, and this call is one of them.
        if s.seq.len() >= 4 {
            let n4 = s.seq.len();
            let a = s.seq[n4 - 4];
            let b = s.seq[n4 - 3];
            let c = s.seq[n4 - 2];
            let d = s.seq[n4 - 1];
            if a == c && b == d && a != b && d == myfp {
                return block(`runaway-guard: the last four calls alternate between two argument sets — an A-B-A-B loop on ${e.name}. Step back, pick one approach, or report what is blocking it.`);
            }
        }
    }
    // polling: same tool, changing arguments, no fixed target.
    let r = s.run[e.id] ?? 0;
    if r >= 6 {
        return block(`runaway-guard: ${r} consecutive calls to ${e.name} with changing arguments reads like polling for a change. State what you are waiting for, batch the work differently, or stop and report.`);
    }
    proceed()
});
