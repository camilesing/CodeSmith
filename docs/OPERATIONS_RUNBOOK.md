# codesmith Operations Runbook

This runbook covers practical debugging and incident response for the local CLI runtime.

## Quick Triage

1. Confirm binary + config:
   - `cargo run -p codesmith-cli -- --version`
   - `cat ~/.codesmith/config.toml` (or inspect configured profile)
2. Enable verbose logs (console-only; there is no file logging):
   - `CODESMITH_LOG_LEVEL=debug cargo run -p codesmith-cli` (or pass `--verbose`; `info`/`trace` also enable it)
   - `RUST_LOG` is not read by the CLI itself — it only reaches spawned child processes
3. Capture current state:
   - `ls ~/.codesmith/sessions`
   - `ls ~/.codesmith/sessions/checkpoints`
   - `ls ~/.codesmith/tasks`

## Incident: Turn Hangs or Stream Stops

Symptoms:
- The REPL status line keeps showing a running turn
- partial assistant output with no completion

Checks:
1. Re-run with `CODESMITH_LOG_LEVEL=debug` (or `--verbose`) and watch the console for retry/health messages
2. Verify endpoint connectivity:
   - `curl -sS https://api.deepseek.com/beta/models -H "Authorization: Bearer $DEEPSEEK_API_KEY"`
3. Confirm no local sandbox/permission deadlock in tool output

Actions:
1. Press `Ctrl+C` to cancel the current turn, including any foreground shell command it is running.
2. If the command was started in the background, ask the assistant to cancel it with `exec_shell_cancel` and the returned task id.
3. Retry the prompt; if it still fails, restart the REPL.
4. On restart, verify the previous queued/in-flight runtime turn is shown as interrupted rather than left in a running state.

## Incident: Network Outage / Offline Behavior

Expected behavior:
- New prompts are queued while offline mode is active
- Queue state persists to `~/.codesmith/sessions/checkpoints/offline_queue.json`

Checks:
1. Run `/queue` in the REPL — the offline queue is a composer-era feature with
   no line-mode equivalent yet; the command reports that messages sent during
   a turn steer/cancel via `Ctrl+C`
2. Confirm persisted queue file exists and updates timestamp

Actions:
1. Restore connectivity
2. Re-send queued entries through the normal input flow (`/queue edit <n>` was
   a composer command with no line-mode equivalent)
3. Ensure queue file clears when queue is empty

## Incident: Crash Recovery Needed

Expected behavior:
- Checkpoint stored at `~/.codesmith/sessions/checkpoints/latest.json`
- Startup begins a fresh session unless `--resume`/`--continue` is supplied

Actions:
1. Resume prior work explicitly via `codesmith --resume <id>`, or `/resume <id>` in the REPL
2. If checkpoint inspection is needed, inspect `latest.json` for schema mismatch/details
3. If schema is newer than binary supports, upgrade binary or remove stale checkpoint

## Incident: Persistent State Schema Errors

Symptoms:
- Errors like `schema vX is newer than supported vY`

Affected stores:
- sessions (`~/.codesmith/sessions/*.json`)
- runtime thread/turn/item records
- tasks (`~/.codesmith/tasks/tasks/*.json`)

Actions:
1. Confirm binary version and migration expectations
2. Back up the state directory before editing
3. Either:
   - run with a newer compatible binary, or
   - archive incompatible records and regenerate state

## Incident: MCP/Tool Execution Failures

Checks:
1. Validate `~/.codesmith/mcp.json` schema and server command paths
2. Confirm server process can start manually
3. Check sandbox denials in the REPL transcript / logs

Actions:
1. Retry with required approvals (or YOLO only when appropriate)
2. Temporarily disable failing MCP server and isolate issue
3. Re-enable after verification with `/mcp` diagnostics

## Telemetry (local jsonl sink)

CodeSmith ships no networked telemetry. The opt-in `telemetry = true` config
flag writes capacity-decision analytics events to a **local-only** jsonl file
at `~/.codesmith/telemetry/events.jsonl`.

- Off by default; the file is never created when the flag is unset or `false`.
- The sink is constructed pre-trust (events queue in-memory) and attaches
  (begins writing) only after the workspace trust boundary passes — so no
  workspace-controlled data reaches disk before consent.
- Events carry an ephemeral `telemetry_session_id` (regenerated per session,
  not persisted); the durable thread id is intentionally not emitted.

To inspect: `cat ~/.codesmith/telemetry/events.jsonl | jq .`
To disable: unset `telemetry` (or set `telemetry = false`) in
`~/.codesmith/config.toml`. The file can be deleted safely at any time.

## Post-Incident Checklist

1. Preserve logs and relevant state files
2. Record trigger, impact, and mitigation
3. Add or update regression tests (retry/recovery/schema)
4. Update this runbook and architecture docs if behavior changed
