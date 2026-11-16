---
name: improve-from-traces
description: Analyzes CodeSmith session records for recurring failure patterns and turns them into a prioritized, human-approved fix plan on a dedicated branch. Use when the user asks to improve the agent from its traces, analyze past sessions, or run an improvement cycle.
metadata:
  short-description: Turn session records into reviewed fixes
---

# Improve From Traces

Agents improve by compounding: run, read the evidence, fix, repeat. This
skill runs one improvement cycle over CodeSmith session records — the
snapshots (`<id>.json`) and journals (`<id>.journal.jsonl`) under
`~/.codesmith/sessions/` (or `$CODESMITH_HOME/sessions`).

Methodology adapted from kayba-ai/recursive-improve (Apache-2.0); the trace
source is CodeSmith's own session records instead of captured API calls.

## Evidence and its blind spots

Filter sessions to the current project via `metadata.workspace`, prefer the
most recent (default 20). Messages are content-block arrays: `tool_use`
(`name`, `input`), `tool_result` (`is_error`, `tool_use_id`), `thinking`,
`text`.

State the blind spots in every report: `codesmith exec` runs and
runtime-API threads keep no journal; sub-agent transcripts are replay-level
records, not full request envelopes; compaction and utility LLM calls are
not journaled. Session records carry no ground-truth success flag —
outcomes are judged by reading the transcript.

## Stage 1 — Survey and deep-dive

1. Inventory the selected sessions: date, `mode`, `model`, message and
   tool-call counts, error count. Read 2–3 transcripts end to end first to
   calibrate before judging.
2. Survey every selected session in batches of ~3: what was asked, what
   the agent did, how it ended, which tool results were errors.
3. Pick 2–3 deep-dive targets: divergent outcomes on similar tasks; the
   longest traces with errors; the most common failure pattern; loops
   (a metrics script flags these — see Stage 3).
4. Deep-dive each target against the full transcript, two passes:
   - **Verify**: for each material claim, compare what the agent asserted
     against the `tool_result` it received. Incorrect claims are
     high-severity findings regardless of correct procedure.
   - **Root cause**: missing instruction, wrong instruction, tool or
     schema limitation, or reasoning failure?

## Stage 2 — Atomic insights

Turn each finding into one atomic, imperative insight — a command to the
agent, never an observation of it.

- Atomicity: start at 1.0, deduct 0.15 per "and/also/plus", 0.20 per vague
  term ("appropriate", "proper"), 0.05 per word over 15. Accept 0.95+,
  split at 0.70–0.94, discard below 0.40.
- Record per insight: evidence (session id, message index), severity
  (high/medium/low), category. The prompt surface for a CodeSmith project
  is `AGENTS.md`, workspace skills, and mods — `prompt_fix` targets those;
  `code_fix` targets product code.
- Reject meta-commentary ("be careful", "make sure"), observations, and
  unqualified "always/never".
- If `eval/skillbook.json` exists from a prior cycle, dedup: >70% semantic
  overlap means UPDATE the existing entry (bump its helpful/harmful tags),
  not ADD. Tagged harmful three times → REMOVE. Keep the skillbook under
  50 entries.

Write `eval/stage2_insights.md` with the insight table.

## Stage 3 — Baseline metrics

If a journal metrics script exists in this workspace
(`scripts/journal_metrics.py`), run it over the same session set and store
its JSON as the baseline. Otherwise generate `eval/compute_baselines.py`
for this project — every metric a numerator/denominator pair over session
records — and run it. Flag denominators under 5 as directional-only. Every
insight maps to a metric or is marked qualitative-only with a reason.

## Stage 4 — Triage and plan

Grep for existing coverage before planning a fix; fully covered with a
healthy baseline → discard with that reason. Then choose per insight:

| Signal | Fix |
|---|---|
| Agent had the data but reasoned wrong | prompt_fix (AGENTS.md / skill) |
| Scenario the instructions never cover | prompt_fix |
| Instruction exists, agent violates it | code_fix — enforce in code |
| Missing tool, schema, or validation | code_fix |
| New instruction plus supporting code | both, noting the dependency |

Priority = impact × confidence × (leading metric 1.5, else 1.0) ÷ risk
(none/low 1.0, medium 1.5, high 2.0). Confidence by sample size: n≥20 → 1.0,
10–19 → 0.8, 5–9 → 0.6, below 5 → 0.3. Write `eval/action_plan.md`: the
priority table, per-fix entries (file, exact change, linked metric), and a
"not fixing" section with every discard's reason.

## Stage 5 — Human gate (mandatory)

Present the executive summary, the top three changes, the full table, and
the "not fixing" section. Explain the branch workflow below, then stop for
the user's decision:

- **[A]** approve all
- **[B]** approve with modifications — walk each fix
- **[C]** reject with feedback

Never auto-approve; never proceed on silence.

## Stage 6 — Implement on a branch

`git status` must be clean (ask the user to commit or stash otherwise).
Create `ri/improve-<YYYYMMDD-HHMMSS>`, then apply approved fixes in
priority order — minimal, targeted changes only — appending each to
`eval/changes_log.md` (type, files, before/after, linked metric). Commit
on the improvement branch; staging only the paths the fixes touched. Stay
on the branch: do not push, merge, or switch back.
