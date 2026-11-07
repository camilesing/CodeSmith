/**
 * The connection carrier for CodeSmith Desktop.
 *
 * Primary mode: the client host bridge — a loopback HTTP server inside the
 * Tauri process (127.0.0.1:1421) that implements the vendored client's RPC
 * surface on top of `codesmith serve --http`. Unary calls POST
 * `/api/<endpoint>`; streams are NDJSON bodies on `/api/stream/<endpoint>`
 * (fetch streaming, no WebSocket).
 *
 * Fallback mode (browser preview without the desktop shell running): the
 * in-memory stub table from the upstream client's own test fixtures, with a
 * visible offline-demo badge so the state is never mistaken for the real
 * thing.
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
  // CodeSmith has no hosted account: the client still opens this stream, so
  // it gets the signed-out shape with no links to follow.
  'account/watch': [{ status: 'signed-out', attempt: null, links: { usageUrl: '', topUpUrl: '' } }],
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

const BRIDGE = bridgeUrl()

/** The bridge address: the port the shell injected, else the default. */
function bridgeUrl(): string {
  const injected = (globalThis as { __CODESMITH_BRIDGE_PORT__?: unknown }).__CODESMITH_BRIDGE_PORT__
  const port = typeof injected === 'number' && injected > 0 ? injected : 1421
  return `http://127.0.0.1:${port}`
}

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

/**
 * Probe the bridge until it answers or the budget runs out. The shell starts
 * the backend child before the bridge binds, so a single one-shot probe can
 * lose the race on a slow first boot; retrying for a few seconds keeps the
 * real carrier instead of falling into the offline stubs.
 */
async function bridgeReady(): Promise<boolean> {
  const deadline = Date.now() + 5_000
  while (true) {
    if (await bridgeHealthy()) return true
    if (Date.now() >= deadline) return false
    await new Promise(resolve => setTimeout(resolve, 250))
  }
}

/**
 * Every open NDJSON stream registers its controller here. Chromium's network
 * service keeps a streaming fetch's connection ESTABLISHED when the page goes
 * away without cancelling the body, and the per-host pool holds six slots — a
 * reload that strands its streams wedges every later fetch (the next boot
 * stalls inside its connection generation). Aborting on pagehide is the only
 * moment the still-running page can free those slots.
 */
const liveStreamControllers = new Set<AbortController>()
{
  const release = (): void => {
    for (const controller of liveStreamControllers) controller.abort()
    liveStreamControllers.clear()
  }
  globalThis.addEventListener?.('pagehide', release)
  globalThis.addEventListener?.('beforeunload', release)
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
    const controller = new AbortController()
    liveStreamControllers.add(controller)
    const teardown = (): void => { liveStreamControllers.delete(controller) }
    const forwardAbort = (): void => { controller.abort(signal.reason) }
    if (signal.aborted) forwardAbort()
    else signal.addEventListener('abort', forwardAbort, { once: true })
    try {
      const resp = await fetch(`${bridge}/api/stream/${endpoint}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(payload ?? {}),
        signal: controller.signal,
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
        // Always cancel, including on abort: an unread streaming body keeps
        // the connection open in the browser's per-host pool (six slots),
        // wedging every later fetch of this origin group.
        reader.cancel().catch(() => undefined)
      }
    } finally {
      teardown()
      controller.abort()
    }
  },
})

function offlineBadge(): void {
  const badge = document.createElement('div')
  badge.textContent = '离线演示模式 — 未连接桌面桥 (127.0.0.1:1421)'
  badge.style.cssText = 'position:fixed;bottom:8px;left:8px;z-index:99999;padding:4px 10px;background:#5a4a17;color:#ffd98a;font:11px/1.4 system-ui;border-radius:6px;pointer-events:none'
  document.body.append(badge)
}

/** The carrier installed into the vendored connection plugin. */
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

/** Probe the bridge before the client boots; fall back to the stubs. */
export async function initTransport(): Promise<void> {
  const holder = rpc as unknown as { __delegate?: ClientConnectionRpc }
  if (await bridgeReady()) {
    holder.__delegate = bridgeRpc(BRIDGE)
  } else {
    offlineBadge()
    holder.__delegate = fakeRpc
  }
}
