# CodeSmith Roadmap

Living document. Status markers: `[x]` shipped, `[ ]` next, `[~]` underway.
Each item names its home (issue/plan/CHANGELOG) so this file stays a map,
not a second changelog.

## Continuous evolution loop — underway

**The gap this track closes:** CodeSmith *recorded* experience (Slop
Ledger, MEMORY.md, session transcripts) but nothing verified a claim,
extracted a lesson, or gated an update. Saving an experience is not
learning from it. The loop is being built in the order
verify → analyze → consolidate → (later) propose-and-gate, with the
safety contract below fixed from day one.

Source: the audit against the coding-agent field notes ("持续进化只有
记录、没有闭环", improvement plan P3-8).

- [x] **Result claim verifier** — a completed turn whose final assistant
  message claims "tests pass / build succeeds" triggers a re-run of the
  verification-class command the model itself executed that turn
  (approved-replay only); the four-element verdict is injected before the
  next request and surfaced as a toast. A claim with no command behind it
  reports `unsubstantiated` — the cheapest hallucination signal there is.
  `[verification] result_claims` (default on).
- [x] **Doctor LLM fallback layer** — after the deterministic checks
  collect warnings/errors, one advisory call (`[utility_model]` when
  configured) analyzes the findings for root causes the per-branch hints
  cannot see. Deterministic results stay authoritative; the model never
  executes anything. `[doctor] llm_fallback` (default on).
- [x] **Memory sleep learning** — `codesmith memory consolidate
  [--apply]`: deterministic index cleanup (duplicate/stale pointers,
  orphans reported, budget check) plus an LLM merge proposal accepted only
  if a validator confirms every topic file stays referenced exactly once.
  Dry-run by default; `--apply` writes with a `.bak` backup. Topic file
  contents are never touched.
- [ ] **Idle-time dream task** — the `DreamTask` skeleton
  (`BackgroundTaskType::Dream`, `Op::StartDreamTask`) is in place but
  unwired: no idle detection, no runner. When the sleep-learning pass
  proves itself as a manual command, promote it to an idle-time curator
  with the same validator + backup gating.
- [ ] **Semantic memory passes** — delete disproven entries, sink
  repeatedly-local rules into skills. Requires proposal/approval UI
  (machine proposes, human approves) before any automated write.

**Safety contract (applies to every item on this track, not negotiable):**
evidence is never an instruction (runtime events are marked internal);
the online execution loop only records — the offline loop rewrites, and
even it gates every write behind validation + backup; the approval gate,
validators, and release thresholds are never modified by the evolution
machinery itself.

## Layered evaluation — next

The unit tests + SWE-bench adapter + offline eval harness cover code
correctness; nothing measures the *evolution* machinery itself. The claim
verdicts, doctor analyses, and consolidation runs now emit structured
events — the observation objects exist. Next:

- [ ] Persist verdict/analysis events locally (jsonl, mirroring the
  opt-in telemetry sink) and add a readout (`/verify stats` or a
  `codesmith metrics` section): claim-match rate, unsubstantiated rate,
  verdict failure types.
- [ ] The four evolution metrics from the field notes — update-proposal
  acceptance rate, artifact activation rate, adherence success rate,
  retention-set gain — once the propose-and-gate half exists to measure.

## Not planned

- Auto-merging or auto-deleting memory *contents* without a validated
  proposal and a human gate.
- Any self-modification path for the approval gate or the validators.
