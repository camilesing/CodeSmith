# RFC: Persistence SQLite Migration

**Issue:** #2189
**Status:** Design draft — pending review; no implementation commitment yet
**Date:** inventory 2026-08 · design draft 2026-08

## 1. Motivation

CodeSmith splits its persistence across seven stores with three different
backends (SQLite, per-record JSON, append-only JSONL). Section 1.1–1.7 below
is the verified inventory. The concrete costs:

1. **Listing is a full scan.** Every session/task/automation list operation
   reads and deserializes every file in a directory. With hundreds of
   sessions and thousands of runtime items this shows up in `/sessions`
   pickers, task views, and startup resume scans.
2. **Filtering is a full scan.** "Failed tasks in the last 7 days" or
   "threads touched since Monday" have no index path — every query is
   `read_dir` + `serde_json::from_str` + filter in Rust.
3. **No transactional consistency.** A crash between saving a runtime turn
   and its items leaves orphans that replay logic must tolerate. The same
   applies to task + queue updates. SQLite gives us single-statement
   atomicity and multi-row transactions for free.
4. **Event timeline replay is O(n).** `events/{thread_id}.jsonl` is
   append-only with fsync per event; resuming a runtime thread re-reads the
   whole file even when only the tail matters.
5. **Six schema-version constants, four reject-newer implementations.**
   Every store re-implements the same versioning policy with its own
   constant; `state.db` has none (implicit binary versioning).
6. **Two disjoint persistence stacks.** `crates/state` (SQLite) serves
   cli/app-server/core; the TUI's five managers never touch it. A schema
   change in one stack tells the other nothing.

**Decision already made (2026-08):** the default `state.db` path is
`~/.codesmith/state.db` (env override `CODESMITH_HOME`/`CODEWHALE_HOME`),
with read fallback to legacy `~/.deepseek/state.db` / `~/.codewhale/state.db`
until the modern path exists. Implemented in `crates/state/src/lib.rs`.

## 2. Target design (proposal for review)

### 2.1 One store, one version mechanism

Extend `crates/state` into the single `StateStore` for all product data:

- **Single database** at `~/.codesmith/state.db`, WAL mode, one writer.
- **`PRAGMA user_version`** as the one schema-version mechanism, with a
  small ordered-migration helper replacing the six per-manager constants.
  Reject-newer stays the policy for cross-binary safety.

### 2.2 New tables

| Table | Replaces | Notes |
|---|---|---|
| `tasks` (+ `task_queue` state) | `tasks/{id}.json`, `queue.json` | status/subject/created_at indexed |
| `automations` | `automations/{id}.json` | schedule + last-run indexed |
| `audit_events` | `audit.log` (JSONL) | append path preserved; queryable |
| `runtime_threads` / `runtime_turns` / `runtime_items` | per-record JSON files | turn+items written in one transaction (fixes orphans) |
| `runtime_events` | `events/{thread}.jsonl`, `state.json` | `(thread_id, seq)` primary key — tail reads replace full replay |
| `sessions` (+ `session_checkpoints`, `offline_queue`) | `sessions/{id}.json` — **open question, see §5** | largest payload, best debuggability as files today |

Records whose shape churns (task payloads, automation specs) are stored as
a JSON column under an indexed envelope (id, status, timestamps), so schema
migration stays cheap while queries hit real columns.

### 2.3 Concurrency model

`rusqlite` connections are not `Sync`. Keep the existing
`persistence_actor` single-writer pattern: one dedicated thread owns the
write connection; read-only connections (WAL) serve list/filter queries.
No async runtime dependency is introduced — `crates/state` stays sync.

### 2.4 Consumers

- `crates/tui` gains a dependency on `codesmith-state`; the five managers
  delegate to the store behind the actor.
- `crates/app-server` / `crates/core` / `crates/cli` keep using the same
  store; their existing tables are unaffected by phase ordering.

### 2.5 Import strategy

First run against an empty database performs a read-only import from the
legacy JSON/JSONL layout; original files are left in place (never deleted
by us). A `--reimport` escape hatch re-runs the import. Failures skip the
offending record with a warn, not a startup abort.

## 3. Migration plan (proposed order)

| Phase | Scope | Why this order |
|---|---|---|
| 0 | Path fix + this design draft | done |
| 1 | `PRAGMA user_version` + migration helper in `crates/state` | foundation, no behavior change |
| 2 | tasks + automations | simplest records, immediate list/filter win, lowest blast radius |
| 3 | audit log → `audit_events` | append-only semantics preserved, queryable audits |
| 4 | runtime threads/turns/items/events | biggest consistency + replay win; transactional turn+item writes |
| 5 | sessions (pending §5 decision) | largest payloads, resume-critical, done last |

Each phase is independently shippable and revertible (legacy files remain
the source of truth until its phase completes; the import in §2.5 makes
rollback a file-level operation).

## 4. Non-goals

- Any networked or shared-server database.
- Changing resume/fork semantics or session id formats.
- Migrating capacity-memory, telemetry jsonl, or secrets (different
  lifetimes, deliberately file-based).

## 5. Open questions (blocking phase 5, maybe phase 1)

1. **Sessions: migrate or index?** Full transcripts in SQLite are
   efficient but lose the "open the file to debug" property; the
   alternative is keeping `sessions/{id}.json` and adding a small
   `session_index` table (title, timestamps, tokens) for listing/filter.
   Leaning: index-only for sessions, full migration for everything else.
2. **Retention.** Should `runtime_events` get time/count-based GC once it
   lives in SQLite? JSONL grew unbounded too, but a queryable store makes
   pruning cheap and noticeable.
3. **One database or two?** Sharing `state.db` with app-server/core means
   one version surface; splitting (`state.db` + `tui.db`) isolates the
   TUI blast radius at the cost of a second version mechanism.
   Leaning: one database.
4. **Backup story.** Today "back up" = copy a directory. With SQLite we
   should document `VACUUM INTO` / `.backup` in OPERATIONS_RUNBOOK.md.

## 6. Current state (verified inventory, 2026-08)

Subsections keep their original numbering for cross-reference stability.

### 6.1 `crates/state` — partial SQLite (rusqlite)

**Backend**: SQLite via `rusqlite` (not sqlx).  
**Path**: `~/.codesmith/state.db`  
**Tables**: `threads`, `thread_dynamic_tools`, `messages`, `checkpoints`, `jobs`  
**Also**: `session_index.jsonl` — append-only JSONL for thread-name lookups.  
**Schema versioning**: none — table shape is versioned implicitly by the binary.

### 6.2 `crates/tui/src/session_manager.rs` — JSON sessions

**Backend**: individual JSON files + atomic writes via `write_atomic`.  
**Paths**:
- `~/.codesmith/sessions/{id}.json` (preferred, v0.8.44+) or `~/.codesmith/sessions/{id}.json` (fallback)
- `~/.codesmith/sessions/checkpoints/latest.json` — crash-recovery checkpoint
- `~/.codesmith/sessions/checkpoints/offline_queue.json` — offline/degraded-mode queue

**Schema constants**:
- `CURRENT_SESSION_SCHEMA_VERSION: u32 = 1` (`SavedSession`)
- `CURRENT_QUEUE_SCHEMA_VERSION: u32 = 1` (`OfflineQueueState`)

**Policy**: reject-newer — older binary will refuse to load data written by a newer version.

### 6.3 `crates/tui/src/runtime_threads.rs` — JSON runtime store

**Backend**: per-record JSON files + append-only JSONL for events.  
**Paths** (under `~/.codesmith/tasks/runtime/` or `CODESMITH_RUNTIME_DIR`):
- `threads/{id}.json`
- `turns/{id}.json`
- `items/{id}.json`
- `events/{thread_id}.jsonl` — append-only JSONL event timeline
- `state.json` — global monotonic sequence counter

**Schema constants**:
- `CURRENT_RUNTIME_SCHEMA_VERSION: u32 = 2`

**Policy**: reject-newer.

### 6.4 `crates/tui/src/task_manager.rs` — JSON task store

**Backend**: per-record JSON files + atomic writes.  
**Paths** (under `~/.codesmith/tasks/` or `CODESMITH_TASKS_DIR`):
- `{id}.json` — per-task records
- `queue.json` — queue state

**Schema constants**:
- `CURRENT_TASK_SCHEMA_VERSION: u32 = 2`

**Policy**: reject-newer.

### 6.5 `crates/tui/src/automation_manager.rs` — JSON automation store

**Backend**: per-record JSON files.  
**Paths** (under `~/.codesmith/automations/` or `CODESMITH_AUTOMATIONS_DIR`):
- `{id}.json`

**Schema constants**:
- `CURRENT_AUTOMATION_SCHEMA_VERSION: u32 = 1`

### 6.6 `crates/tui/src/audit.rs` — JSONL audit log

**Backend**: append-only JSONL with fsync after each event.  
**Path**: `~/.codesmith/audit.log`  
**Schema**: no version field — each line is a `{"ts", "event", "details"}` blob.

### 6.7 Summary of issues

| Area | Backend | Schema Version | Write Strategy | Queryability |
|------|---------|---------------|----------------|-------------|
| state (threads/messages/jobs) | SQLite | implicit | direct SQL | SQL |
| sessions | JSON files | v1 | atomic rename | file scan |
| runtime threads/turns/items | JSON files | v2 | atomic rename | file scan |
| runtime events | JSONL | v2 | append+fsync | linear scan |
| tasks | JSON files | v2 | atomic rename | file scan |
| automations | JSON files | v1 | atomic rename | file scan |
| audit | JSONL | none | append+fsync | linear scan |

**Key pain points**:
1. **Listing** threads/sessions/tasks requires scanning directories and deserializing every file.
2. **Filtering** (e.g., "all failed tasks in last 7 days") requires full scans.
3. **No transactional consistency** — a crash between saving a turn and its items can leave orphans.
4. **Event timeline growth** — JSONL append is O(n) for replay; no indexing.
5. **Six different schema version constants** across four modules, each with the same reject-newer policy.

