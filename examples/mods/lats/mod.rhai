// lats — Language Agent Tree Search (Zhou et al., 2023) as a loop
// script: best-first search over candidate approaches. The main turn
// searches; children (spawn_depth() > 0 — the session's loop runs in
// them too) are single-round evaluators, so the script branches on its
// own depth.
//
// Search shape per round: expand the best node (`llm_request` with a
// purpose-distinguished prompt), simulate each candidate on a child
// (`spawn_child` fan-out + `eval_child`), score the outcomes, then
// BACKTRACK — the frontier keeps parents and children pruned best-first
// to the beam.
//
// Knobs come from the `lats-policy` policy kind (builtin defaults
// width 3 / depth 2 / beam 2). Override with your own mod:
//   register_policy("lats-policy", #{width: 4, depth: 3, beam: 3});
//
// Style note: loop bodies live in named functions — Rhai's parser caps
// expression depth per body (default 32), and a dense doubly-nested
// block trips it long before the logic gets interesting.

register_loop("main");

// `"" + t` stringifies any Dynamic (unit -> "") — eval results hand the
// script Option-typed fields.
fn text_of(t) { "" + t }

// Extract a trailing "SCORE: N" from a child's reply; -1 when absent
// (unscored candidates sort last — never dropped silently).
fn score_of(t) {
    let s = text_of(t);
    let parts = s.split("SCORE:");
    if parts.len() < 2 { return -1; }
    let tail = parts[parts.len() - 1].trim();
    let digits = "";
    for ch in tail.chars() {
        if ch >= '0' && ch <= '9' { digits += ch; }
        else if digits.len() > 0 { break; }
    }
    if digits.len() == 0 { return -1; }
    if digits.parse_int().is_ok() { return digits.parse_int().unwrap(); }
    -1
}

fn best_index_of(frontier) {
    let bi = 0;
    for i in 1..frontier.len() {
        if frontier[i].score > frontier[bi].score { bi = i; }
    }
    bi
}

// Best-first prune to the beam (frontiers are small; max-scan is fine).
fn prune_to(frontier, beam) {
    let kept = [];
    let rest = frontier;
    while kept.len() < beam && rest.len() > 0 {
        let bi = best_index_of(rest);
        kept.push(rest[bi]);
        rest.remove(bi);
    }
    kept
}

// One round: expand the best node, simulate every candidate on a child,
// score the outcomes, prune to the beam. Returns the next frontier.
fn search_round(frontier, width, beam, round) {
    let node = frontier[best_index_of(frontier)];

    // Expand: `width` distinct refinements of the best approach.
    let x = llm_request(#{prompt:
        "Propose " + width + " distinct candidate refinements of this approach, one per line, no numbering:\n"
        + node.approach});
    let lines = x.text.split("\n");
    let batch = [];
    for line in lines {
        let t = line.trim();
        if t.len() > 0 && batch.len() < width { batch.push(t); }
    }

    // Simulate + score each candidate.
    for cand in batch {
        frontier = add_scored(frontier, cand);
    }

    // Backtrack: parents survive (the node stays), the beam prunes.
    let pruned = prune_to(frontier, beam);
    note("lats: round " + (round + 1) + " done, frontier " + pruned.len());
    pruned
}

fn add_scored(frontier, cand) {
    let sim = "Pursue exactly this approach and report the outcome. End with 'SCORE: N' (0-100) for how promising the outcome is.\n" + cand;
    let h = spawn_child(#{prompt: sim, type: "general"});
    let r = eval_child(h, 300000);
    let entry = #{approach: cand, result: text_of(r.final_text), score: score_of(r.final_text)};
    frontier.push(entry);
    frontier
}

fn main() {
    // Child role: one round against the assignment, then done — this
    // mod running in a child is the evaluator, not another search.
    if spawn_depth() > 0 {
        llm_request();
        return ();
    }

    let width = loop_policy_int("width", 3);
    let depth = loop_policy_int("depth", 2);
    let beam = loop_policy_int("beam", 2);
    note("lats: search start (width " + width + ", depth " + depth + ", beam " + beam + ")");

    // Seed the frontier with the model's opening read of the task.
    let seed = llm_request();
    let frontier = [#{approach: text_of(seed.text), result: "", score: -1}];

    for round in 0..depth {
        if cancelled() { return (); }
        frontier = search_round(frontier, width, beam, round);
    }

    // Answer from the best branch.
    let best = frontier[best_index_of(frontier)];
    let ask = "The best-scoring approach and its simulated outcome:\n" + best.result + "\nWrite the final answer accordingly.";
    llm_request(#{prompt: ask});
    note("lats: search done — best score " + best.score);
}
