---
name: ratchet
description: Autonomous keep-or-revert improvement loop over improve-from-traces — improve, run the agent, measure, keep wins, revert regressions, repeat. Use for long-running unattended improvement after the user explicitly confirms the configuration and auto-approve scope.
metadata:
  short-description: Keep-or-revert improvement loop
---

# Ratchet

One iteration: improve → run the agent → measure → KEEP (commit) or REVERT
(restore) → log → repeat. Only improvements survive. Runs the
`improve-from-traces` skill inside the loop; adapted from
kayba-ai/recursive-improve's ratchet (Apache-2.0), with git itself as the
loop engine.

## Step 1 — Configure (mandatory, never skip)

Ask the user to confirm every value, then write `eval/program.md`:

- Objective — what "better" means here.
- Agent run command — the command that produces fresh session records.
  Note that `codesmith exec` writes none; interactive sessions do.
- Sessions directory (default `~/.codesmith/sessions`).
- Metrics to optimize — name, minimize/maximize, weight, from
  `scripts/journal_metrics.py` (or the project's `eval/compute_baselines.py`).
- Stopping conditions — max iterations (default 20), max hours (default 8),
  plateau patience (default 3).

State plainly and get explicit consent before starting: inside the loop,
improve-from-traces Stage 5's human gate is auto-approved ([A]) and fixes
are committed on the ratchet branch without per-fix review. Normal tool
approvals still apply throughout — the user must run the session with an
approval mode matching that intent.

## Step 2 — Branch and baseline

Clean working tree required. Create `ri/ratchet-<YYYYMMDD-HHMMSS>`. Store
the baseline: `python3 scripts/journal_metrics.py --json eval/baseline.json`
(or the project's metrics command). Record the weighted score.

## Step 3 — Loop

Each iteration:

1. Read `eval/ratchet_log.jsonl` if present; do not repeat approaches that
   were reverted.
2. Run the full improve-from-traces pipeline with two modifications:
   auto-approve Stage 5 (write `Mode: ratchet` into the decision file), and
   apply fixes directly on the ratchet branch — no nested `ri/improve-*`
   branch. Skip insight work `eval/skillbook.json` already covers; metrics
   always re-run on fresh records.
3. Clear the new-record window and run the agent command. A failed run or
   no new records → log `skip`, continue.
4. Re-run the metrics command into a fresh JSON; compute the weighted score
   against `eval/program.md`.
5. Score improved → commit on the ratchet branch (stage only the fix
   paths, conventional message, never push) and update the baseline.
   Otherwise → `git restore` tracked changes, remove untracked fix files,
   increment the plateau counter.
6. Append to `eval/ratchet_log.jsonl`: iteration, score, keep/revert/skip,
   metrics, commit hash or null, duration.
7. Stop at max iterations, the time budget, or plateau patience.

## Step 4 — Summary

Write `eval/ratchet_summary.md`: a per-iteration table (score, verdict,
commit), the metric deltas against the baseline
(`journal_metrics.py --compare eval/baseline.json`), the kept commits, and
the review command `git diff <base>...ri/ratchet-*`. Tell the user how to
merge, discard, or re-run. Pushing and merging stay with the user.

## Rules

- Revert on regression, always — never keep a worse score.
- Never modify session records.
- Commit only on the ratchet branch.
- Log every iteration, including skips and reverts.
