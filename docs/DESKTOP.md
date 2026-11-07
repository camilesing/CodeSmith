# CodeSmith Desktop

CodeSmith Desktop is a Tauri 2 app whose UI is a vendored copy of an upstream
open-source client (MIT; see `desktop/ui/vendor/THIRD_PARTY_NOTICES.md` for
the upstream license and provenance), mechanically rebranded and bridged onto
`codesmith serve --http`. It is what `--preset all` opens in place of the
terminal REPL (see [CLI.md](CLI.md) → "Desktop UI (preset `all`)").

Design spec: `docs/superpowers/specs/2026-11-06-codesmith-desktop-design.md`.

## Architecture

```
Tauri App (desktop/)
├─ WebView (vendored client UI, 58-package closure)
│   ├─ our boot: the __CODESMITH_BOOT__ plugin graph + first-party brand row
│   └─ our connection carrier: fetch+NDJSON transport (no WebSocket)
│        rpc.call / rpc.open ─► loopback bridge http://127.0.0.1:1421
└─ Rust src-tauri: the client RPC bridge (the only protocol code we own)
    ├─ client RPC surface: session/* settings/* workspace/* $events follow…
    ├─ event mapper: runtime SSE envelopes → client wire events
    │   (per-session monotone seq, strict assistant-stream revision +1,
    │    surfaceOp synthesis, numeric turn/step ordinals)
    ├─ $events waterfall: approval / user-question requests answered via
    │   POST /v1/approvals/{id} and /v1/user-input/{thread}/{input}
    └─ child process: `codesmith serve --http` (free port + fresh token)
```

- The frontend writes no new UI code beyond the boot assembly and the
  transport adapter; pixels come from the vendored bundles.
- Upward: client routes → runtime_api REST (`POST /v1/threads/{id}/turns`,
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
(seq/revision invariants, frame shapes), the waterfall mappings, and the
journal page contract.

## CLI integration

`--preset all` (or `preset = "all"`) replaces the REPL entry with the desktop
launch (`crates/cli/src/desktop.rs`): the app is looked up in
`[desktop] app_path` → `$CODESMITH_HOME/desktop/codesmith-desktop` → the
platform install location; a missing app fails loud with build guidance;
`[desktop] enabled = false` opts out. The launched app receives
`CODESMITH_DESKTOP_CODESMITH` pointing at the launching binary.

## Known limitations

Copied from the design spec §10, plus what the implementation added:

- Vendored copies sync with upstream manually (`vendor_client.py` records the
  source commit); no live tracking.
- The RPC surface is a subset; unimplemented endpoints answer
  `gateway/method-unavailable` instead of pretending success
  (`session/updateQueue`, session pinning, …). Session rename/fork/search,
  archive/unarchive, the skills catalog, the desktop settings document,
  schedules over `/v1/automations`, and the control projections ARE bridged.
- Upstream-only concepts (accounts/balance, plugin market, jobs list,
  attachment upload, sidebar terminal/files/browser/preview, subagent views,
  goal bar, plan preview) are not simulated — their plugins stay out of the
  boot roster or answer empty states.
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
- The composer's model seat resolves from the session's `modelSelection`
  projection plus the shared catalog: an empty session opens its journal at
  cursor −1 (0-based seqs), projections ride a process-wide seq clock (the
  runtime journals no event for a model switch), and control frames list
  `changes` as `{key, val}` entries. Switching models in the seat persists the
  thread model and pushes a live projection frame; a model id the catalog
  lacks keeps its own id and reads as not routable.
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
