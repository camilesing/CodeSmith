# The Harness: What CodeSmith Is and How It Works

A model answers a question. An agent finishes a task. The difference is
the harness — a system of rules, evidence, and feedback that keeps the
model oriented instead of drifting.

CodeSmith is that harness, built around DeepSeek V4 and guided by three ideas:

| Principle | How it works |
|---|---|
| **Start with trust** | Every turn begins with "A" — possibility before certainty, craft before convenience |
| **Clear jurisdiction** | A written Constitution with nine tiers of authority. User intent outranks stale instructions. Verification outranks confidence. |
| **Recursive improvement** | V4 helped write the harness. As the harness improves, V4 becomes more effective — and helps improve the harness further. Each turn starts stronger. |

It's open source, terminal-native, and packaged as a single `codesmith` Rust
binary.

## Jurisdiction: the Constitution

Agentic models deal with conflicting information at scale: user intent,
project rules, system defaults, tool output, and stale memory all compete
for authority in a single turn. LLM-as-a-judge needs jurisdiction — which
source wins when they disagree?

CodeSmith answers this with a **Constitution** (`prompts/base.md`). It's a
formal hierarchy of law — Article VII ranks nine sources from the
Constitution's own articles down to prior-session handoffs. The user's
current message outranks stale project instructions. Live tool output
outranks assumptions. Verification outranks confidence. The model inherits
a clear chain of authority every turn and never has to guess which
directive to follow.

Six articles (I-VI) sit above the hierarchy, defining the model's identity,
duties, and agency: a verification mandate (Article V — every action leaves
evidence, never declare success on faith), a coordination legacy (Article
VI — leave the workspace legible for the next intelligence), and a
primacy-of-truth clause (Article II — no lower rule may override it).

## Prefix caching makes it practical

DeepSeek V4's prefix caching makes this practical. The Constitution is long
and detailed, but once cached it costs roughly 100× less per turn than a
cold read. The model references it recursively — peeking, scanning, and
querying through RLM sessions — revisiting information on demand rather
than relying on a single memorized pass. It performs more like an
open-book test than a closed one.

## Failure is feedback

Because the authority structure is explicit, failure isn't hidden. Non-zero
exit codes, type errors from rust-analyzer arriving between turns, sandbox
denials — these are fed back as correction vectors. The model uses its own
drift to self-correct.

## Recording and replaying model calls

Set `CODESMITH_RECORD_LLM=<path.jsonl>` to wrap the resolved LLM client in a
recorder: every model call served through that client (including
seam and compaction calls that share it) appends one JSONL
line — the full request envelope exactly as sent
(after every extension transform: messages, system prompt, the complete
tool schema, sampling parameters) plus the streamed response events or the
non-streaming response. A cross-provider `[utility_model]` client gets its
own recorder on the same path (append mode; whole-line writes keep the
interleaved file parseable), so multi-model sessions record completely.
For the recorded session, any historical provider
request is a pure function of the log. A bad path aborts startup loudly.
REPL and ACP sessions get the main-turn half of this without opting in:
their session journal (`<id>.journal.jsonl`) records every main-turn
request envelope in order with the transcript, and
`session_journal::reconstruct_requests` rebuilds those requests from it.

`ReplayClient::load(path)` / `ReplayClient::load_with_mode(path, mode)`
(in `codesmith-agent`, `llm_client::record_replay`)
feeds the recorded events back through the real turn loop — keyless,
networkless regression tests. Two matching modes:

- `ReplayMode::Strict` (default): FIFO, one line per call — byte-
  deterministic replay for single-model sessions and regression pins.
- `ReplayMode::ByModel`: pops the first line whose recorded model matches
  the outgoing request's — recordings that mix main + utility models
  replay regardless of the nondeterministic interleaving between the two
  call sites.

Replay replays streams verbatim, including
a missing terminal `message_stop` (the engine treats that as a disconnect,
exactly like the original run). The engine test
`record_then_replay_round_trip` is the working example, and
`crates/cli/tests/fixtures/llm/` holds the committed golden suite driven
by the `golden_fixture_*` engine tests — record a real session, redact,
commit the JSONL, and extend `run_golden_fixture` calls to add a case.
Each case also compares the transcript the REPL would persist (the last
`SessionUpdated` snapshot) against `<fixture>.transcript.txt`: one line per
message with roles, block kinds, tool names/ids/inputs, result status, and
text prefixes, with `<turn_meta>` bodies and the temp workspace path
normalized. Run the suite with `CODESMITH_UPDATE_GOLDEN=1` to regenerate
the expected files after a deliberate behavior change, and review the diff
before committing. Fixtures contain full model I/O — never commit ones with
secrets.

### Provider debug flags (opt-in, off by default)

- `CODESMITH_DUMP_400_PAYLOAD=1` writes the full failed request transcript
  plus the provider error body to `/tmp/codesmith-400-dump-<pid>-<n>.json`
  (owner-only mode; redirect the directory with
  `CODESMITH_DUMP_400_PAYLOAD_DIR`). Dumps accumulate without a cap — clean
  them up manually.
- `CODESMITH_REASONING_PASSTHROUGH=1` makes the generic `openai` provider
  arm forward `reasoning_effort` verbatim for third-party
  OpenAI-compatible gateways (Zhipu GLM, …) that accept the field.

## Modes and sandboxing

Three modes control the action space: plan is read-only, agent gates
destructive operations behind approval, and yolo auto-approves in trusted
workspaces. The user-facing control surface follows the general agent's split: a
**permission preset** names the sandbox + approval bundle a session runs
under, and **plan** is a separate collaboration state rather than a third
preset. Two presets are selectable — `workspace-write` (agent mode) and
`danger-full-access` (yolo, keeping its trust + auto-approve posture) — and
`/permission` switches them by switching the session mode, which stays the
mechanism that picks the toolset and approval policy. Plan is enforced as
before (read-only tools, no writes) and projects as `read-only`, a derived
value outside the selectable table; a permission switch while plan is active
is refused rather than silently leaving it, and `/plan off` (or an approved
`exit_plan_mode`) restores the mode the session came from. `coordinator`
remains an experimental orchestrator-only mode behind the `coordinator_mode`
feature and projects as `custom` — it is not a permission either. A persisted
preset value from an older build (`agent`/`plan`/`yolo`/`coordinator`)
normalizes to the two presets on read.

OS-level sandboxing is enforced on macOS (Seatbelt) and on
Linux via bubblewrap when installed; the Landlock ruleset code is not
wired for child-process enforcement, and no Windows sandbox exists. See
[PRESETS.md](PRESETS.md) and [SANDBOX.md](SANDBOX.md).

Model auto-routing (`--model auto`, the default) is a per-turn classifier that
runs on the provider's strongest tier — a tiny thinking-off call picking the
heavy or light model and the thinking level for the turn. A free local
heuristic short-circuits obvious cases; `[auto] cost_saving = true` restores
the cheap-utility-model router. Fin — the fast light-tier path — still handles
quick tool work and background summaries.

Every turn records a side-git snapshot outside your repo's `.git`.
`/restore` and `revert_turn` roll back the workspace.

Sub-agents run concurrently (up to 20). `agent_open` returns immediately;
results arrive inline as completion sentinels with a summary. Full
transcripts stay behind bounded handles through `agent_eval`.

The rest of the surface: LSP diagnostics after every edit (rust-analyzer,
pyright, typescript-language-server, gopls, clangd, jdtls,
vue-language-server), RLM sessions for batched analysis, MCP protocol,
HTTP/SSE runtime API, persistent task queue, ACP adapter for Zed,
SWE-bench export, and live cost tracking with cache hit/miss breakdowns.

## Architecture in one paragraph

`codesmith` (single binary) → line-based interactive REPL ↔ async engine ↔
OpenAI-compatible streaming client. Tool calls
route through a typed registry (shell, file ops, git, web, sub-agents, MCP,
RLM) and results stream back into the transcript. The engine manages
session state, turn tracking, the durable task queue, and an LSP subsystem
that feeds post-edit diagnostics into the model's context before the next
reasoning step.

Full walkthrough: [ARCHITECTURE.md](ARCHITECTURE.md).

## Sub-agents: concurrent background execution

CodeSmith can dispatch multiple sub-agents that run in parallel — like a
concurrent task queue:

- **Non-blocking launch.** `agent_open` returns immediately. The child gets
  its own fresh context and tool registry and runs independently. The
  parent keeps working.
- **Background execution.** Sub-agents execute concurrently (default cap:
  10, configurable to 20). The engine manages the pool — no polling loop
  needed.
- **One turn, one fan-out.** Several spawn calls in a single assistant
  message start their children together: the dispatch batch is what makes
  the cap meaningful, so issuing them one per turn serializes the fan-out no
  matter what the cap says. Spawning asks for no approval (`agent_run`'s
  foreground form included) — a child inherits the parent's posture, and a
  non-auto parent's child runs `permission_mode: "ask"` for its own reads and
  writes.
- **Completion notification.** When a sub-agent finishes, the runtime
  injects a `<codesmith:subagent.done>` sentinel into the parent's
  transcript. The human-readable summary — including the child's findings,
  changed files, and any risks — sits on the line immediately before the
  sentinel. The parent model reads that summary and integrates findings
  without an extra tool call.
- **Bounded result retrieval.** The full child transcript lives behind a
  `transcript_handle` accessible through `agent_eval`. When the summary
  isn't enough, the parent calls `handle_read` for slices, line ranges, or
  JSONPath projections — keeping the parent context lean without losing
  access to the details.

Full sub-agent reference: [SUBAGENTS.md](SUBAGENTS.md).
