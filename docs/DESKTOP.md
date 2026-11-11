# CodeSmith Desktop

CodeSmith Desktop is a Tauri 2 app whose UI is a vendored copy of an upstream
open-source client (MIT; see `desktop/ui/vendor/THIRD_PARTY_NOTICES.md` for
the upstream license and provenance), mechanically rebranded and bridged onto
`codesmith serve --http`. It is what `--preset all` opens in place of the
terminal REPL (see [CLI.md](CLI.md) → "Desktop UI (preset `all`)").

## Architecture

```
Tauri App (desktop/)
├─ WebView (vendored client UI, 75-package closure)
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

`desktop/scripts/vendor_client.py` copies the built client closure from a
local upstream checkout (located via `--upstream` or the
`CODESMITH_DESKTOP_UPSTREAM` environment variable) and then applies,
in order:

1. **VENDOR_PATCHES** — exact-string product-copy patches (fail loud if the
   upstream string disappeared).
2. **REWRITES** — a mechanical, order-dependent table applied to every
   vendored text file: the upstream package scope → `@codesmith/*`, bundler
   identifiers, upstream globals → `__CODESMITH_*`, upstream env names →
   `CODESMITH_*`, product prose, and the lowercase upstream prefixes (CSS
   variables, storage keys, URL schemes) → `cds-`, a **length-preserving**
   rename so hard-coded offsets (`key.slice(21)`) stay valid. The table
   (exact source strings included) is recorded in `ui/vendor/VENDOR.json`
   (with the upstream commit and per-file hashes); re-running is a no-op.

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
# 1. vendor + boot graph. Re-run whenever either side drifts: ui/public/plugins
#    (the staged bundles) is gitignored, so a vendor tree can be current while
#    the staging is stale — a missing bundle 404s at boot and its plugins
#    silently never activate (the approval card among them).
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
visible 离线演示模式 badge. The stub serves the settings namespaces —
`ui-settings-general`, `locale`, `ui-settings`, `permission`, `ui-theme`,
`ui-conversation`, `ui-chat` — with in-memory writes and the bridge's schemas,
so toggles, steppers and menu pickers work and survive closing Settings. It
also serves the permission catalog (labels follow the in-memory locale),
`session/events`, and an empty `@` file source, so those pages render without
the shell.

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
  document (theme, fonts, enter behavior, transcript view, language,
  developer tools, the new-session permission default), schedules over
  `/v1/automations`, the control projections, the plan page
  (`session/events` + the plan body injected into `exit_plan_mode`), the
  permission surfaces (`permissionPresets/catalog` over the thread mode), the
  `@` file source (`fileReferences/list`), the Workspace registry +
  directory picker (`workspace/create|rename|delete|insertBefore`,
  `directoryPicker/list|createDirectory|pick`), the open-in-app application
  catalog (`open-in-app/apps|open`), per-message and session feedback
  (`messageFeedback/list|put|delete`, `sessionFeedback/record`), the GoalBar
  (`goals/*` over `/v1/threads/{id}/goal`), workspace path open/reveal
  (`session/openWorkspacePath|canOpenWorkspacePath|workspacePathApplications`),
  and the local settings-document action (`settings/openSettingsDocument`)
  ARE bridged.
- A prompt sent while a turn runs parks on the runtime thread's queue
  (`POST /v1/threads/{id}/queue`): the queue dock renders it from the
  `inbox` projection (the runtime's `queue.updated` event, pushed live), and
  queued prompts start FIFO, one per turn, as soon as the running turn ends
  (an interrupt keeps the queue). `session/updateQueue` bridges the dock's
  three actions: edit (text-only; the edit now KEEPS the item's attachments —
  the vendored dock still disables Edit on attachment rows, its own gate),
  remove, and steer (pull the item into the running turn, attachments
  included). A queue that outlives a `serve` restart resumes as soon as a
  client watches the thread's event stream (the runtime loads the engine and
  drains — a watcher can answer the turn's approvals); nothing fires
  unattended at boot. A queued turn still takes the thread's defaults, not
  per-prompt overrides.
- The composer's attachment presentation is the vendored attachment plugin
  (`client-ui-attachment`): the draft attachment rail (thumbnails, file
  cards, retry, remove), the full-viewport drop overlay, and the message
  image galleries, over the bridged upload pipeline. The overlay's own
  signal — document drag events — never fires under wry's native
  drag-drop, so the shell's DragDrop Enter/Leave drive synthetic
  `dragenter`/`dragleave` on the page (`__codesmithHostDragHover`); the
  drop itself still arrives through the host-path paste intake, which also
  clears the hover (wry emits no Leave after a Drop). Upload progress is
  structurally absent: the vendored upload runtime's custom-transport hook
  (which carries uploads to the bridge origin) drops the progress callback
  by contract — and loopback uploads settle in tens of milliseconds, so
  the bar would never visibly move.
- The jobs popover (`client-ui-jobs` + the job-controller remote) is bridged
  onto the runtime's background tasks: `job/list` serves the session's task
  roster as whole-set `rows` frames (2s poll while open), `job/follow`
  streams a task's timeline as the output ring (consecutive message deltas
  concatenate; every other entry is one log line — 1s poll, ending with the
  terminal `status` frame), and `job/kill` maps onto
  `POST /v1/tasks/{id}/cancel` (`requested` / `already-finished`;
  `job/not-found` for an unknown id). Status mapping: queued/running →
  running, completed, failed, canceled → killed. A task with no linked
  thread belongs to no session, so no roster lists it; there is no progress
  line or byte cap to report.
- The changes-review tab and changed-files card (`client-ui-deliverables`)
  are served from the thread's durable tool items: `write_file` and
  `edit_file` return a unified diff as the call's result, and `apply_patch`
  returns a JSON summary whose requested hunks the bridge recovers from the
  call's own `patch` argument (one region per file header), so all three
  list with counts and a comparison; its whole-file `changes` form lists
  paths without a comparison (the runtime keeps no pre-written content to
  diff against). A failed or denied file call changed nothing and is not
  listed. The mapper announces each turn that settled a file change with a
  `workspace/changes` journal event (live and on replay). The bridge serves
  `/api/changes.summary|diff|open` (+ `/api/present.host|open`) on the page's
  behalf — the page's fetch hook carries page-relative `/api/…` routes to
  the bridge origin. Repeated edits to one path in a turn fold into one row
  (counts sum; the comparison is the last call's hunks — the runtime keeps
  per-call diffs, not turn-start/end snapshots). `changes.open` resolves a
  listed relative path against the session workspace and opens/reveals it in
  the OS file manager (404 when the file is gone, 204 on success); the
  summary and comparison routes answer `cache-control: no-store`.
  `present.open` (the declared-file opener) answers the client's
  `nativeUnavailable` — CodeSmith has no declared-file registry. The
  card/review tab's open/reveal control is a slot contributor
  (`deliverables.file.actions` / `deliverables.review.file.actions`) filled
  by the vendored `client-ui-open-in-app`: `session/canOpenWorkspacePath`
  answers true and `session/workspacePathApplications` an empty catalogue,
  so the control falls back to Reveal; `session/openWorkspacePath`
  canonicalizes the path and only admits a target inside the bridge cwd, a
  registered workspace, or an imported thread workspace. The host also
  serves the open-in-app catalog and launch routes
  (`GET /open-in-app/apps`, `POST /open-in-app/open`): the catalog is the
  application ids the vendored control can label — the OS file manager
  (`finder` / `explorer` / `filemanager`) plus editors, IDEs, Git GUIs and
  terminals installed in their conventional locations (macOS the two
  `Applications` roots and the system Terminal; Linux/Windows PATH
  executables). Discovery resolves only launchers this host actually holds;
  launch appends the directory to the argv unless an argument carries the
  `{path}` token (`--cd={path}`, `--working-directory={path}`), runs the
  application detached with secret-looking environment variables scrubbed,
  watches the first moments for an immediate failure, and requires an
  existing absolute directory inside the same workspace fence. That is what
  enables the session header's “Open In…” control and the Workspace row's
  “Show in folder”. Icons ARE served on macOS: the vendored control's icon
  prefix is vendored as an absolute bridge URL (`<img>` loads bypass the
  transport's fetch hook, so a page-relative icon URL would 404 against the
  asset origin), and `GET /open-in-app/icon/<id>` answers the largest PNG
  entry of the bundle's icns — the principal `AppIcon.icns` first — as
  `image/png` with a day of cache and an mtime:size ETag (`If-None-Match`
  → 304; the route admits originless requests because an `<img>` load
  carries no Origin). Finder (no bundle), every Linux/Windows entry, and an
  icns holding only raw-ARGB entries answer 404, and the control renders
  its built-in fallback glyph; a bundle renamed or moved outside the
  conventional roots is not offered.
- Sidebar pins and the manual session order are desktop-shell state,
  persisted at `~/.codesmith/desktop/workspace.json` and never sent to the
  runtime: pinned sessions lead their section (most recently pinned
  first), manually ordered ones keep their relative order, and
  never-dragged sessions follow in runtime recency order. The host
  slash-command catalog carries `/compact` (bridged onto the runtime's
  manual compaction over `commands/execute`: an invocation must be
  argument- and attachment-free, the execute result only claims the
  runtime accepted the compaction — the compaction itself runs as an async
  turn and its transcript node carries the outcome, and a busy thread
  surfaces the runtime's conflict text as the command's error notice) plus
  the per-thread mode switches: `/mode <agent|plan|yolo|coordinator>`
  (argument demanded, misspellings fail loud with the valid spellings),
  `/plan [off]` and the `/yolo` shortcut, and `/permission
  <workspace-write|danger-full-access>` (argument demanded) — each a
  `PATCH /v1/threads/{id}` of the `mode` field whose next turn applies the
  mode's toolset and approval policy. A `/permission` switch while the
  thread is in `plan` is refused with text pointing at `/plan off`, so the
  preset picker cannot silently drop plan. The catalog also carries `/goal
  <objective> [budget: N]` and `/feedback <text>`, and the `/goal`, `/plan`,
  `/permission` and `/feedback` rows carry the vendored client's builtin
  definition ids (`@codesmith/command-goal`, `@codesmith/plan-mode`,
  `@codesmith/permission-presets`, `@codesmith/command-feedback`), so the
  client's localized spellings (`/目标`, `/计划`, `/权限`, `/反馈`) resolve
  to them. `/model` rides the vendored client's own contribution (a popup
  over `session/selectModel`, already bridged). The REPL's other slash
  commands are REPL-local and stay out of the catalog, so their
  "/"-prefixed lines fall through and submit as ordinary prompts.
- The Models settings page (`client-ui-settings-models`, bridged in
  `host::llm` / `host::credentials`): the bridge serves a settings namespace
  named exactly `llm-pi-ai` (the page's generic OpenAI-compatible editor
  keys its layout on that name — anything else renders a hint-only 'unknown'
  card). The value is `{providers: {<route>: {apiKeyEnv?, displayName?,
  api?, baseURL?, models: [...]}}}`, read from and written to config.toml
  (crates/config's `ProvidersToml` for the typed view, surgical `toml_edit`
  writes that preserve comments, unknown keys, and every other table): a
  route is a builtin provider slug or a `[[providers.custom]]` id;
  `apiKeyEnv` is the page's own `<ROUTE>_API_KEY` derivation (never
  persisted, recomputed on read; self-hosted runtimes sglang/vllm/ollama
  carry none, so their rows ask for no key, and a custom route carries it
  only when it actually has a credential — an inline `api_key` or a
  store/env secret — so a keyless gateway keeps the create card's
  provider-native-auth posture instead of showing the missing-key dot); a
  model row maps
  `id→model`, `name→display_model`, `contextWindow→context_window`,
  `input` naming `image→vision=true`. The schemastery envelope carries the
  `providers.<route>.api` protocol union the page probes — with exactly one
  choice, `openai-completions` (CodeSmith custom gateways ride the
  openai-compat backend) — while `models[].reasoningEfforts` is
  deliberately absent: CodeSmith configures no per-model reasoning levels,
  so the page hides that column instead of editing a field nothing reads.
  `llm/listConfigurableProviders` builds the directory from config (the 16
  builtins plus custom entries, customs flagged `declared: true`),
  `llm/listProviders` comes from the runtime's `/v1/models`, and
  `llm/discoverModels` answers a named route from the runtime catalog or
  reqwest-GETs `{base_url}/models` with the key (the one the form just
  typed, else the route's stored credential; OpenAI-compatible
  `{data: […]}`; its two-parameter remote packs its args object-keyed, so
  the draft rides under `request` — `{settingsNs, request: {provider?,
  baseURL?, apiKey?}}`). `credentials/describe|set|unset` map onto the
  keyring (`crates/secrets`; a reference's slot is the lowercase
  `codesmith login` provider slug), describe answering only
  `{configured, source?, writable}` — never the value — and set/unset
  pushing a `credentials/record-updated` event frame; `source` is `file`
  for a store-supplied value (the vendored page's spelling for "saved by
  this page's credential provider", whichever backend holds it) and `env`
  for a process-environment hit. Describe also probes config.toml for the
  route's inline `api_key` (a custom entry's own key, or the root key where
  the runtime accepts it) and reports it `{configured: true, source:
  "file", writable: false}`: `source: "file"` selects the page's
  saved-credential copy (leave the field blank to use the saved key)
  instead of its env-var fallback text, which would be false for a
  config-file key; `writable: false` keeps the page from offering a
  removal its store write could not honour. It is the runtime's first key
  source, and this page can neither replace nor clear it. The namespace
  revision counter lives in `desktop/settings.json`; a stale
  `expectedRevision` is refused as `settings/conflict`. The page's built-in
  welcome notice reads a `ui-settings-general` namespace, now served too
  (its default is the
  current copy version — the notice stays hidden; an upstream bump shows it
  once and its own dismiss persists). Known limitations: config.toml
  configures ONE model per provider — a write naming two or more rows is
  refused, not silently dropped; a model row's `maxTokens` has no config
  home, so a write carrying one is refused rather than silently dropped
  (CodeSmith derives the output cap from the model's context window) and
  `llm/discoverModels` does not advertise a listed `max_tokens` for the
  same reason; an inline `[providers.<route>] api_key` reads as configured
  (green dot, saved-credential copy) but the page cannot replace or remove
  it, and the runtime keeps using it — a key typed on the page for that
  route lands in the store yet the config key still wins; a key saved on
  the Models page lands in the
  credential store and is live for the backend: `codesmith serve` resolves
  provider keys from config.toml, then the process environment, then the
  store (builtins by provider slot, the active custom route by its
  `[[providers.custom]]` id) when it constructs an engine — a key saved
  after the backend started applies on restart, and an environment key
  still shadows a page-saved one (the page's credential view probes its
  store before the environment, so that route still reads as saved — the
  env var is what the runtime sends); provider edits are load-time state
  for the runtime, so a running `codesmith serve` needs a restart; a
  hand-declared custom route still needs a provider factory (`providers.toml`
  or an extension) before the runtime can serve it; the first-run onboarding
  dialog depends on a `llm-deepseek` namespace (not served) and never
  appears.
- The General settings document also serves `locale` (the Language row
  persists across reloads), `ui-settings` (developer tools; default on, which
  restores the Trajectory tab, the changed-files card and scripted HTML
  previews), and `permission` (the new-session default below). The General
  page's own sub-surfaces are all live: appearance and typography, the
  expanded font/code-font/terminal settings, work-step display and folding,
  transcript view, language, the shortcuts editor, busy-send behavior, and
  performance/usage detail. `settings/openSettingsDocument` creates the
  preference document when absent and opens `~/.codesmith/desktop/settings.json`
  in the OS default application (the settings header's document action). The
  upstream plugin/account settings sections remain absent: CodeSmith has no
  plugin system or hosted account, so those pages would have no backend. That
  is the final shape of this settings document — General, Models, shortcuts
  and the font/theme overrides are the whole scope; adding plugin or account
  pages needs an explicit product decision to also add those backends.
- Plan mode is a full desktop surface (`client-ui-plan`): the composer plan
  chip reads the `plan` projection (`active` = thread mode `plan`), `/plan
  off` leaves the mode, and a submitted plan renders as a turn-tail card plus
  the right-sidebar plan tab. The runtime's `exit_plan_mode` has an empty
  argument schema and reads the plan file itself, so the bridge attaches the
  latest `write_plan_file` body as the call's `arguments.plan` both on replay
  and live (the live mapper is seeded from the replayed snapshot for a stream
  that joins mid-turn). The vendored card also needs a `# title` heading; a
  plan the model wrote without one gets `# Plan` prepended on the wire only
  (the plan file stays exactly as written). The runtime records an approved
  exit as a `status` item, not a tool item, so the bridge synthesizes the
  `exit_plan_mode` call from that item (its id is the plan's call id) — that
  is what feeds the card and the plan resource. Approving the call queues the
  thread-mode restore to the mode the session came from — the REPL's
  `plan_restore_mode` — which the bridge stashes whenever one of its own mode
  writes enters `plan`; a thread already in `plan` when the bridge first sees
  it (another host switched it, or a page re-approves after a bridge restart)
  has no stash and falls back to `agent`, the runtime default. The restore
  runs when the turn-end event arrives, because the runtime's own settlement
  save would overwrite a PATCH sent mid-turn. A runtime refusal leaves the
  switch to the plan chip. No implementation kickoff is submitted.
  Known limitations: there is no “request changes” review intent — the
  approval card stays allow/deny — and a plan-mode turn that never wrote a
  plan file submits no card.
- The permission surfaces (`client-ui-permission-presets`) map the vendored
  preset picker onto the thread mode through the general agent's two-preset table:
  `workspace-write` (thread mode `agent`) and `danger-full-access`
  (CodeSmith's `yolo`, keeping the picker's explicit risk confirmation). Plan
  is not a preset: while the thread is in `plan` the projection reports the
  derived `read-only`, and `coordinator` reports `custom`; neither is
  advertised in the catalog nor pickable. `/permission
  <workspace-write|danger-full-access>` joins `/mode` in the host command
  catalog; a pick resolves to the mode the preset names and settles as the
  same thread-mode `PATCH` (the `permissions` projection follows it), and a
  pick made while the thread is in `plan` is refused with text pointing at
  `/plan off` rather than silently leaving plan. The same PATCH carries
  `auto_approve` (on for `yolo`/`coordinator`, off otherwise): the runtime's
  prompt assembly requires that pairing, and leaving full access for plan
  mode must still ask before exiting it. A thread left by an older build in
  full-access mode without the flag is repaired when it is first followed or
  prompted. The picker localizes the built-in preset names itself (the
  catalog's `name` equals its `value`, and a name outside the built-in set is
  title-cased), so no `locale.preference` round trip is involved and a
  language switch applies on the next catalog fetch/reload. The General
  settings row's `permission.defaultPreset` lives in `desktop/settings.json`
  and defaults to `workspace-write`; `session/create` applies it to the new
  thread (a legacy `agent`/`plan`/`yolo`/`coordinator` value normalizes on
  read, and any other unknown value falls back to `workspace-write`).
- The `@` menu (`client-ui-reference`) lists workspace files and directories
  over `fileReferences/list`: an empty query lists the root, a path query
  lists that directory, and a bare fragment searches a bounded (50k) workspace
  index with upstream's scoring. Hidden entries and the version-control/
  dependency/build directories are skipped, and symlinks are not followed. The
  index is cached per workspace and dropped when a turn settles. The
  `workingDirectory` projection (`thread.workspace`) lets file chips and file
  links resolve, and each `session/list` row carries its own thread's
  workspace as `cwd` — the bridge joins the summary rows with the runtime's
  thread list (one loopback read per page, no protocol change), so a session
  left by an earlier run in another directory still opens the right files
  tree and resolves chips there; a row whose record cannot be read falls
  back to the bridge process's directory.
  `sessionReferenceResolver/candidates` answers an empty list:
  CodeSmith has no cross-session context-injection feature, so the menu is
  file-only.
- The Workspace sidebar and the empty-state picker are real registry rows
  (`client-ui-workspace` + `client-ui-directory-picker-native`): the native
  OS chooser picks one host directory and `workspace/create` adopts it
  (absolute, existing, a directory; an already-registered path is
  idempotent). The vendored in-app browser package remains for its
  primitives — one absolute host directory level (`directoryPicker/list`:
  directories only, hidden rows included, breadcrumb ancestry, a 2,000-row
  name-sorted prefix with `truncated`) and one child directory
  (`createDirectory`) — but it is not composed, and that is final for this
  shell: booting it alongside the native picker would register a second
  occupant of the same directory-flow holes and open both dialogs.
  Registrations persist at
  `~/.codesmith/desktop/workspaces.json` (atomic rename; a corrupt file logs
  and degrades to the default workspace). The bridge cwd is the implicit
  default workspace (id `codesmith-default`); `rename`, `delete`,
  `insertBefore`, and the per-workspace session order persist. Sessions group
  by the runtime thread's own `workspace` path, not by creation order:
  `session/create` accepts `{workspaceId}` (or `{cwd}`) and posts it to
  `/v1/threads`; a reused blank is returned only while it has no turns. A
  deleted workspace keeps its folder and session logs, and its sessions trail
  under the client's Ungrouped bucket. `directoryPicker/pick` opens the
  native OS folder chooser through Tauri's dialog plugin (cancellation is
  `null`; a bridge-only host answers `directory-picker/unavailable`). A
  cancelled chooser changes nothing; a picked directory is one
  `workspace/create` whose `replace` baseline reaches every open
  `workspace/follow` stream, which is the row the sidebar renders. The
  in-app browser package stays vendored for its `list`/`createDirectory`
  primitives but is not booted (see above).
- The GoalBar (`client-ui-goal`) is fed by the `goal` projection: the live
  engine state when the thread's engine is resident (`GET
  /v1/threads/{id}/goal` answers `loaded:true`), else the latest completed
  `create_goal`/`update_goal` tool record in the thread journal (reading a
  session never loads an engine). The projection carries
  `{goal:{id, revision, objective, phase, blockedReason?, maxGoalRounds},
  roundsStarted, createdAt, updatedAt}`; `complete` renders nothing and
  `blocked` carries the runtime's blocker text. `goals/get` returns the live
  `GoalView` plus `activation` (`armed` while active, else `disarmed`);
  `goals/edit|pause|resume|clear` POST the runtime action (loading the
  engine, which seeds active/completed goals from the journal) and push the
  refreshed projection. The runtime snapshot carries no goal id, so the
  bridge synthesizes a stable id from the creating tool item and counts
  revisions. Desktop pause/clear persist in `~/.codesmith/desktop/goals.json`
  (keyed by that identity): a read presents the persisted stop over the
  journal fallback without loading an engine and re-applies it to an engine
  that is already loaded, and a pre-prompt reconcile re-applies it before a
  freshly loaded engine can drive continuation rounds. The bar itself does
  not create, but the host `/goal <objective> [budget: N]` command does (it
  POSTs the runtime's `edit` action directly, which creates a goal when none
  exists, keeps an unchanged objective's progress, and adopts the optional
  token budget parsed from the `budget: N` suffix — the same suffix the REPL
  accepts; it refuses to create a goal literally named pause/resume/clear,
  pointing at the bar instead). Mutations enforce the client's compare-and-set
  (`ref {id, revision}`): a stale or absent ref answers `goal/stale-revision`
  or `gateway/arguments-invalid` — upstream's GOAL_STALE_REVISION guard — and
  because the synthesized revision is the journal item count plus the
  persisted overlay counter, the ref also survives a bridge restart. Known
  limitations: a goal created through
  `/goal` has no journal item, so it does not survive an engine eviction at
  all; a queued turn the runtime starts after an eviction without an
  intervening desktop prompt can outrun a pause for one turn. The client's
  optional `maxGoalRounds` edit is accepted only at the runtime's fixed cap
  (25); any other value is refused with `goal/rejected` rather than silently
  ignored.
- The document-preview and changed-file open controls
  (`client-ui-open-in-app` + `client-ui-file-actions`) are bridged over the
  existing workspace-files surface: `session/canOpenWorkspacePath` answers
  true, `session/workspacePathApplications` answers an empty catalog (this
  desktop registers no per-path application handlers, so the controls fall
  back to Reveal), and `session/openWorkspacePath` opens or reveals a path
  after canonicalizing it and requiring it to lie inside the bridge cwd, a
  registered workspace, or an imported thread's workspace (a symlinked
  escape reads `workspace-file/outside-workspace`; a missing path reads
  `workspace-file/not-found`). File Actions' Download and Copy image use
  `workspaceFiles/readBytes` and the browser clipboard; Save as stays hidden
  (it needs a native save dialog this shell does not compose), and
  `present.open` remains `nativeUnavailable`. The session header's
  “Open In…” control and the Workspace row's “Show in folder” launch an
  installed application (the file manager first) through the host
  open-in-app catalog; the per-path `workspacePathApplications` catalogue is
  still empty, so the file/document controls render their reveal action
  rather than an application menu.
- Per-message feedback and the `/feedback` dialog
  (`client-ui-message-feedback`) are backed by a desktop-local
  compare-and-set sidecar at `~/.codesmith/desktop/feedback.json`.
  `messageFeedback/list|put|delete` validate the persisted session and that
  the target is a finalized assistant item, enforce the UUID `ifVersion`
  check, the 1,024-byte note cap, and the fixed category vocabulary;
  `sessionFeedback/record` appends a session-level remark. The Like/Dislike
  controls and the dialog update from each mutation's reply, and `/feedback
  <text>` joins the host command catalog with the `@codesmith/command-feedback`
  definition id (so the client's localized `/反馈` alias resolves); the
  vendored plugin decorates that one host row's bare invocation with its
  dialog — it does not register a second menu contribution. The dialog copy
  says feedback is stored locally (the upstream text claimed the conversation
  log was uploaded; this shell uploads nothing). Known limitations: the
  vendored surface has no live event hook, so there are no cross-client
  feedback pushes (the CAS reply reconciles a client that lost a race, and a
  reload re-reads the sidecar); removing a session log leaves its feedback
  rows behind. The CAS list/put/delete contract is covered end to end against
  a fake runtime (`host::tests::feedback_dispatch_enforces_the_sidecar_contract`).
- Control projections also carry `todos` (the latest successful whole-list
  `todo`/`checklist_write` call, pushed live on the tool's `item.started`;
  single-item `todo_add`/`todo_update`/`checklist_add`/`checklist_update`
  calls fold their resulting snapshot from the result metadata),
  `plan`, `permissions`, `workingDirectory`, `goal` (the followed session's
  snapshot and live goal updates; the sidebar's control baseline omits it so
  it never reads another session's goal), and `turnOutline` (every started
  turn's seq plus bounded prompt/response previews). `contextBreakdown` and
  `userQuestions` stay absent: the runtime records no per-request
  system/tools/message composition, and question requests carry no tool-call
  id or cancel route.
- The sidebar terminal tabs (`client-ui-sidebar-terminal` over the
  `terminal/*` remote, `host::terminal`) are real user shells the bridge
  itself spawns: `terminal/create` opens a PTY (`portable-pty`) in the
  session's workspace with the scrubbed environment (`TERM=xterm-256color`,
  `CODESMITH_SESSION_ID`; secret-looking variables dropped) — up to 8
  terminals per session, closed identities never come back, and repeated
  closes succeed. `terminal/shells` probes upstream's candidate list with
  the environment default first; `terminal/environment` reports the session
  directory plus the mirrored limits (500×200, 64 KiB per input command,
  2 MiB per follower queue, 1000 rows of history). `terminal/attach` is the
  one duplex stream: input rides the mux uplink (write/resize commands
  numbered from 1 per attachment, acked in order), input control belongs to
  the newest attachment alone (a demoted attachment acks `read-only`, a
  post-exit one `not-running`), and every attachment opens with a
  whole-screen snapshot — a vt100 emulator mirrors the PTY and serializes
  the visible screen as replayable escape codes (upstream's headless-xterm
  role), with the host's own stripped-text line ring above it for the
  history vt100's viewport cannot walk. Output sequences are contiguous
  from the snapshot; a follower whose queue passes 2 MiB fails its stream
  and reconnecting re-snapshots (upstream's recovery model).
  `terminal/retain` acknowledges the window hold (this host reclaims
  nothing unattended). `terminal/close` TERMs the shell's process group,
  escalates to KILL after the grace, waits the drain, and finishes the
  followers' streams. These are the user's terminals, not the model's —
  `allow_shell` (the model's shell tool) does not gate them. Known
  limitations: terminals die with the bridge process (upstream's host
  process dies the same way); the history ring is stripped text (no colors
  past the visible screen, and it may duplicate lines still visible);
  `attach` carries no `inputWindow` (the mux uplink inbox is frame-counted,
  not byte-counted — the wire types omit the field for such carriers);
  `shellPath` accepts any existing executable, not only discovered shells
  (this host has no sandboxed subprocess provider; the page-origin gate is
  the boundary); a grandchild holding the PTY open past the shell's death
  loses its output stream at the drain timeout, not its life.
- Upstream-only concepts are not simulated: accounts/balance and the account
  settings section (`account/*` answer the signed-out shape or null), the
  plugin manager/market and the settings plugin inventory (the Cordis/JS
  plugin system has no CodeSmith equivalent), per-session agent presets
  (`agentPresets/list` answers an empty roster; CodeSmith selects
  configuration presets at launch, not an agent composition per session),
  sidebar browser tabs (no user-visible browser session; the terminal tabs
  ARE bridged — see their bullet), subagent child-session views (CodeSmith
  subagents are in-process, not separate threads with journals), and plugin
  entry customization. Their plugins stay out of the boot roster or answer
  empty states. The workflow-run panel IS fed: the runtime journals the
  `workflow` tool's lifecycle as `workflow.*` thread events (the
  `WorkflowRunRecorder` seam — live, every outcome) and the successful
  run's durable record on the tool item's `metadata.workflow_run` (replay);
  the mapper folds both onto the vendored client's `tool-workflow/*` node
  family (run header, per-member rows with synthetic inert child ids, stop
  reason). Known limitations: a FAILED run's node is live-only (its error
  item settles without metadata, so a reload drops it); a reconnect
  mid-run shows the node only after the run settles (events before the
  detail's cursor are not replayed); nested runs (a sub-agent's own
  workflow) record nothing. (The jobs popover, the attachment
  presentation, the changes-review tab, the sidebar files/preview tabs, the
  sidebar terminal tabs, the plan page, the permission surfaces, the `@`
  file source, the Workspace picker/registry, the GoalBar, the
  open/reveal/file actions, and the per-message feedback surface ARE
  bridged — see their bullets.)
- The sidebar files tree and document preview
  (`client-ui-sidebar-files` + `client-ui-sidebar-documentpreview` over the
  `workspaceFiles` remote, `host::files`) read the session's workspace
  directly: `workspaceFileScope` resolves the root from the thread record,
  `list` serves one directory's children (workspace-path vocabulary,
  lexically confined to the root), `read` serves line pages of text files,
  `stat` the freshness token (`mtime:size`), and `changes` is a 1s stat
  poll pushing upstream's `ready`/`change` frames. `readBytes` answers
  multipart/form-data — the envelope carries a top-level attachments
  manifest while its value holds `data: null`, and the bytes ride as a
  binary file part (a `filename` is required so the Fetch parser keeps
  them as bytes rather than UTF-8 text), which the page's carrier plants
  (the client's strict codec only accepts a real `Uint8Array`). Renderers
  are the vendored pure-web ones (markdown, code, text, image, PDF,
  Excel/CSV); the OFFICE renderer's conversion RPC (`officeToPdf/render`)
  shells out to a local LibreOffice `soffice` (PATH, the macOS bundle, or
  `CODESMITH_DESKTOP_SOFFICE`) under an isolated per-conversion profile —
  the source stages through the workspace fence with version re-checks
  (`source-changed`), the PDF is validated (`%PDF-`/`%%EOF`, size caps) and
  answers through the same multipart bytes attachment `readBytes` uses, and
  an absent engine answers the wire's `unavailable` reason so the panel
  renders its localized unavailable state instead of pretending
  (`officeToPdf/generation` is a per-process UUID; no bridge-side result
  cache — the client caches by generation+version). Reads refuse a
  final-component
  symlink (upstream's own gate) and `baseFile`-relative resolution is not
  implemented (the preview addresses files by absolute path).
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
  attached again; no upload progress
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
- Tool-call output deltas are not streamed — the vendored cards don't
  consume live output either (a running card shows its shimmer and settles
  from `tool/result`), so this matches the upstream interaction. Tool names
  are normalized to the vendored cards' vocabulary at the wire (`exec_shell`
  → bash card, `read_file` → read card, …); the runtime record keeps
  CodeSmith's real names. Tool items record their arguments in additive
  `metadata.arguments` (preserved when the result's own metadata replaces
  it), so replayed tool calls render their arguments; a replay with no
  recorded arguments falls back to `{}` (foreign HTTP clients' turns from
  before the field existed). `toolResults/watch` (the trajectory inspector's
  on-demand lookup) answers the settled `tool/result` wire event for a seq,
  statelessly from a thread-record replay — a seq nothing maps to answers
  `{available:false}` and parks.
- Token accounting is bridged end to end. The runtime reports usage per TURN;
  the final agent item's completion payload carries it (the runtime defers
  that emission to the turn's end so the usage is known — a mid-turn settle
  emits before the next engine event, preserving journal order), and the
  mapper attaches the client's usage sample to that item's
  `assistant/message` (`inputTokens` = the uncached share,
  `cacheReadTokens` = the hit share, `totalTokens` = input + output), with
  the seat's real (provider, model) as the message source — the client's
  turn fold aggregates samples into the per-turn pill, live and on replay
  (replay stamps the turn's `turns[].usage` on its last settled agent
  message). The `sessionStats` and `tokenUsage` projections (the composer's
  activity and usage pills) fold from the durable records: turns, steps,
  LLM/tool wall time, decode tokens; TTFT is not separable from durable
  records, so the projection reports none and decode time uses the full
  agent-item spans (a conservative speed). The context meter's denominator
  rides `contextWindow` in the `contextPressure` projection, from
  `GET /v1/models`'s per-model `context_window` estimate (absent when the
  runtime can't estimate one — the meter then stays hidden); CodeSmith
  reports no cache-write bucket, so that row never appears.
- User questions have no HTTP cancel route, so an unanswered question
  waterfall waits for the runtime's own deadline; question requests carry no
  tool call id, so the composer card renders without the countdown claim.
- The `$events` waterfall follows the upstream gateway contract: each stream
  gets its own `clientId`, still-pending requests are re-delivered to a newly
  connected stream, and a late or duplicate `$events/result` is an idempotent
  no-op (`delivered:false`, runtime 404 included). A delivered decision is
  followed by the request's cancel frame — that frame is what retires the
  answering card on every connected client. A waterfall pushes on its first
  registration only: the follow stream and the background watcher both read
  the runtime's event tail, and a second push would double-dispatch the
  client's waterfall chain (an approval card that survives its own decision
  until a second click). Detection does not depend
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
  admitted, like the HTTP routes). Uplink items route to the open stream
  (the sidebar terminal's attach is the one reader; a stream whose handler
  drops its uplink receiver swallows the items, upstream's unread inbox) and
  a full uplink inbox fails its stream (`gateway/uplink-overflow`). The mux
  sends no ping heartbeat (loopback: a vanished page
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
  stays unverified. The scripted mock closes that gap at the wire level: a
  "写文件" prompt makes it stream a structured `write_file` tool call, and the
  approval round is verified at both layers — the bridge protocol (the
  `$events` waterfall arrives, `$events/result` allow/deny reaches
  `/v1/approvals/{id}`, the journal carries both decisions, the allowed call
  writes the file and the denied one leaves it byte-identical) and the live
  UI (the approval card renders from the waterfall, 允许一次 executes the
  write and 拒绝 feeds the denial back, the card and the sidebar's pending
  badge retiring after either decision).
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

## Parity roadmap

The desktop converges on the upstream client's design: shared features are
upstream's, rebranded mechanically (the logo and product naming stay
CodeSmith). This table is the durable tracking surface for that convergence —
update it with every vendor bump and every landed slice; per-feature detail
lives in the bullets above.

| Surface | State | Next | Decision |
| --- | --- | --- | --- |
| Sidebar terminal tabs | bridged | — | landed |
| workflow-run panel | fed (runtime journal + mapper) | — | landed |
| Subagent / team board | not vendored | in-process subagents are not threads; needs runtime-side design | needs design |
| Plugin manager / market | not simulated | map onto CodeSmith extensions + Rhai mods | **needs product decision** |
| Agent presets | not simulated | map onto launch presets | **needs product decision** |
| Office document preview | bridged over a local `soffice` | — | landed |
| open-in-app icons | macOS icns → PNG served | Linux/Windows keep the glyph | landed |
| goal durable CAS | landed (the mutation ref guard) | — | landed |
| pixel-parity baseline · upload progress | Known-limitations bullets above | clear per bullet | queued |
| Auto-update · welcome · tray | none | desktop-shell slices (Tauri updater and friends) | later, optional |
| Account / platform · sidebar browser tabs | not simulated | — | will not bridge (upstream brand infrastructure; Electron's nested webview has no wry equivalent) |
| Vendor refresh | pinned `8bd07339`, level with the local checkout | re-run the vendor pipeline after upstream moves and repair wire drift | standing mechanism |
