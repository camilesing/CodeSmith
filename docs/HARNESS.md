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

It's open source, terminal-native, and packaged as a matched `codesmith` /
`codesmith-tui` Rust binary pair.

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

Seven articles sit above the hierarchy, defining the model's identity,
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

## Modes and sandboxing

Three modes control the action space. Plan is read-only. Agent gates
destructive operations behind approval. YOLO auto-approves in trusted
workspaces. OS-level sandboxing is enforced per platform: macOS Seatbelt,
Linux Landlock + seccomp (plus optional bubblewrap), and a Windows Job
Object v1. See [MODES.md](MODES.md) and [SANDBOX.md](SANDBOX.md).

Fin — a cheap Flash call with thinking off — handles model auto-routing per
turn. `--model auto` is the default.

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

`codesmith` (dispatcher CLI) → `codesmith-tui` (companion binary) → ratatui
interface ↔ async engine ↔ OpenAI-compatible streaming client. Tool calls
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
