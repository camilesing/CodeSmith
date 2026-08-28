# Feishu / Lark Bridge

This bridge lets a Feishu or Lark chat control a local `codesmith serve --http`
runtime from a phone. It uses the official Lark/Feishu Node SDK long-connection
mode, so the first version does not need a public webhook URL.

Security model:

- `codesmith serve --http` stays bound to `127.0.0.1`.
- `/v1/*` runtime calls use `CODESMITH_RUNTIME_TOKEN`.
- Feishu/Lark chats must be allowlisted unless `CODESMITH_ALLOW_UNLISTED=true`
  is set for first pairing.
- Direct messages are the intended MVP control surface. Group chat control is
  disabled unless `FEISHU_ALLOW_GROUPS=true`.
- Tool approvals are text commands: `/allow <approval_id>` or `/deny <approval_id>`.

## Setup

```bash
cd /opt/codesmith/bridge
npm install --omit=dev
cp .env.example /etc/codesmith/feishu-bridge.env
sudoedit /etc/codesmith/feishu-bridge.env
node src/index.mjs
```

Validate the env files before starting the service:

```bash
npm run validate:config -- \
  --env /etc/codesmith/feishu-bridge.env \
  --runtime-env /etc/codesmith/runtime.env \
  --workspace-root /opt/whalebro \
  --check-filesystem
```

For a Tencent Lighthouse deployment, use:

```bash
sudo systemctl enable --now codesmith-runtime codesmith-feishu-bridge
sudo journalctl -u codesmith-feishu-bridge -f
```

## Commands

- `/status`
- `/threads`
- `/new`
- `/resume <thread_id>`
- `/interrupt`
- `/compact`
- `/allow <approval_id> [remember]`
- `/deny <approval_id>`

Anything else is sent as a prompt. If group control is explicitly enabled,
messages must start with `/ds` by default, for example:

```text
/ds check git status and tell me what is dirty
```
