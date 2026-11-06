/**
 * The DSH connection carrier for CodeSmith Desktop.
 *
 * Primary mode: the dsh_host bridge — a loopback HTTP server inside the
 * Tauri process (127.0.0.1:1421) that implements the DSH RPC surface on
 * top of `codesmith serve --http`. Unary calls POST `/api/<endpoint>`;
 * streams are NDJSON bodies on `/api/stream/<endpoint>` (fetch streaming,
 * no WebSocket).
 *
 * Fallback mode (browser preview without the desktop shell running): the
 * in-memory stub table from DSH's own test fixtures, with a visible
 * offline-demo badge so the state is never mistaken for the real thing.
 */

export type RpcResult =
  | { ok: true; value: unknown }
  | { ok: false; error: { code: string; message: string; details: Record<string, unknown> } }

export interface ClientConnectionRpc {
  call(channel: string, endpoint: string, payload: unknown, signal?: AbortSignal): Promise<RpcResult>
  open?: (
    channel: string,
    endpoint: string,
    payload: unknown,
    signal: AbortSignal,
    uplink?: AsyncIterable<unknown>,
  ) => AsyncIterable<unknown>
}

const ok = (value: unknown): RpcResult => ({ ok: true, value })
const unavailable = (endpoint: string): RpcResult => ({
  ok: false,
  error: { code: 'gateway/method-unavailable', message: `codesmith desktop: ${endpoint} is not implemented yet`, details: {} },
})

/** Park until the stream is cancelled; lets the client drop an open stream cleanly. */
async function parked(signal: AbortSignal): Promise<void> {
  if (signal.aborted) return
  await new Promise<void>(resolve => signal.addEventListener('abort', () => resolve(), { once: true }))
}

const UNARY: Record<string, () => RpcResult> = {
  'session/list': () => ok({ items: [] }),
  'workspace/initializeDefault': () => ok(undefined),
  'settings/describe': () => ok({ writable: true, hasDocument: false, namespaces: [] }),
  'session/modelCatalog': () => ok({
    default: { provider: 'codesmith', model: 'default' },
    routableProviders: [],
    groups: [{ id: 'codesmith', name: 'CodeSmith', models: [{ id: 'default', name: 'Default' }] }],
    failures: [],
  }),
  'agentPresets/list': () => ok({ presets: [] }),
  'dynamicCordisRunner/syncInspectManifest': () => ok(null),
  'dynamicCordisRunner/inventory': () => ok([]),
  'credentials/describe': () => ok({}),
  'web/listProviders': () => ok({ search: [], fetch: [] }),
  'permissionPresets/catalog': () => ok({ options: [] }),
  'account/getProfile': () => ok(null),
  'account/getBalance': () => ok(null),
  'account/getUnnotifiedBonuses': () => ok(null),
  'account/ackBonusNotified': () => ok(true),
}

const STREAM_BASELINES: Record<string, unknown[]> = {
  'session/control': [{ type: 'baseline', value: { projections: {} } }],
  'account/watch': [{ status: 'signed-out', attempt: null, links: { usageUrl: 'https://platform.deepseek.com/usage', topUpUrl: 'https://platform.deepseek.com/top_up' } }],
  'workspace/follow': [{ type: 'baseline', value: { items: [], archivedSessionIds: [], pinnedSessionIds: [] } }],
  'remoteAccess/follow': [{ type: 'baseline', owner: true }],
}

const fakeRpc: ClientConnectionRpc = {
  async call(_channel, endpoint, _payload, signal) {
    if (signal?.aborted) return { ok: false, error: { code: 'gateway/cancelled', message: 'aborted', details: {} } }
    const handler = UNARY[endpoint]
    return handler ? handler() : unavailable(endpoint)
  },
  async *open(_channel, endpoint, _payload, signal) {
    if (endpoint === '$events') {
      yield { type: 'ready', clientId: crypto.randomUUID(), host: { home: '/codesmith' } }
      await parked(signal)
      return
    }
    const baseline = STREAM_BASELINES[endpoint]
    if (baseline === undefined) {
      throw new Error(`codesmith desktop: stream ${endpoint} is not implemented yet`)
    }
    for (const frame of baseline) yield frame
    await parked(signal)
  },
}

const BRIDGE = `http://127.0.0.1:${globalThis.location?.port === '1421' ? 1422 : 1421}`

async function bridgeHealthy(): Promise<boolean> {
  try {
    const probe = new AbortController()
    const timer = setTimeout(() => probe.abort(), 800)
    const resp = await fetch(`${BRIDGE}/health`, { signal: probe.signal })
    clearTimeout(timer)
    return resp.ok
  } catch {
    return false
  }
}

const bridgeRpc = (bridge: string): ClientConnectionRpc => ({
  async call(_channel, endpoint, payload, signal) {
    const resp = await fetch(`${bridge}/api/${endpoint}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(payload ?? {}),
      signal,
    })
    if (!resp.ok) {
      return { ok: false, error: { code: 'gateway/internal', message: `bridge ${endpoint}: HTTP ${resp.status}`, details: {} } }
    }
    return (await resp.json()) as RpcResult
  },
  async *open(_channel, endpoint, payload, signal, _uplink) {
    const resp = await fetch(`${bridge}/api/stream/${endpoint}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(payload ?? {}),
      signal,
    })
    if (!resp.ok || resp.body === null) {
      throw new Error(`codesmith desktop: stream ${endpoint} refused (HTTP ${resp.status})`)
    }
    const reader = resp.body.getReader()
    const decoder = new TextDecoder()
    let buffer = ''
    try {
      for (;;) {
        const { done, value } = await reader.read()
        if (done) return
        buffer += decoder.decode(value, { stream: true })
        for (;;) {
          const newline = buffer.indexOf('\n')
          if (newline < 0) break
          const line = buffer.slice(0, newline).trim()
          buffer = buffer.slice(newline + 1)
          if (line !== '') yield JSON.parse(line)
        }
      }
    } finally {
      if (!signal.aborted) reader.cancel().catch(() => undefined)
    }
  },
})

function offlineBadge(): void {
  const badge = document.createElement('div')
  badge.textContent = '离线演示模式 — 未连接 dsh bridge (127.0.0.1:1421)'
  badge.style.cssText = 'position:fixed;bottom:8px;left:8px;z-index:99999;padding:4px 10px;background:#5a4a17;color:#ffd98a;font:11px/1.4 system-ui;border-radius:6px;pointer-events:none'
  document.body.append(badge)
}

/** The carrier installed into the DSH connection plugin. */
export const rpc: ClientConnectionRpc = new Proxy({} as ClientConnectionRpc, {
  get(target, prop: keyof ClientConnectionRpc) {
    const delegate = (target as { __delegate?: ClientConnectionRpc }).__delegate
    const impl = delegate?.[prop]
    if (typeof impl !== 'function') {
      throw new Error(`codesmith desktop: rpc carrier not ready (${String(prop)})`)
    }
    return impl.bind(delegate)
  },
})

/** Probe the bridge once before the client boots; fall back to the stubs. */
export async function initTransport(): Promise<void> {
  const holder = rpc as unknown as { __delegate?: ClientConnectionRpc }
  if (await bridgeHealthy()) {
    holder.__delegate = bridgeRpc(BRIDGE)
  } else {
    offlineBadge()
    holder.__delegate = fakeRpc
  }
}
