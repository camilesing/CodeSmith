# Configuration

codesmith reads configuration from a TOML file plus environment variables.
At process startup it also loads a workspace-local `.env` file when present.
Use the tracked `.env.example` as the template; copy it to `.env`, then edit
only the provider and safety knobs you need.

## Where It Looks

Default config path:

- `~/.codesmith/config.toml`
- Legacy fallback: `~/.deepseek/config.toml`

Overrides:

- CLI: `codesmith --config /path/to/config.toml`
- Env: `CODESMITH_CONFIG_PATH=/path/to/config.toml`
- Legacy env alias: `DEEPSEEK_CONFIG_PATH=/path/to/config.toml`

If both are set, `--config` wins. Environment variable overrides are applied after the file is loaded.

### Per-project overlay (#485)

When the TUI starts in a workspace that contains a
`<workspace>/.codesmith/config.toml` file, the values declared in that
file are merged on top of the global config. Legacy
`<workspace>/.deepseek/config.toml` files are still read when the
CodeSmith path is absent. This lets a repo lock its own provider,
model, sandbox policy, or approval policy without touching the user's
`~/.codesmith/config.toml`. Pass
`--no-project-config` to skip the overlay for one launch.

Supported keys in the project overlay (top-level fields only):

| Key | Effect |
|---|---|
| `provider` | switch backend (e.g. `"nvidia-nim"` for an enterprise repo) |
| `model` | override `default_text_model` |
| `api_key` | use a per-repo key (typically read from `.env`, **not committed**) |
| `base_url` | point at a self-hosted endpoint |
| `reasoning_effort` | force `"high"` / `"max"` for a complex repo |
| `approval_policy` | `"never"` / `"on-request"` / `"untrusted"` for opinionated repos |
| `sandbox_mode` | `"read-only"` / `"workspace-write"` / `"danger-full-access"` |
| `mcp_config_path` | per-repo MCP server set |
| `notes_path` | keep notes in-repo |
| `max_subagents` | clamp concurrency for a constrained repo (clamped to 1..=20) |
| `allow_shell` | gate shell tool access on `false` |

The overlay is intentionally narrow — it covers the fields a repo
maintainer is most likely to want to standardize across contributors.
Other settings (skills_dir, hooks, capacity, retry, etc.) stay
user-global. If your repo needs more, file an issue describing the
specific use case.

The `codesmith` facade and `codesmith-tui` binary share the same config file for
DeepSeek auth and model defaults. `codesmith auth set --provider deepseek` (and
the legacy `codesmith login --api-key ...` alias) saves the key to
`~/.codesmith/config.toml` (migrating legacy `~/.deepseek/config.toml` on first
launch when needed), and `codesmith --model deepseek-v4-flash` is forwarded to
the TUI as `DEEPSEEK_MODEL`.

Credential lookup uses `config -> keyring -> env` after any explicit CLI
`--api-key`. Run `codesmith auth status` to inspect the active provider's config
file, OS keyring backend, environment variable, winning source, and last-four
label without printing the key itself. The command only probes the active
provider's keyring entry.

For hosted, generic OpenAI-compatible, or self-hosted providers, set
`provider = "nvidia-nim"`, `"openai"`, `"atlascloud"`, `"wanjie-ark"`,
`"volcengine"`, `"openrouter"`, `"xiaomi-mimo"`, `"novita"`, `"fireworks"`,
`"siliconflow"`, `"moonshot"`, `"sglang"`, `"vllm"`, or `"ollama"` or pass
`codesmith --provider <name>`.
For the provider-by-provider registry, including auth variables, default base
URLs, model IDs, and capability metadata, see [PROVIDERS.md](PROVIDERS.md).
The facade saves provider credentials to the shared user config and forwards
the resolved key, base URL, provider, and model to the TUI process. Use
`codesmith auth set --provider nvidia-nim --api-key "YOUR_NVIDIA_API_KEY"` or
`codesmith auth set --provider openai --api-key "YOUR_OPENAI_COMPATIBLE_API_KEY"` or
`codesmith auth set --provider atlascloud --api-key "YOUR_ATLASCLOUD_API_KEY"` or
`codesmith auth set --provider wanjie-ark --api-key "YOUR_WANJIE_API_KEY"` or
`codesmith auth set --provider xiaomi-mimo --api-key "YOUR_XIAOMI_KEY"` or
`codesmith auth set --provider fireworks --api-key "YOUR_FIREWORKS_API_KEY"` or
`codesmith auth set --provider siliconflow --api-key "YOUR_SILICONFLOW_API_KEY"`
to save provider keys through the facade. The generic `openai` provider defaults
to `https://api.openai.com/v1`, accepts `OPENAI_BASE_URL`, and defaults to
`gpt-5`. Pointing `OPENAI_BASE_URL` at a third-party gateway requires an
explicit model (`OPENAI_MODEL`, `--model`, or `[providers.openai].model`);
without one, startup fails fast instead of sending the provider default to a
gateway that cannot serve it. `atlascloud` defaults to
`https://api.atlascloud.ai/v1`, accepts `ATLASCLOUD_BASE_URL`, and uses
`deepseek-ai/deepseek-v4-flash` as its default model. `wanjie-ark` targets
Wanjie Ark's OpenAI-compatible endpoint at
`https://maas-openapi.wanjiedata.com/api/v1`, defaults to `deepseek-reasoner`,
and passes model IDs through unchanged because Wanjie model access is
account-scoped. SGLang, vLLM, and Ollama are
self-hosted and can run without an API key by default. Ollama defaults to
`http://localhost:11434/v1` and sends model tags such as `codesmith-coder:1.3b`
or `qwen2.5-coder:7b` unchanged. Self-hosted providers and loopback custom
URLs (`localhost`, `127.0.0.1`, `[::1]`, `0.0.0.0`) do not read the secret store
unless API-key auth is explicitly requested; use an env var or config-file key
when a local server does require bearer auth.
SiliconFlow defaults to `https://api.siliconflow.com/v1`, accepts
`SILICONFLOW_BASE_URL`, and uses `deepseek-ai/DeepSeek-V4-Pro` by default.
`https://api.siliconflow.cn/v1` can still be configured explicitly when a user
needs the regional endpoint.

### Custom OpenAI-Compatible Gateways

For a third-party service that implements the OpenAI Chat Completions API, use
the built-in `openai` provider name and point its provider table at the gateway:

```toml
provider = "openai"
default_text_model = "your-model-id"

[providers.openai]
api_key = "YOUR_OPENAI_COMPATIBLE_API_KEY"
base_url = "https://your-gateway.example/v1"
```

Do not invent a custom provider name; `provider` must be one of the known
providers listed above. Put the endpoint under `[providers.openai]`, not the
legacy top-level `base_url`, so the OpenAI-compatible provider receives it.
`default_text_model` is the model ID sent to the gateway; if you keep several
provider tables in one config, `[providers.openai].model` can be used as the
OpenAI-provider-specific override.

Local HTTP endpoints such as Ollama, SGLang, and vLLM are allowed by default
when they use localhost or loopback addresses. For a non-local `http://`
gateway, launch with `DEEPSEEK_ALLOW_INSECURE_HTTP=1` only on a trusted network:

```bash
DEEPSEEK_ALLOW_INSECURE_HTTP=1 codesmith
```

Third-party OpenAI-compatible gateways that need extra request headers can set
`http_headers = { "X-Model-Provider-Id" = "your-model-provider" }` at the top
level or under a provider table such as `[providers.deepseek]`. When configured,
codesmith sends those custom headers on model API requests. The equivalent
environment override is `DEEPSEEK_HTTP_HEADERS`, using comma-separated
`name=value` pairs such as
`X-Model-Provider-Id=your-model-provider,X-Gateway-Route=dev`. `Authorization`
and `Content-Type` are managed by the client and are not overridden by this
setting.

### Vision Model

CodeSmith's chat provider and `image_analyze` tool are configured separately.
The main chat path remains the selected text/tool provider; image analysis runs
through `[vision_model]` when the `vision_model` feature is enabled.

Xiaomi's current image-understanding docs include `mimo-v2.5` for image input.
To use MiMo for `image_analyze`, configure the vision model explicitly:

```toml
[features]
vision_model = true

[vision_model]
model = "mimo-v2.5"
api_key = "YOUR_XIAOMI_KEY"
base_url = "https://api.xiaomimimo.com/v1"
```

### Utility Model

`[utility_model]` designates a cheap/fast secondary LLM for background
assists so the main model's budget is spent on the actual conversation:

- Workshop large-output synthesis (#548): tool results above the
  `large_output_threshold_tokens` threshold are condensed by this model;
  only the synthesis enters the parent context, the raw text is kept in
  the workshop variable `last_tool_result` (`raw = true` on a tool call
  bypasses routing)
- Auto-route classification (`/model auto` and sub-agent routing)
- Flash seam default (`[context] seam_model`, when that key is unset)

When the table is absent every assist uses the main model — behaviour is
unchanged for single-model setups.

```toml
[utility_model]
model = "deepseek-v4-flash"        # required: setting the table enables it
# provider = "openai"              # optional: defaults to the main provider
# api_key = "YOUR_API_KEY"         # optional: defaults to the main api_key
# base_url = "https://..."         # optional: defaults to the main base_url
```

Same-provider setups reuse the main client with a per-request model
override; a different `provider` builds a dedicated second client (for
example main = anthropic, utility = deepseek) and then needs its own
`api_key` — one vendor's key is never sent to another. The model id can
also be set with `CODESMITH_UTILITY_MODEL` (legacy alias
`DEEPSEEK_UTILITY_MODEL`).

### Code Index

`[index]` configures the persistent per-workspace code index that backs the
`symbol_search` and `find_references` tools (see `docs/INDEX.md`). The
table is optional — absent means enabled with the built-in `tree-sitter`
backend for rust/python/js/ts/go, SQLite storage under
`~/.codesmith/index/<workspace>/`, and lazy incremental refresh on every
query (no file watcher):

```toml
[index]
enabled = true              # master switch (CODESMITH_INDEX_ENABLED)
refresh_budget_ms = 2000    # per-query incremental refresh budget

[index.symbols]
backend = "tree-sitter"     # backend id (CODESMITH_INDEX_SYMBOLS_BACKEND)
[index.symbols.languages]   # per-language switches, absent = enabled
python = false              # e.g. skip python
```

Setting `enabled = false` (or `[index.symbols] enabled = false`) removes
the two tools from the session catalog entirely. `[index.semantic]` exists
as a reserved seam for embedding-based search but has no built-in backend
yet — leave it disabled.

To bootstrap MCP and skills directories at their resolved paths, run `codesmith-tui setup`.
To only scaffold MCP, run `codesmith-tui mcp init`.

Note: setup, doctor, mcp, features, sessions, resume/fork, exec, review, and eval
are subcommands of the `codesmith-tui` binary. The `codesmith` dispatcher exposes a
distinct set of commands (`auth`, `config`, `model`, `thread`, `sandbox`,
`app-server`, `mcp-server`, `completion`) and forwards plain prompts to
`codesmith-tui`.

### Startup Update Checks

By default, the TUI starts a background check for the latest stable CodeSmith
release and shows a short toast only when a newer release is available and the
official release assets are complete.

Disable the startup check entirely for air-gapped, corporate-proxy, or managed
desktop environments:

```toml
[update]
check_for_updates = false
```

To redirect the startup check, set `update_uri` to an internal endpoint that
returns GitHub-compatible latest-release JSON. Minimal mirror metadata with a
`tag_name` field is accepted; if `assets` are present, CodeSmith requires the
same uploaded asset set as the official release before showing the toast.

```toml
[update]
check_for_updates = true
update_uri = "https://internal.mirror.example/codesmith/releases/latest"
```

When `update_uri` is not set, startup checks honor release mirror environment
variables such as `CODESMITH_RELEASE_BASE_URL` before falling back to the
official GitHub API endpoint. If a configured `update_uri` cannot be fetched or
parsed and a release mirror env var is set, the TUI falls back to that mirror
instead of failing startup.

## Profiles

You can define multiple profiles in the same file:

```toml
api_key = "PERSONAL_KEY"
default_text_model = "deepseek-v4-pro"

[profiles.work]
api_key = "WORK_KEY"
base_url = "https://api.deepseek.com/beta"

[profiles.nvidia-nim]
provider = "nvidia-nim"
api_key = "NVIDIA_KEY"
base_url = "https://integrate.api.nvidia.com/v1"
default_text_model = "deepseek-ai/deepseek-v4-pro"

[profiles.fireworks]
provider = "fireworks"
default_text_model = "accounts/fireworks/models/deepseek-v4-pro"

[profiles.siliconflow]
provider = "siliconflow"
default_text_model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.siliconflow.providers.siliconflow]
base_url = "https://api.siliconflow.com/v1"

[profiles.openai-compatible]
provider = "openai"

[profiles.openai-compatible.providers.openai]
base_url = "https://openai-compatible.example/v4"
model = "glm-5"

[profiles.atlascloud]
provider = "atlascloud"

[profiles.atlascloud.providers.atlascloud]
base_url = "https://api.atlascloud.ai/v1"
model = "deepseek-ai/deepseek-v4-flash"

[profiles.sglang]
provider = "sglang"
base_url = "http://localhost:30000/v1"
default_text_model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.vllm]
provider = "vllm"
base_url = "http://localhost:8000/v1"
default_text_model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.ollama]
provider = "ollama"
base_url = "http://localhost:11434/v1"
default_text_model = "codesmith-coder:1.3b"
```

Select a profile with:

- CLI: `codesmith --profile work`
- Env: `CODESMITH_PROFILE=work`

If a profile is selected but missing, codesmith exits with an error listing available profiles.

## Environment Variables

Most runtime environment variables override config values. API-key variables are
fallbacks after saved config and keyring credentials.

Every app-level variable accepts the `CODESMITH_*` name (preferred) plus its
legacy `DEEPSEEK_*` (and older `CODEWHALE_*`) alias as a fallback. When both
forms are set the `CODESMITH_*` value wins:

- `CODESMITH_PROVIDER` (preferred) / `DEEPSEEK_PROVIDER` (legacy alias) —
  `deepseek|nvidia-nim|openai|atlascloud|wanjie-ark|openrouter|xiaomi-mimo|novita|fireworks|siliconflow|moonshot|sglang|vllm|ollama`
- `CODESMITH_MODEL` (preferred) / `DEEPSEEK_MODEL` (legacy alias) — default model for the active provider
- `CODESMITH_BASE_URL` (preferred) / `DEEPSEEK_BASE_URL` (legacy alias) — base URL for the active provider

Remaining app-level variables (each also answers to its `DEEPSEEK_*` alias):

- `CODESMITH_API_KEY`
- `CODESMITH_HTTP_HEADERS` (custom model request headers, comma-separated `name=value` pairs)
- `DEEPSEEK_DEFAULT_TEXT_MODEL` (extra legacy alias of `CODESMITH_MODEL`)
- `NVIDIA_API_KEY` or `NVIDIA_NIM_API_KEY` (preferred when provider is `nvidia-nim`; falls back to `CODESMITH_API_KEY` / `DEEPSEEK_API_KEY`)
- `NVIDIA_NIM_BASE_URL`, `NIM_BASE_URL`, or `NVIDIA_BASE_URL`
- `NVIDIA_NIM_MODEL`
- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`
- `OPENAI_MODEL`
- `ATLASCLOUD_API_KEY`
- `ATLASCLOUD_BASE_URL`
- `ATLASCLOUD_MODEL`
- `WANJIE_ARK_API_KEY`, `WANJIE_API_KEY`, or `WANJIE_MAAS_API_KEY`
- `WANJIE_ARK_BASE_URL`, `WANJIE_BASE_URL`, or `WANJIE_MAAS_BASE_URL`
- `WANJIE_ARK_MODEL`, `WANJIE_MODEL`, or `WANJIE_MAAS_MODEL`
- `VOLCENGINE_API_KEY`, `VOLCENGINE_ARK_API_KEY`, or `ARK_API_KEY`
- `VOLCENGINE_BASE_URL`, `VOLCENGINE_ARK_BASE_URL`, or `ARK_BASE_URL`
- `VOLCENGINE_MODEL` or `VOLCENGINE_ARK_MODEL`
- `OPENROUTER_API_KEY`
- `OPENROUTER_BASE_URL`
- `XIAOMI_MIMO_API_KEY`, `XIAOMI_API_KEY`, or `MIMO_API_KEY`
- `XIAOMI_MIMO_BASE_URL` or `MIMO_BASE_URL`
- `XIAOMI_MIMO_MODEL` or `MIMO_MODEL`
- `NOVITA_API_KEY`
- `NOVITA_BASE_URL`
- `FIREWORKS_API_KEY`
- `FIREWORKS_BASE_URL`
- `SILICONFLOW_API_KEY`
- `SILICONFLOW_BASE_URL`
- `SILICONFLOW_MODEL`
- `MOONSHOT_API_KEY` or `KIMI_API_KEY`
- `MOONSHOT_BASE_URL` or `KIMI_BASE_URL`
- `MOONSHOT_MODEL`, `KIMI_MODEL_NAME`, or `KIMI_MODEL`
- `SGLANG_BASE_URL`
- `SGLANG_MODEL`
- `SGLANG_API_KEY` (optional; many localhost SGLang servers do not require auth)
- `VLLM_BASE_URL`
- `VLLM_MODEL`
- `VLLM_API_KEY` (optional; many localhost vLLM servers do not require auth)
- `OLLAMA_BASE_URL`
- `OLLAMA_MODEL`
- `OLLAMA_API_KEY` (optional; many localhost Ollama servers do not require auth)
- `CODESMITH_LOG_LEVEL` or `RUST_LOG` (`info`/`debug`/`trace` enables lightweight verbose logs)
- `CODESMITH_SKILLS_DIR`
- `CODESMITH_MCP_CONFIG`
- `CODESMITH_NOTES_PATH`
- `CODESMITH_MEMORY` (`1|on|true|yes|y|enabled` turns user memory on)
- `CODESMITH_MEMORY_PATH`
- `CODESMITH_ALLOW_SHELL` (`1`/`true` enables)
- `CODESMITH_APPROVAL_POLICY` (`on-request|untrusted|never`)
- `CODESMITH_SANDBOX_MODE` (`read-only|workspace-write|danger-full-access|external-sandbox`)
- `CODESMITH_MANAGED_CONFIG_PATH`
- `CODESMITH_REQUIREMENTS_PATH`
- `CODESMITH_MAX_SUBAGENTS` (clamped to `1..=20`)
- `CODESMITH_TASKS_DIR` (runtime task queue/artifact storage, default
  `~/.codesmith/tasks`, with legacy `~/.deepseek/tasks` fallback when only the
  legacy directory exists)
- `CODESMITH_HOME` (override the base data directory; defaults to `~/.codesmith`).
  If you previously exported `DEEPSEEK_HOME`, rename it to `CODESMITH_HOME`;
  the old env var is not used for new CodeSmith state paths.
- `CODESMITH_RELEASE_BASE_URL` (release asset mirror used by `codesmith update`
  and by TUI startup update checks when `[update].update_uri` is not set, or as
  a fallback when that configured URI cannot be fetched)
- `CODESMITH_AUTOMATIONS_DIR` (override the automations storage directory; uses
  `~/.codesmith/automations` by default, with legacy `~/.deepseek/automations`
  fallback when only the legacy directory exists)
- `CODESMITH_CAPACITY_ENABLED`
- `CODESMITH_CAPACITY_LOW_RISK_MAX`
- `CODESMITH_CAPACITY_MEDIUM_RISK_MAX`
- `CODESMITH_CAPACITY_SEVERE_MIN_SLACK`
- `CODESMITH_CAPACITY_SEVERE_VIOLATION_RATIO`
- `CODESMITH_CAPACITY_REFRESH_COOLDOWN_TURNS`
- `CODESMITH_CAPACITY_REPLAN_COOLDOWN_TURNS`
- `CODESMITH_CAPACITY_MAX_REPLAY_PER_TURN`
- `CODESMITH_CAPACITY_MIN_TURNS_BEFORE_GUARDRAIL`
- `CODESMITH_CAPACITY_PROFILE_WINDOW`
- `CODESMITH_CAPACITY_PRIOR_CHAT`
- `CODESMITH_CAPACITY_PRIOR_REASONER`
- `CODESMITH_CAPACITY_PRIOR_V4_PRO`
- `CODESMITH_CAPACITY_PRIOR_V4_FLASH`
- `CODESMITH_CAPACITY_PRIOR_FALLBACK`
- `NO_ANIMATIONS` (`1|true|yes|on` forces `low_motion = true` and
  `fancy_animations = false` at startup, regardless of the saved
  settings; see [`docs/ACCESSIBILITY.md`](./ACCESSIBILITY.md)).
- `SSL_CERT_FILE` — corporate-proxy / TLS-inspecting MITM users
  point this at a PEM bundle (or single DER cert) and the cert(s)
  get added alongside the platform's system trust store. Failures
  log a warning and continue — the existing system roots still
  apply.

### Instruction sources (`instructions = [...]`, #454)

Add a list of additional system-prompt sources that get
concatenated, in declared order, alongside the auto-loaded
`AGENTS.md`:

```toml
instructions = [
    "./AGENTS.md",
    "~/.codesmith/global.md",
    "~/team/agents-shared.md",
]
```

Rules:

- Paths run through `expand_path` so `~` and env vars work.
- Each file is capped at 100 KiB; oversized files are
  truncated with a `[…elided]` marker rather than skipped.
- Missing files are skipped with a tracing warning so a stale
  entry doesn't fail the launch.
- Project config (`<workspace>/.codesmith/config.toml`, or legacy
  `<workspace>/.deepseek/config.toml`)
  **replaces** the user array wholesale rather than merging.
  If you want both, list `~/global.md` inside the project
  array. Set `instructions = []` in the project to clear the
  user list for that repo.

### `/hooks` listing

Run `/hooks` (or `/hooks list`) inside the TUI to see every
configured lifecycle hook grouped by event, including each
hook's name, command preview, timeout, and condition. The
`[hooks].enabled` flag's state is shown at the top so it's
obvious when hooks are globally suppressed. Hooks are
configured under `[[hooks.hooks]]` entries — see the existing
hook-system documentation for the full schema.

### Mutable `message_submit` hooks

`message_submit` hooks run before a submitted message is added to
history or sent to the model. Unlike observer-only lifecycle hooks,
non-background `message_submit` hooks can replace or block the
submitted text.

```toml
[[hooks.hooks]]
event = "message_submit"
command = "~/.codesmith/hooks/inject-context.sh"
timeout_secs = 2
continue_on_error = true
```

The hook receives JSON on stdin:

```json
{
  "event": "message_submit",
  "text": "original user text",
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "total_tokens": 1234
}
```

If the hook exits `0` and prints JSON with a non-empty string `text` field,
that value replaces the submitted text:

```json
{ "text": "replacement user text" }
```

Exit `0` with empty stdout, or stdout JSON without `text`, leaves
the current text unchanged. A JSON `text` field must not be empty;
`{"text":""}` is treated as invalid stdout and ignored. Exit `2`
blocks the submission before the turn starts; a `reason` field,
stderr, or stdout can provide the status message shown in the TUI.
Other non-zero exits follow the hook's `continue_on_error` setting.
Timeouts and spawn failures are also surfaced as transient TUI status
messages when `continue_on_error = true` lets submission continue.

Multiple `message_submit` hooks run in config order, and each hook
receives the text produced by the previous hook. Hooks marked
`background = true` are observer-only and cannot transform or block
the message. Existing environment variables remain available.
`shell_env` hooks keep their existing `KEY=VALUE` stdout contract;
the JSON stdout contract applies only to `message_submit`.

The `session_id` field (and the `DEEPSEEK_SESSION_ID` env var) carries an
**ephemeral** per-construction telemetry id — it changes on every session
start and does not correlate across restarts. For cross-restart correlation
(resume, capacity-memory continuity), use `DEEPSEEK_THREAD_ID`, which carries
the persistent thread id and is also provided as a `thread_id` field in
structured hook payloads.

### Composer stash (`/stash`, Ctrl+S)

Press **Ctrl+S** in the composer to park the current draft to
`~/.codesmith/composer_stash.jsonl`. `/stash list` shows parked
drafts with one-line previews and timestamps; `/stash pop`
restores the most recently parked draft (LIFO); `/stash clear`
wipes the file. Capped at 200 entries; multiline drafts
round-trip intact.

## Settings File (Persistent UI Preferences)

codesmith also stores user preferences in:

- `~/.config/deepseek/settings.toml`

Notable settings include `auto_compact` (default `false`), which opts into
replacement-style summarization only near the active model limit. The default
V4 path preserves the stable message prefix for cache reuse; use manual
`/compact` or enable `auto_compact` only when you explicitly want automatic
replacement compaction. You can inspect or update these from the TUI with
`/settings` and `/config` (interactive editor).

Common settings keys:

- `theme` (`system`, `dark`, `light`, `grayscale`, `catppuccin-mocha`,
  `tokyo-night`, `dracula`, `gruvbox-dark`; default `system`): `system`
  follows terminal background detection, `dark`/`light` use the DeepSeek
  palettes, `grayscale` is the low-opinion black/white theme, and the named
  community presets apply across the TUI. Aliases such as `whale`, `mono`,
  `black-white`, `tokyonight`, and `gruvbox` are accepted.
- `auto_compact` (on/off, default off)
- `paste_burst_detection` (on/off, default on): fallback rapid-key paste
  detection for terminals that do not emit bracketed-paste events. This is
  independent of terminal bracketed-paste mode.
- `mention_menu_limit` (integer, default `128`): maximum number of
  `@`-mention popup candidates retained before the composer renders the
  visible window. The visible rows still depend on terminal height.
- `mention_walk_depth` (integer, default `6`): maximum workspace depth for
  `@`-mention completion walks. Set to `0` for unlimited depth in deeply
  nested workspaces; keep the default in very large repos unless needed.
- `show_thinking` (on/off)
- `is_simple` (on/off, default off): answer in maximum-compression "simple"
  (caveman) conversation style — short sentences, no filler or pleasantries —
  while code, commands, and error messages stay byte-exact and the user's
  language is preserved. Security warnings and destructive-operation notices
  automatically switch back to full clarity. Toggle via `/config is_simple on`;
  applies from the next turn.
- `show_tool_details` (on/off)
- `locale` (`auto`, `en`, `ja`, `zh-Hans`, `pt-BR`; default `auto`): UI chrome
  locale. `auto` checks `LC_ALL`, `LC_MESSAGES`, then `LANG`; unsupported or
  missing locales fall back to English. The runtime also exposes the resolved
  locale in the system prompt as the fallback natural language for V4 reasoning
  and replies when the latest user message is ambiguous. Clear user language
  still takes priority; Chinese turns should produce Chinese `reasoning_content`
  and Chinese final replies even when the resolved locale is English.
- `background_color` (`#RRGGBB`, `RRGGBB`, or `default`): optional main TUI
  background color applied to the root, header, transcript, and footer
  surfaces while preserving panel contrast.
- `cost_currency` (`usd`, `cny`; default `usd`): currency used by the footer,
  context panel, `/cost`, `/tokens`, and long-turn notification summaries. The
  aliases `rmb` and `yuan` normalize to `cny`.
- `default_mode` (agent, plan, yolo; legacy `normal` is accepted and normalized to `agent`)
- `sidebar_focus` (`auto`, `work`, `tasks`, `agents`, `context`, `hidden`; default
  `auto`): selects the right sidebar focus. `auto` prioritizes Work, Tasks,
  Agents, then optional Context, and uses Work as the single quiet empty state.
  `hidden` disables the right sidebar entirely so raw terminal selection cannot
  cross from the transcript into sidebar borders. Legacy `plan` and `todos`
  values are accepted and normalized to `work`.
- `max_history` (number of submitted input history entries; cleared drafts are
  also kept locally for composer history search)
- `default_model` (model name override)

Only `agent`, `plan`, and `yolo` are visible modes in the UI. Switch between
them with `/mode`. For compatibility, older settings files with
`default_mode = "normal"` still load as `agent`.

Localization scope is tracked in [LOCALIZATION.md](LOCALIZATION.md). The v0.7.6
core pack covers high-visibility TUI chrome only; provider/tool schemas,
personality prompts, and full documentation remain English unless explicitly
translated later.

Readability semantics:

- Selection uses a unified style across transcript, composer menus, and modals.
- Footer hints use a dedicated semantic role (`FOOTER_HINT`) so hint text stays readable across themes.
- The footer includes a compact `coherence` chip that describes how stable and
  focused the current session is right now. Possible states are `healthy`,
  `crowded`, `refreshing`, `verifying`, and `resetting`; these are derived from
  capacity and compaction events without exposing internal formulas in normal UI.

### Token Quantities and Drivers

DeepSeek V4 prefix caching makes token labels matter. These quantities are kept
separate:

| Quantity | Meaning | Allowed to drive |
|---|---|---|
| Active request input estimate | Conservative estimate of the next request's live system prompt and transcript payload. | Header/footer context percent, hard-cycle trigger, opt-in Flash seam trigger, and emergency overflow preflight. |
| Reserved response headroom | The internal turn budget plus safety headroom. v0.8.16 keeps normal turns at `262144` reserved output tokens and adds `1024` safety tokens for context-window checks, even though V4 capability metadata reports the official `384000` max output. | Hard-cycle and emergency overflow budget checks only. |
| Cumulative API usage | Provider-reported input plus output tokens summed across completed API calls; multi-tool turns may count the same stable prefix more than once. | Session usage and approximate cost telemetry only. |
| Prompt cache hit/miss | Provider cache telemetry for the most recent call when available. | Cache-hit display and cost estimation only; never compaction, seam, or cycle triggers. |
| Context percent | Active request input estimate divided by the model context window. | Display only; it mirrors the active-input basis used by context safeguards. |
| Cost estimate | Approximate spend from provider usage and configured DeepSeek rates. | Display only. |

For the default V4 path, hard cycles fire when active input reaches the smaller
of the configured cycle threshold (`768000`) and the model window minus reserved
response headroom. Replacement compaction remains opt-in (`auto_compact = false`
by default), the Flash seam manager remains opt-in (`[context].enabled = false`),
and the capacity controller remains disabled unless configured.

### Command Migration Notes

If you are upgrading from older releases:

- Old: `/codesmith`
  New: `/links` (aliases: `/dashboard`, `/api`)
- Old: `/set model deepseek-reasoner`
  New: `/config` and edit the `model` row to `deepseek-v4-pro` or `deepseek-v4-flash`
- Old: visible `Normal` mode or `default_mode = "normal"`
  New: use `Agent` / `default_mode = "agent"`; legacy `normal` still maps to `agent`
- Old: discover `/set` in slash UX/help
  New: use `/config` for editing and `/settings` for read-only inspection

## Key Reference

### Core keys (used by the TUI/engine)

- `provider` (string, optional): `deepseek` (default), `nvidia-nim`, `openai`, `atlascloud`, `wanjie-ark`, `openrouter`, `xiaomi-mimo`, `novita`, `fireworks`, `siliconflow`, `moonshot`, `sglang`, `vllm`, or `ollama`. Legacy `deepseek-cn` configs are still accepted as an alias for `deepseek`; DeepSeek uses the same official host [`https://api.deepseek.com`](https://api-docs.deepseek.com/) worldwide. `nvidia-nim` targets NVIDIA's NIM-hosted DeepSeek endpoints through `https://integrate.api.nvidia.com/v1`; `openai` targets a generic OpenAI-compatible endpoint, defaulting to `https://api.openai.com/v1`; `atlascloud` targets AtlasCloud's OpenAI-compatible endpoint at `https://api.atlascloud.ai/v1`; `wanjie-ark` targets Wanjie Ark's OpenAI-compatible endpoint at `https://maas-openapi.wanjiedata.com/api/v1`; `openrouter` targets `https://openrouter.ai/api/v1`; `xiaomi-mimo` targets Xiaomi MiMo's OpenAI-compatible endpoint at `https://api.xiaomimimo.com/v1`; `novita` targets `https://api.novita.ai/v1`; `fireworks` targets `https://api.fireworks.ai/inference/v1`; `siliconflow` targets SiliconFlow, defaulting to `https://api.siliconflow.com/v1`; `moonshot` targets Moonshot/Kimi, defaulting to `https://api.moonshot.ai/v1`; `sglang` targets a self-hosted OpenAI-compatible endpoint, defaulting to `http://localhost:30000/v1`; `vllm` targets a self-hosted vLLM OpenAI-compatible endpoint, defaulting to `http://localhost:8000/v1`; `ollama` targets Ollama's OpenAI-compatible endpoint, defaulting to `http://localhost:11434/v1`.
- `api_key` (string, required for hosted providers): must be non-empty for DeepSeek/hosted providers (or set the provider API key env var). Self-hosted SGLang, vLLM, and Ollama can omit it.
- `base_url` (string, optional): defaults to `https://api.deepseek.com/beta` for DeepSeek's OpenAI-compatible Chat Completions API, including legacy `provider = "deepseek-cn"` configs. Other defaults are `https://integrate.api.nvidia.com/v1` for `nvidia-nim`, `https://api.openai.com/v1` for `openai`, `https://api.atlascloud.ai/v1` for `atlascloud`, `https://maas-openapi.wanjiedata.com/api/v1` for `wanjie-ark`, `https://openrouter.ai/api/v1` for `openrouter`, `https://api.xiaomimimo.com/v1` for `xiaomi-mimo`, `https://api.novita.ai/v1` for `novita`, `https://api.fireworks.ai/inference/v1` for `fireworks`, `https://api.siliconflow.com/v1` for `siliconflow`, `https://api.moonshot.ai/v1` for `moonshot`, `http://localhost:30000/v1` for `sglang`, `http://localhost:8000/v1` for `vllm`, and `http://localhost:11434/v1` for `ollama`. Set `https://api.deepseek.com` or `https://api.deepseek.com/v1` explicitly to opt out of DeepSeek beta features.
- `default_text_model` (string, optional): defaults to `deepseek-v4-pro` for DeepSeek and generic OpenAI-compatible endpoints, `deepseek-ai/deepseek-v4-pro` for NVIDIA NIM, `deepseek-ai/deepseek-v4-flash` for AtlasCloud, `deepseek-reasoner` for Wanjie Ark, `deepseek/deepseek-v4-pro` for OpenRouter and Novita, `mimo-v2.5-pro` for Xiaomi MiMo, `accounts/fireworks/models/deepseek-v4-pro` for Fireworks, `deepseek-ai/DeepSeek-V4-Pro` for SiliconFlow, `kimi-k2.6` for Moonshot, `deepseek-ai/DeepSeek-V4-Pro` for SGLang/vLLM, and `deepseek-coder:1.3b` for Ollama. Current public DeepSeek IDs are `deepseek-v4-pro` and `deepseek-v4-flash`, both with 1M context windows, 384K max output, and thinking mode enabled by default. Legacy `deepseek-chat` and `deepseek-reasoner` remain compatibility aliases for `deepseek-v4-flash` until July 24, 2026, except SiliconFlow maps `deepseek-reasoner` and `deepseek-r1` to its Pro model while `deepseek-chat` and `deepseek-v3` map to Flash. Provider-specific mappings translate `deepseek-v4-pro` / `deepseek-v4-flash` to each provider's model ID where supported. OpenRouter also recognizes recent large IDs such as `arcee-ai/trinity-large-thinking`, `qwen/qwen3.7-max`, `xiaomi/mimo-v2.5-pro`, `qwen/qwen3.6-35b-a3b`, `google/gemma-4-31b-it`, and `moonshotai/kimi-k2.6`. Generic `openai`, `atlascloud`, `wanjie-ark`, `xiaomi-mimo`, and Ollama model IDs are passed through unchanged. OpenRouter and SiliconFlow provider configs with a custom `base_url` also preserve explicit model values, which lets OpenAI-compatible gateways accept bare model IDs. Use `/models` or `codesmith models` to discover live IDs from your configured endpoint. `CODESMITH_MODEL` overrides this for a single process; `DEEPSEEK_MODEL` is the legacy alias.
- `reasoning_effort` (string, optional): `off`, `low`, `medium`, `high`, or `max`; defaults to the configured UI tier. DeepSeek Platform receives top-level `thinking` / `reasoning_effort` fields. NVIDIA NIM receives equivalent settings through `chat_template_kwargs`.
- `allow_shell` (bool, optional): defaults to `true` (sandboxed).
- `telemetry` (bool, optional, default `false`): opt-in **local-only** telemetry. When `true`, capacity-decision analytics events are written to `~/.codesmith/telemetry/events.jsonl` after the workspace trust boundary passes. Never networked; the sink queues in-memory pre-trust and attaches (writes) only post-trust, so no workspace-controlled data reaches disk before consent. Events carry an ephemeral per-session id, not the durable thread id.
- `approval_policy` (string, optional): `on-request`, `untrusted`, or `never`. Runtime `approval_mode` editing in `/config` also accepts `on-request` and `untrusted` aliases.
- `sandbox_mode` (string, optional): `read-only`, `workspace-write`, `danger-full-access`, `external-sandbox`.
  Platform support is not identical. macOS uses Seatbelt for policy
  enforcement. Linux support is helper-gated around Landlock or optional
  bubblewrap (`prefer_bwrap = true`). Windows does not currently advertise an OS
  sandbox; the planned Windows helper contract starts with process-tree
  containment only and must not be described as read-only filesystem isolation,
  workspace-write enforcement, network blocking, registry isolation, or
  AppContainer isolation until those are implemented. Advanced sandbox controls
  can be set under `[sandbox]`: `enabled`, `fail_if_unavailable`,
  `enabled_platforms`, `excluded_commands`, `auto_allow_bash_if_sandboxed`,
  `prefer_bwrap`, plus `[sandbox.filesystem]` and `[sandbox.network]` tables.
  Shell results report both requested and effective sandbox metadata so fallback
  behavior is explicit.
- `managed_config_path` (string, optional): managed config file loaded after user/env config.
- `requirements_path` (string, optional): requirements file used to enforce allowed approval/sandbox values.
- `max_subagents` (int, optional): defaults to `10` and is clamped to `1..=20`.
- `subagents.*` (optional): per-role/type model defaults for `agent_open` and
  related persistent sub-agent sessions. Explicit tool `model` values win, then role/type
  overrides, then the parent runtime model. Supported convenience keys are
  `default_model`, `worker_model`, `explorer_model`, `awaiter_model`,
  `review_model`, `custom_model`, `max_concurrent`, `api_timeout_secs`, and
  `inherit_full_registry`. The
  `[subagents] max_concurrent` value overrides top-level `max_subagents` and is
  also clamped to `1..=20`; `[subagents] api_timeout_secs` controls the
  per-step API timeout for sub-agent model calls and is clamped to `1..=1800`,
  with `0` or unset preserving the legacy 120 second default.
  `[subagents] inherit_full_registry` (bool, optional, default `false`)
  controls child-permission narrowing (`restrictToSubset`): when `false`, a
  sub-agent's tool surface is a subset of its parent's effective tools —
  children can never call a tool the parent lacks, and a narrowed parent's
  grandchildren inherit the narrow set rather than re-expanding to the full
  surface. Top-level General children still get the full surface (their parent
  is unrestricted), so recursive spawning is preserved. Set `true` to restore
  the legacy v0.6.6 behavior where every child inherits the full agent surface
  regardless of the parent's effective set.
  `[subagents.models]` accepts lower-case role or type keys such as `worker`,
  `explorer`, `general`, `explore`, `plan`, and `review`. Values must normalize
  to a supported DeepSeek model id before an agent is spawned.
- `skills_dir` (string, optional): defaults to `~/.codesmith/skills` (each skill is
  a directory containing `SKILL.md`). Workspace-local `.agents/skills` or
  `./skills` are preferred when present; the runtime also discovers global
  agentskills.io-compatible `~/.agents/skills` and the broader Claude-ecosystem
  `~/.claude/skills`. First launch installs versioned bundled skills for common
  workflows including skill creation, delegation, MCP/plugin scaffolding,
  documents, presentations, spreadsheets, PDFs, and Feishu/Lark.
- `mcp_config_path` (string, optional): defaults to `~/.codesmith/mcp.json`, with
  legacy `~/.deepseek/mcp.json` fallback when the CodeSmith path is absent.
  It is visible in `/config` and can be changed from the TUI. The new path is
  used immediately by `/mcp`, but rebuilding the model-visible MCP tool pool
  requires restarting the TUI.
- `notes_path` (string, optional): defaults to `~/.codesmith/notes.txt`, with
  legacy `~/.deepseek/notes.txt` fallback when the CodeSmith path is absent, and
  is used by the model-visible `note` tool.
- `[memory].enabled` (bool, optional): defaults to `false`. When `true`,
  the TUI loads the user memory file into a `<user_memory>` prompt block,
  enables `# foo` quick-capture in the composer, surfaces the `/memory`
  slash command, and registers the `remember` tool. The same toggle is
  available via `DEEPSEEK_MEMORY=on`.
- `memory_path` (string, optional): defaults to `~/.codesmith/memory.md`, with
  legacy `~/.deepseek/memory.md` fallback when the CodeSmith path is absent.
  Used by the user-memory feature when enabled — see
  [`MEMORY.md`](MEMORY.md) for the full feature surface (`# foo`
  composer prefix, `/memory` slash command, `remember` tool, opt-in
  toggle).
- `snapshots.*` (optional): side-git workspace snapshots for file rollback:
  - `[snapshots].enabled` (bool, default `true`)
  - `[snapshots].max_age_days` (int, default `7`)
  - snapshots live under
    `~/.codesmith/snapshots/<project_hash>/<worktree_hash>/.git`, with legacy
    `~/.deepseek/snapshots/...` fallback when only the legacy state exists, and
    never use the workspace's own `.git` directory
- `context.*` (optional): append-only Fin seam manager, currently opt-in.
  Fin is the fast `deepseek-v4-flash` path with thinking off used for
  coordination work such as routing, summaries, and context maintenance.
  Thresholds use the active request input estimate, not lifetime summed API
  usage:
  - `[context].enabled` (bool, default `false`)
  - `[context].verbatim_window_turns` (int, default `16`)
  - `[context].l1_threshold` (int, default `192000`)
  - `[context].l2_threshold` (int, default `384000`)
  - `[context].l3_threshold` (int, default `576000`)
  - `[context].cycle_threshold` (int, default `768000`)
  - `[context].seam_model` (string, default: the configured `[utility_model]`
    model id, else `deepseek-v4-flash`)
- `retry.*` (optional): retry/backoff settings for API requests:
  - `[retry].enabled` (bool, default `true`)
  - `[retry].max_retries` (int, default `3`)
  - `[retry].initial_delay` (float seconds, default `1.0`)
  - `[retry].max_delay` (float seconds, default `60.0`)
  - `[retry].exponential_base` (float, default `2.0`)
- `capacity.*` (optional): runtime context-capacity controller. This is opt-in
  because its active interventions can rewrite the live transcript.
  - `[capacity].enabled` (bool, default `false`)
  - `[capacity].low_risk_max` (float, default `0.50`)
  - `[capacity].medium_risk_max` (float, default `0.62`)
  - `[capacity].severe_min_slack` (float, default `-0.25`)
  - `[capacity].severe_violation_ratio` (float, default `0.40`)
  - `[capacity].refresh_cooldown_turns` (int, default `6`)
  - `[capacity].replan_cooldown_turns` (int, default `5`)
  - `[capacity].max_replay_per_turn` (int, default `1`)
  - `[capacity].min_turns_before_guardrail` (int, default `4`)
  - `[capacity].profile_window` (int, default `8`)
  - `[capacity].deepseek_v3_2_chat_prior` (float, default `3.9`)
  - `[capacity].deepseek_v3_2_reasoner_prior` (float, default `4.1`)
  - `[capacity].deepseek_v4_pro_prior` (float, default `3.5`)
  - `[capacity].deepseek_v4_flash_prior` (float, default `4.2`)
  - `[capacity].fallback_default_prior` (float, default `3.8`)
- `[notifications].method` (string, optional): `auto`, `osc9`, `bel`, or
  `off`. Defaults to `auto`. The TUI fires this on completed (successful)
  turns whose elapsed time meets `threshold_secs`; failed and cancelled
  turns are silent. `auto` resolves to `osc9` for `iTerm.app`, `Ghostty`,
  and `WezTerm` (detected via `$TERM_PROGRAM`). Otherwise the fallback is
  `bel` on macOS / Linux and `off` on Windows (where BEL maps to the
  system error chime — see the [Notifications](#notifications) section
  for the full rationale, #583).
- `[notifications].threshold_secs` (int, optional): defaults to `30`.
  Only completed turns whose elapsed time meets or exceeds this fire a
  notification.
- `[notifications].include_summary` (bool, optional): defaults to
  `false`. When `true`, the notification body includes the elapsed
  duration and the turn's cost in the configured display currency.
- `tui.alternate_screen` (string, optional): `auto`, `always`, or `never`. This is retained for config compatibility, but interactive sessions now always use the TUI-owned alternate screen so host terminal scrollback cannot hijack the viewport.
- `tui.mouse_capture` (bool, optional, default `true` on non-Windows terminals and on Windows Terminal/ConEmu/Cmder when the alternate screen is active; `false` on legacy Windows console and inside JetBrains JediTerm — PyCharm/IDEA/CLion/etc. — where mouse-event escapes leak into the input stream as garbled text, see #878 / #898): enable internal mouse scrolling, transcript selection, right-click context actions, and transcript scrollbar dragging. TUI-owned drag selection copies only transcript text, removes visual wrap-column line breaks from paragraphs, and keeps selection scoped to the transcript pane. Set this to `false` or run with `--no-mouse-capture` for raw terminal selection; set it to `true` or run with `--mouse-capture` to opt in anywhere it's defaulted off. On raw terminal selection, especially on legacy Windows console or when mouse capture is disabled, selection may cross the right sidebar and include visual wraps because the terminal, not the TUI, owns the selection.
- `tui.terminal_probe_timeout_ms` (int, optional, default `500`): startup terminal-mode probe timeout in milliseconds. Values are clamped to `100..=5000`; timeout emits a warning and aborts startup instead of hanging indefinitely.
- `tui.osc8_links` (bool, optional, default `true`): emit OSC 8 escape sequences around URLs in transcript output so terminals that support them (iTerm2, Terminal.app 13+, Ghostty, Kitty, WezTerm, Alacritty, recent gnome-terminal/konsole) render them as Cmd+click hyperlinks. Terminals without OSC 8 support render the plain URL and ignore the escape. Set `false` for terminals that misrender the sequence; selection/clipboard output always strips the escapes.
- `hooks` (optional): lifecycle hooks configuration (see `config.example.toml`).
- `features.*` (optional): feature flag overrides (see below).

### Workspace notes

`/note` manages a simple notes file in the current workspace at
`.deepseek/notes.md`. Existing `/note <text>` usage still appends a note.
The management forms are:

| Command | Action |
|---|---|
| `/note <text>` | Append a note (legacy shorthand) |
| `/note add <text>` | Append a note explicitly |
| `/note list` | List notes with temporary 1-based numbers |
| `/note show <n>` | Show the full note at number `n` |
| `/note edit <n> <text>` | Replace note `n` with new text |
| `/note remove <n>` | Delete note `n`; `rm` and `delete` are aliases |
| `/note clear` | Empty the workspace notes file |
| `/note path` | Show the resolved workspace notes path |

The numbers shown by `/note list` are not stored in the file; they are derived
from the current order each time notes are read. This keeps the file format
compatible with the existing `---`-separated notes.

### User memory

User memory is split across one top-level path setting and one opt-in
toggle table:

```toml
memory_path = "~/.codesmith/memory.md"

[memory]
enabled = true
```

Notes:

- `memory_path` stays at the top level beside `notes_path` and
  `skills_dir`; it is not nested under `[memory]`.
- `CODESMITH_MEMORY_PATH` overrides the file path from the environment.
- `DEEPSEEK_MEMORY=on` (also `1`, `true`, `yes`, `y`, or `enabled`)
  flips the feature on without editing `config.toml`.
- The feature is inert when disabled: no file is injected, `# foo`
  falls through to normal message submission, and the model does not
  see the `remember` tool.
- See [`MEMORY.md`](MEMORY.md) for examples and the full `/memory`
  command surface.

### Notifications

The TUI can emit a desktop notification (OSC 9 escape or plain BEL) when a turn **completes successfully** and took longer than a threshold, so you can tab away while a long task runs. Failed or cancelled turns are intentionally silent — the notification is a "your task is ready" cue, not a generic ping. Configuration lives under `[notifications]`:

```toml
[notifications]
method          = "auto"  # auto | osc9 | bel | off
threshold_secs  = 30      # only notify when the turn took >= this many seconds
include_summary = false   # include elapsed time + cost in the notification body
```

Method semantics:

- `auto` (default) — picks `osc9` for `iTerm.app`, `Ghostty`, and `WezTerm` (detected via `$TERM_PROGRAM`). On macOS and Linux it falls back to `bel`. **On Windows the fallback is `off`** instead of `bel`, because the Windows audio stack maps `\x07` to the `SystemAsterisk` / `MB_OK` chime — the same sound application error popups use, so a successful-turn notification ends up sounding like an error (#583).
- `osc9` — emit `\x1b]9;<msg>\x07`. Inside tmux the sequence is wrapped in DCS passthrough so it reaches the outer terminal.
- `bel` — emit a single `\x07` byte. Use this on Windows only if you actively want the chime back.
- `off` — disable post-turn notifications entirely.

Windows users who run inside a known OSC-9 terminal (e.g. WezTerm on Windows) keep getting OSC-9 notifications; the `off` fallback only applies when no recognised `TERM_PROGRAM` is detected.

### Parsed but currently unused (reserved for future versions)

These keys are accepted by the config loader but not currently used by the interactive TUI or built-in tools:

- `tools_file`

## Tool Catalog

CodeSmith loads a small core native tool catalog by default and leaves less
common native tools discoverable through ToolSearch. To keep specific native
tools loaded on every request, add them to `[tools].always_load`:

```toml
[tools]
always_load = ["git_show", "notify"]
```

## Feature Flags

Feature flags live under the `[features]` table and are merged across profiles.
Defaults are enabled for built-in tooling, so you only need to set entries you
want to force on or off.

```toml
[features]
shell_tool = true
subagents = true
web_search = true # enables canonical web.run plus the compatibility web_search alias
apply_patch = true
mcp = true
exec_policy = true
```

You can also override features for a single run:

- `codesmith-tui --enable web_search`
- `codesmith-tui --disable subagents`

Use `codesmith-tui features list` to inspect known flags and their effective state.

## Web Search Provider

`web_search` uses DuckDuckGo by default and does not require an API key. The
DuckDuckGo path keeps a Bing fallback when DDG returns a bot challenge or no
parseable results. Bing remains selectable for users who explicitly want it,
and Tavily, Bocha, Metaso, or Baidu can be selected when an API-backed provider
is preferred.

**Metaso** ([metaso.cn](https://metaso.cn)) has a 100 searches/day free quota;
set `METASO_API_KEY` or `[search] api_key` for a higher quota.

**Baidu** uses Baidu AI Search at
`https://qianfan.baidubce.com/v2/ai_search/web_search`. Set
`BAIDU_SEARCH_API_KEY` or `[search] api_key`. This is a search-tool backend
only; it does not add a Baidu model provider.

```toml
[search]
provider = "baidu" # duckduckgo | bing | tavily | bocha | metaso | baidu
# api_key = "YOUR_KEY" # required for tavily, bocha, and baidu; optional for metaso
```

## Local Media Attachments

Use `@path/to/file` in the composer to add local text file or directory context
to the next message. Use `/attach <path>` for local image/video media paths, or
`Ctrl+V` to attach an image from the clipboard. DeepSeek's public Chat
Completions API currently accepts text message content, so media attachments are
sent as explicit local path references instead of native image/video payloads.
Attachment rows appear above the composer before submit; move to the start of
the composer, press `↑` to select an attachment row, then press `Backspace` or
`Delete` to remove it without editing the placeholder text by hand.

## Managed Configuration and Requirements

codesmith supports a policy layering model:

1. user config + profile + env overrides
2. managed config (if present)
3. requirements validation (if present)

By default on Unix:
- managed config: `/etc/deepseek/managed_config.toml`
- requirements: `/etc/deepseek/requirements.toml`

Requirements file shape:

```toml
allowed_approval_policies = ["on-request", "untrusted", "never"]
allowed_sandbox_modes = ["read-only", "workspace-write"]
```

If configured values violate requirements, startup fails with a descriptive error.

See `docs/capacity_controller.md` for formulas, intervention behavior, and telemetry.

## Notes On `codesmith-tui doctor`

`codesmith-tui doctor` follows the same config resolution rules as the rest of the
TUI. That means `--config`, `CODESMITH_CONFIG_PATH`, and the legacy
`DEEPSEEK_CONFIG_PATH` are respected, and MCP/skills
checks use the resolved `mcp_config_path` / `skills_dir` (including env overrides).

To bootstrap missing MCP/skills paths, run `codesmith-tui setup --all`. You can
also run `codesmith-tui setup --skills --local` to create a workspace-local
`./skills` dir.

`codesmith-tui doctor --json` prints a machine-readable report that skips the
live API connectivity probe. Top-level keys: `version`, `config_path`,
`config_present`, `workspace`, `api_key.source`, `base_url`,
`default_text_model`, `mcp`, `skills`, `tools`, `plugins`, `sandbox`,
`platform`, `api_connectivity`, `capability`. CI consumers should rely on `api_key.source`
(`env`/`config`/`missing`) rather than parsing the human-readable `doctor`
text.

The `capability` key contains per-provider capability info derived from
static knowledge (release docs, API guides) rather than live API probes.
Top-level sub-keys: `resolved_provider`, `resolved_model`, `context_window`,
`max_output`, `thinking_supported`, `cache_telemetry_supported`,
and `request_payload_mode`.

Use `capability.context_window` and `capability.max_output` for model-limit
checks in CI scripts; do not treat `capability.max_output` as the per-turn
request budget. Use `capability.thinking_supported` to decide whether to
configure reasoning effort.

## Setup status, clean, and extension dirs

`codesmith-tui setup` accepts a few flags beyond the existing `--mcp`,
`--skills`, `--local`, `--all`, and `--force`:

- `--status` — print a compact one-screen status (api key, base URL, model,
  MCP/skills/tools/plugins counts, sandbox, `.env` presence). Read-only and
  network-free; safe to run in CI. If `.env` is missing and `.env.example` is
  present in the workspace, the status output points at `cp .env.example .env`.
- `--tools` — scaffold `~/.codesmith/tools/` with a `README.md` describing the
  self-describing frontmatter convention (`# name:` / `# description:` /
  `# usage:`) and an `example.sh` that follows it. The directory is
  intentionally not auto-loaded; wire individual scripts into the agent via
  MCP, hooks, or skills.
- `--plugins` — scaffold `~/.codesmith/plugins/` with a `README.md` and an
  `example/PLUGIN.md` placeholder using the same frontmatter shape as
  `SKILL.md`. Plugins are not loaded automatically either; reference them
  from a skill, hook, or MCP wrapper when you want them active.
- `--all` now scaffolds MCP + skills + tools + plugins together.
- `--clean` — list `~/.codesmith/sessions/checkpoints/latest.json` and
  `offline_queue.json` if they exist. Legacy
  `~/.deepseek/sessions/checkpoints/` files are not scanned automatically; set
  `CODESMITH_HOME=~/.deepseek` for a one-off legacy cleanup. Pass `--force` to
  actually remove matched files. This never touches real session history or the
  task queue.

`--status` and `--clean` are mutually exclusive with the scaffold flags.

## Why the engine strips XML/`[TOOL_CALL]` text

codesmith sends and receives tool calls only over the API tool channel
(structured `tool_use` / `tool_call` items). The streaming loop in
`crates/tui/src/core/engine.rs` recognizes a fixed set of fake-wrapper start
markers — `[TOOL_CALL]`, `<codesmith:tool_call`, `<tool_call`, `<invoke `,
`<function_calls>` — and scrubs them from visible assistant text without ever
turning them into structured tool calls. When a wrapper is stripped, the loop
emits one compact `status` notice per turn so the user can see why their
visible text shrank. Treat any change that re-enables text-based tool
execution as a regression; the protocol-recovery tests in
`crates/tui/tests/protocol_recovery.rs` lock the contract.
