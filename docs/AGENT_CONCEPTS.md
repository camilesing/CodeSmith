# General Agent Concept Map — the event-stream vocabulary, located

A widely-circulated article on general-purpose agent harnesses (written about
DeepSeekHarness, DSH) describes a harness as layers cut across one event
stream, each concept with its own job — not a pipeline:

> Source translates the external world into events; Thread groups events by
> clue; Eval makes an intake decision and Policy hands it the ruler; Ticket
> gives work an identity and Mandate gives it boundaries; Subscription lets
> context flow automatically; Assignment and Executor do the real execution;
> Channel carries results back to the crowd.

This map pins each concept to the code that already owns it here. It is a
shared vocabulary for design discussions and reviews — **not a renaming
program**. Five of the ten words already name different things in this
repository (`Thread*` in the protocol/runtime API, `Policy*` in five modules,
`Eval` in four senses, `*Executor*` throughout the engine, `Channel` in the
protocol), and importing the article's names would give us two vocabularies
for one codebase. Where a concept had no home, the gap is named below; the
only one closed so far is the Source frame catalog
(`crates/agent-runtime/src/engine/runtime_event.rs`).

The boundary rules that bound any future work on these seams live in
[ARCHITECTURE.md](ARCHITECTURE.md) (boundary note) and `AGENTS.md`: one loop,
one inbox, the seam triple, prefix-cache discipline.

## The map

| Concept | What owns it here |
|---|---|
| Source | ingress mechanisms + the runtime-event frame catalog |
| Thread | runtime-API threads + the session journal |
| Eval | intake decisions, in three places by design |
| Policy | the rulers those decisions consult |
| Ticket | named child sessions + team task entries |
| Mandate | the delegation trust envelope riding the child's tool surface |
| Subscription | partial: child→parent context patches; cross-session recall is open ground |
| Assignment | the sub-agent runtime and the team task flow |
| Executor | the turn-executor registry (strongest overlap) |
| Channel | the event channel, mailbox, runtime API, and desktop bridge |

### Source — the external world becomes events

There is no single Source abstraction, by accumulation rather than by
decision. Each ingress path already names itself:

- Host ops: `Op` (`crates/agent-runtime/src/ops.rs`) — the engine's ingress
  vocabulary; REPL input, runtime-API prompts, synthesized turns.
- Completions: `SubAgentCompletionSender`
  (`crates/agent-runtime/src/subagent.rs`) — sub-agent and shell-job
  completions, with the side-band idle-wake `Notify`.
- Steer: `rx_steer` (`crates/agent-runtime/src/engine/turn/seams.rs`).
- Team inbox: the file-based poller (`crates/cli/src/tools/team/inbox_poller.rs`)
  dispatching through `Op::TeamInboxDispatch`
  (`crates/agent-runtime/src/engine/team_inbox.rs`).
- MCP: the client pool (`crates/mcp-client`) surfaces as tools, not events.

What was missing — and is now closed — is the catalog of what an injected
*internal* input looks like: every engine-injected sentinel user message rides
the `<codesmith:runtime_event>` envelope, and the closed kind set plus the one
frame builder live in
`crates/agent-runtime/src/engine/runtime_event.rs` (`subagent_completion`,
`shell_job_completion`, `result_verification`, `plan_mode`,
`plan_mode_exit`, `deliverables_check`, `idle_wake`). `<teammate-message>` and
`<turn_meta>` are separate envelopes on purpose (see that module's known
limitations).

### Thread — events grouped by clue

Runtime-API threads (`Thread*` in `crates/protocol/src/lib.rs`,
`crates/cli/src/runtime_threads.rs`): one engine plus one per-thread JSONL
event stream per conversation — Codex-like lifecycle semantics, the durable
authoritative record. Note the word collision: these are *conversations*, not
the general agent's clue-grouped event threads; nothing here groups events by clue, and no
such grouping is planned. The durable per-session record underneath every
thread is the session journal (`crates/session/src/journal.rs`).

### Eval — the intake decision

Intake is decided at three gates, each with its own question:

- Turn intake: the inbox delivery rules
  (`crates/agent-runtime/src/engine/inbox.rs` — Target × Delivery) decide
  *when* parked input reaches the loop; the executor's pre-request drain
  delivers it.
- Per-tool intake for a child: `DelegationPolicy::admit`
  (`crates/cli/src/tools/subagent/mod.rs`) — allowlist, approval tier,
  terminal-takeover rejection, out-of-workspace path questions routed to the
  parent.
- Per-action intake for the host: the approval gate
  (`crates/agent-runtime/src/engine/turn/approval.rs`) and the sandbox
  execution policy (`crates/sandbox/src/execpolicy/`).

The model-facing `agent_eval` tool is a different sense of the word: it
fetches or waits on a child session's result, not an intake decision.

### Policy — the ruler

The rulers those Evals consult: `DelegationPolicy` (the trust envelope:
`allowed_tools`, `auto_approve`, path-ask routing), `PolicyStore`
(`crates/agent/src/policy.rs` — scriptable host-behavior knobs), the sandbox
execpolicy compiler, and the sandbox-mode policy
(`crates/agent-runtime/src/engine/sandbox_mode_policy.rs`).

### Ticket — work has an identity

Named child sessions via `agent_open` (`crates/cli/src/tools/subagent/`),
the `SubAgentAssignment { objective, role }` record
(`crates/agent-runtime/src/subagent.rs`), and team task entries
(`TaskAssignmentEntry`, `crates/cli/src/tools/team/inbox_poller.rs`).

### Mandate — work has boundaries

The boundary rides the child's tool surface, never the loop:
`DelegationPolicy` narrows the envelope (`allowed_tools`, approval ceiling,
path-ask), and `ChildTurnSpec` (`crates/agent-runtime/src/engine/child_turn.rs`)
carries it as plain data — messages are the assignment, tools are the narrowed
surface. "Delegation narrows the envelope, never the loop."

### Subscription — context flows automatically

Partial. What exists is push-at-assembly: the child→parent
`SubAgentCompletion.context_patch` (tighten-only), the fact ledger
(`crates/session/src/fact_ledger.rs`), and agent memory
(`crates/context/src/agent_memory/`) — all flowing context into the next
assembly without a model call. **Open ground:** the general agent's session-reference
(cross-session recall injected automatically) has no counterpart here. Any
future slice must keep the prefix-cache discipline: volatile facts enter as
user-role messages, never the frozen prefix.

### Assignment — work gets an owner

The sub-agent runtime: `SubAgentManager` / `run_subagent_task`
(`crates/cli/src/tools/subagent/mod.rs`) building the `ChildTurnSpec` and
running `run_child_turn`; plus the team task assignment flow
(`crates/cli/src/tools/team/`).

### Executor — the work runs

The strongest overlap, already adopted deliberately (founder decision
2026-10-30, the general-agent kernel-composition model): every turn — main and child
alike — resolves its factory from the process-shared `TurnExecutorRegistry`
(`crates/agent-runtime/src/engine/executor_composition.rs`), with
`HostAgentExecutor` as the production loop, the `rhai` registry entry for
script loops, `TurnLease` (`crates/agent-runtime/src/turn_lease.rs`) as
structural ownership, and `verify-single-turn-loop.py` guarding the
one-loop contract. See the boundary note in
[ARCHITECTURE.md](ARCHITECTURE.md).

### Channel — results reach people

The UI event channel (`crates/agent-runtime/src/events.rs`), the spawn-tree
`Mailbox` (`crates/agent-runtime/src/mailbox.rs`) and `ChildEventSink`, the
runtime API threads (HTTP), and the desktop app's loopback bridge
([DESKTOP.md](DESKTOP.md)).

## Deliberate non-adoptions

- **No article vocabulary as type names.** The mapping lives in this document
  only; `Thread`, `Policy`, `Eval`, `Executor`, `Channel` already mean
  something else here, and new code keeps repository names
  (`RuntimeEventKind`, `DelegationPolicy`, ...).
- **No second inbox or loop.** `engine/inbox.rs` is vocabulary over the
  existing mechanisms; a second queue beside them would be "a second inbox
  wearing a hat."
- **No addressable team/workflow inbox** (general-agent-style per-agent delivery) —
  future work on the inbox seam, not present vocabulary.
- **No new journal holes:** sub-agent transcripts stay unjournaled
  (documented limitation, `ARCHITECTURE.md`); injected frames are logged
  because the frame text is the record ("model-visible means logged").
