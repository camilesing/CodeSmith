# CodeSmith Desktop

CodeSmith Desktop is a Tauri 2 app whose UI is a vendored copy of an upstream
open-source client (MIT; see `desktop/ui/vendor/THIRD_PARTY_NOTICES.md` for
the upstream license and provenance), mechanically rebranded and bridged onto
`codesmith serve --http`. It is what `--preset all` opens in place of the
terminal REPL (see [CLI.md](CLI.md) → "Desktop UI (preset `all`)").

## Architecture

```
Tauri App (desktop/)
├─ WebView (vendored client UI, 58-package closure)
│   ├─ our boot: the __CODESMITH_BOOT__ plugin graph + first-party brand row
│   └─ our connection carrier: unary rpc.call over HTTP; every stream rides
│        the vendored WebSocket mux (one socket per client)
│        POST /api/<endpoint> · ws /api/remote.mux ─► loopback bridge 127.0.0.1:1421
└─ Rust src-tauri: the client RPC bridge (the only protocol code we own)
    ├─ client RPC surface: session/* settings/* workspace/* commands/* …
    ├─ stream mux: open/item/end/cancel ↔ item/end/error per logical
    │   stream; the upgrade admits only the page origins
    ├─ event mapper: runtime SSE envelopes → client wire events
    │   (per-session monotone seq, strict assistant-stream revision +1,
    │    surfaceOp synthesis, numeric turn/step ordinals)
    ├─ $events waterfall: approval / user-question requests answered via
    │   POST /v1/approvals/{id} and /v1/user-input/{thread}/{input}
    └─ child process: `codesmith serve --http` (free port + fresh token)
```

- The frontend writes no new UI code beyond the boot assembly and the
  transport adapter; pixels come from the vendored bundles.
- Upward: client routes → runtime_api REST (`POST /v1/threads/{id}/queue`,
  `…/steer`, `…/interrupt`, `PATCH /v1/threads/{id}`, `GET /v1/models` …).
- Downward: the backend's SSE stream folds into client wire events and
  assistant-stream frames.
- Unimplemented endpoints answer `gateway/method-unavailable` (the client
  renders an error state; it never pretends success).

## Vendor pipeline and rebranding

`desktop/scripts/vendor_client.py` copies the built client closure from an
upstream checkout (default `/Users/camile/Work/TypeScript/deepseek-harness`,
override with `--upstream` or `CODESMITH_DESKTOP_UPSTREAM`) and then applies,
in order:

1. **VENDOR_PATCHES** — exact-string product-copy patches (fail loud if the
   upstream string disappeared).
2. **REWRITES** — a mechanical, order-dependent table applied to every
   vendored text file: package scope `@deepseek-ai/dsh-*` → `@codesmith/*`,
   bundler identifiers, globals `__DSH_*` → `__CODESMITH_*`, env `DSH_*` →
   `CODESMITH_*`, product prose, and the lowercase `dsh-` prefixes (CSS
   variables, storage keys, URL schemes) → `cds-`, a **length-preserving**
   rename so hard-coded offsets (`key.slice(21)`) stay valid. The table is
   recorded in `ui/vendor/VENDOR.json` (with the upstream commit and
   per-file hashes); re-running is a no-op.

`desktop/scripts/gen_boot_graph.mjs` then generates the boot graph
(`ui/src/boot-graph.generated.ts`) and stages the classic-script bundles
under `ui/public/plugins/`. `desktop/scripts/check_brand.py` enforces the
no-upstream-brand rule (exit 1 on any brand hit; only
`THIRD_PARTY_NOTICES.md` copyright lines and `VENDOR.json` provenance are
exempt). Run it after any vendor refresh:

```sh
python3 desktop/scripts/vendor_client.py && node desktop/scripts/gen_boot_graph.mjs
python3 desktop/scripts/check_brand.py
```

LICENSE texts are copied verbatim; the MIT notice and required attribution
live in `ui/vendor/THIRD_PARTY_NOTICES.md`.

## Build and run

Requirements: Rust (repo toolchain), Node + pnpm, and a built `codesmith`
binary (`cargo build -p codesmith-cli`).

```sh
# 1. vendor + boot graph (skip if ui/vendor is current)
python3 desktop/scripts/vendor_client.py && node desktop/scripts/gen_boot_graph.mjs
# 2. frontend
cd desktop/ui && pnpm install && pnpm dev        # vite on :1420
# 3. desktop shell (bridge on :1421, spawns codesmith serve --http)
CODESMITH_DESKTOP_CODESMITH=<repo>/target/debug/codesmith \
  cargo run --manifest-path desktop/src-tauri/Cargo.toml
# release bundle (what preset=all looks for)
cargo tauri build --manifest-path desktop/src-tauri/Cargo.toml
```

Env knobs: `CODESMITH_DESKTOP_CODESMITH` (backend binary),
`CODESMITH_DESKTOP_BRIDGE_PORT` (bridge port), `CODESMITH_DESKTOP_APP`
(where the CLI launches the app from). The browser-only preview
(`pnpm dev` without the shell) falls back to an offline stub transport with a
visible 离线演示模式 badge.

Tests: `cargo test --manifest-path desktop/src-tauri/Cargo.toml` — the mapper
(seq/revision invariants, frame shapes), the waterfall mappings, the journal
page contract, and the stream mux's frame contract and origin gate.

## CLI integration

`--preset all` (or `preset = "all"`) replaces the REPL entry with the desktop
launch (`crates/cli/src/desktop.rs`): the app is looked up in
`[desktop] app_path` → `$CODESMITH_HOME/desktop/codesmith-desktop` → the
platform install location; a missing app fails loud with build guidance;
`[desktop] enabled = false` opts out. The launched app receives
`CODESMITH_DESKTOP_CODESMITH` pointing at the launching binary.

## Known limitations

Known limitations of the current implementation:

- Vendored copies sync with upstream manually (`vendor_client.py` records the
  source commit); no live tracking.
- The RPC surface is a subset; unimplemented endpoints answer
  `gateway/method-unavailable` instead of pretending success. Session
  rename/fork/search, archive/unarchive, session pinning and the sidebar's
  manual order, the prompt queue, the skills catalog, the desktop settings
  document, schedules over `/v1/automations`, and the control projections
  ARE bridged.
- A prompt sent while a turn runs parks on the runtime thread's queue
  (`POST /v1/threads/{id}/queue`): the queue dock renders it from the
  `inbox` projection (the runtime's `queue.updated` event, pushed live), and
  queued prompts start FIFO, one per turn, as soon as the running turn ends
  (an interrupt keeps the queue). `session/updateQueue` bridges the dock's
  three actions: edit (text-only — the item's attachments are dropped, and
  the dock disables Edit on attachment rows), remove, and steer (pull the
  item into the running turn). Gaps: steering an attachment-carrying row is
  refused loudly (a steer is text-only); after a `serve` restart the queue
  is still there but waits for the next prompt or turn end (resume-idle);
  a queued turn takes the thread's defaults, not per-prompt overrides.
- Sidebar pins and the manual session order are desktop-shell state,
  persisted at `~/.codesmith/desktop/workspace.json` and never sent to the
  runtime: pinned sessions lead their section (most recently pinned
  first), manually ordered ones keep their relative order, and
  never-dragged sessions follow in runtime recency order. The
  slash-command catalog carries exactly `/compact`, bridged onto the
  runtime's manual compaction over `commands/execute`: an invocation must
  be argument- and attachment-free (upstream's own admission), the execute
  result only claims the runtime accepted the compaction — the compaction
  itself runs as an async turn and its transcript node (the mapped
  `context_compaction` system message) carries the outcome — and a busy
  thread surfaces the runtime's conflict text as the command's error
  notice. The REPL's other slash commands are REPL-local and stay out of
  the catalog, so their "/"-prefixed lines fall through and submit as
  ordinary prompts.
- Upstream-only concepts (accounts/balance, plugin market, jobs list,
  sidebar terminal/files/browser/preview, subagent views, goal bar, plan
  preview) are not simulated — their plugins stay out of the boot roster or
  answer empty states.
- Attachment upload IS bridged, end to end. The composer's uploads stage in
  the bridge (`fileUploads/upload`, and the raw-bytes
  `api/session/uploadFileBinary` route, carried to the bridge origin by the
  page's `__CODESMITH_FILE_UPLOAD__` hook), and `session/prompt` resolves
  the staged receipts into the runtime turn API's inline `attachments[]`.
  Images reach the model as native image blocks (magic-verified, dimensions
  parsed from the header); other files fold into the message through the
  REPL's own intake (inline text, or a durable store-path reference the
  model reads on demand). Settled messages keep their thumbnails/chips (the
  runtime records each attachment on the user item; `session/attachment`
  serves the bytes back from the durable store, so history renders across
  restarts). Finder/desktop drops into the composer ride the host-path
  bridge: with the native drag-drop enabled the DOM never sees a file drop
  (wry claims `performDragOperation`), so the window's DragDrop event
  describes each path to the page (`__codesmithHostDrop`), which replays it
  into the composer's own paste intake with `__CODESMITH_HOST_PATHS__`
  answering — non-image files and directories become `@path` reference
  chips, and an in-cap image (magic-sniffed png/jpeg/gif/webp) fetches its
  one-shot staged bytes (`api/host/dropBytes`) into the ordinary upload
  pipeline. Limits and gaps: 10MB per file, 8 per message, audio/video
  refused (at attach time, with a business error); receipts are in-memory —
  a desktop restart drops staged-not-yet-sent uploads, which must be
  attached again; a steer cannot carry attachments (the runtime's steer
  route is text-only and the bridge refuses loudly); no upload progress
  bar; no EXIF orientation or image normalization (bytes travel untouched);
  a dropped image past the caps, or whose bytes cannot travel, cites by
  path instead of uploading; a drop needs an open, idle composer and shows
  no drag-hover highlight; the paperclip picker and a real paste stay
  path-less and upload as before.
- Schedules: automations accept exactly two RRULE shapes (`FREQ=HOURLY` with
  INTERVAL/BYDAY, `FREQ=WEEKLY` with BYDAY/BYHOUR/BYMINUTE, evaluated in the
  machine's local zone). The task page's daily/weekly/whole-hour/cron shapes
  compile into them; one-shot timings, sub-hour intervals, foreign time
  zones, and other cron shapes fail loud at create/edit instead of silently
  rescheduling. "Ask the model" task creation opens a session, but the model
  has no schedule-registration tool — use the manual form.
- Tool-call output deltas are not streamed; tool nodes settle from their
  durable results. Tool names are normalized to the vendored cards'
  vocabulary at the wire (`exec_shell` → bash card, `read_file` → read card,
  …); the runtime record keeps CodeSmith's real names, and replayed tool
  calls lose their arguments (durable items carry result metadata only).
- User questions have no HTTP cancel route, so an unanswered question
  waterfall waits for the runtime's own deadline; question requests carry no
  tool call id, so the composer card renders without the countdown claim.
- The `$events` waterfall follows the upstream gateway contract: each stream
  gets its own `clientId`, still-pending requests are re-delivered to a newly
  connected stream, and a late or duplicate `$events/result` is an idempotent
  no-op (`delivered:false`, runtime 404 included). A delivered decision is
  followed by the request's cancel frame — that frame is what retires the
  answering card on every connected client. Detection does not depend
  on a client following the session: every prompt arms a per-session
  background watcher on the thread event stream that surfaces approvals and
  questions on its own, and a turn the queue starts re-arms it. A prompt
  that predates the watcher (foreign HTTP clients, pre-restart turns) still
  needs a follow stream to be surfaced.
- Desktop sessions inherit the config's top-level `allow_shell` (default
  `false`, GHSA-72w5-pf8h-xfp4): with it off the model has no shell tool and
  approval cards come from write tools (`write_file`/`edit_file`). The bridge
  never flips the security default; users opt in via config.
- Streams ride one WebSocket per client at `/api/remote.mux` (the vendored
  mux client's protocol; unary calls stay HTTP POSTs, as upstream's do), so a
  client holds one bridge connection however many streams it opens — the
  former fetch+NDJSON carrier pinned a connection per stream, filled
  Chromium's six-per-host pool, and stalled reloads. WebSockets bypass CORS,
  so the upgrade admits only the page origins (an originless local client is
  admitted, like the HTTP routes). The mux reads no uplink (no bridged
  endpoint takes client items; upstream's only one, the sidebar terminal, is
  not simulated) and sends no ping heartbeat (loopback: a vanished page
  closes its socket, which aborts every stream it carried).
- The composer's optimistic copy retires on the echoed `source.rpcId` (the
  client sends `requestId`). The runtime records the id on the queued item
  and on the settled user item (`metadata.rpc_id`), so the match is by
  identity: a queue steer or remove cannot shift it onto another message,
  and every client that follows the session sees the same stamp.
- The composer's model seat resolves from the session's `modelSelection`
  projection plus the shared catalog: an empty session opens its journal at
  cursor −1 (0-based seqs), projections ride a process-wide seq clock (the
  runtime journals no event for a model switch), and control frames list
  `changes` as `{key, val}` entries. Switching models in the seat persists the
  thread model and pushes a live projection frame; a model id the catalog
  lacks keeps its own id and reads as not routable.
- Offline desktop E2E rides loopback providers: the scripted mock for
  protocol timing, and a real model for generation — Qwen2.5-1.5B-Instruct
  imported into ollama 0.23.2 from the local HF cache (`ollama create` over
  the safetensors dir plus a hand-written template that renders `.Tools`; the
  stock pulled `qwen2` template has no tools block, so its endpoint answers
  400 on every turn). Real-model chat turns verify end to end (prompt →
  stream → settle → replay, every replayed instant matching the durable
  record) under two standing constraints: codesmith's ~50k-token first turn
  overflows the 32k context every on-disk local model declares (ollama
  truncates head-first; the model still answers from the surviving tail), and
  ollama returns an imported model's `<tool_call>` blocks as plain content —
  no structured `tool_calls` — while rejecting `Qwen3ForCausalLM` at import,
  so a real-model tool round (a live approval card over a real tool call)
  stays unverified.
- The rebrand rewrite is textual (see the audit note in `vendor_client.py`'s
  doc comment): a future upstream offset that depends on a brand string's
  length would not be caught by the script — `check_brand.py` catches the
  branding, review the offsets when bumping the upstream commit.
- Pixel parity with the upstream client is a manual, screenshot-based check
  (`desktop/screenshots/baseline/`), not automated. Our four baseline shots
  (empty/fixture × light/dark, 1200×800) are captured; the upstream comparison
  run is pending — the 2026-11-07 attempt was blocked by the Electron binary
  download, the unbuilt upstream host CLI, and this machine's redacted window
  capture (evidence in `desktop/screenshots/baseline/README.md`).
