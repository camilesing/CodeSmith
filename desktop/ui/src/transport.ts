/**
 * The connection carrier for CodeSmith Desktop.
 *
 * Primary mode: the client host bridge — a loopback HTTP server inside the
 * Tauri process (127.0.0.1:1421) that implements the vendored client's RPC
 * surface on top of `codesmith serve --http`. Unary calls POST
 * `/api/<endpoint>`; streams ride the vendored client's own WebSocket mux
 * (one socket for every logical stream, `ws://…/api/remote.mux`): this
 * carrier supplies no `open`, so the gateway starts its mux client against
 * `streamBaseUrl`.
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
export function bridgeUrl(): string {
  const injected = (globalThis as { __CODESMITH_BRIDGE_PORT__?: unknown }).__CODESMITH_BRIDGE_PORT__
  const port = typeof injected === 'number' && injected > 0 ? injected : 1421
  return `http://127.0.0.1:${port}`
}

/**
 * Redirect the vendored client's page-relative `/api/…` HTTP routes (the
 * changes-review reads, the present metadata) onto the bridge origin. This
 * page's origin serves no HTTP API — upstream's host process does — so
 * without the redirect every such fetch would 404 against the app bundle.
 * Absolute URLs (the unary RPCs, the mux, uploads) pass through untouched.
 */
export function installApiRouteRedirect(bridge: string): void {
  const bridgeOrigin = new URL(bridge).origin
  const nativeFetch = globalThis.fetch.bind(globalThis)
  globalThis.fetch = (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url =
      typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
    const resolved = new URL(url, globalThis.location?.href ?? '/')
    // Already-absolute bridge URLs (the upload hook) pass through.
    if (resolved.pathname.startsWith('/api/') && resolved.origin !== bridgeOrigin) {
      const target = `${bridge}${resolved.pathname}${resolved.search}`
      return nativeFetch(target, init)
    }
    return nativeFetch(input as RequestInfo, init)
  }
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
    const mediaType = resp.headers.get('content-type')?.split(';', 1)[0]?.trim().toLowerCase()
    if (mediaType === 'multipart/form-data') return parseBinaryResult(await resp.formData())
    return (await resp.json()) as RpcResult
  },
})

/**
 * Decode the bridge's binary-answer form (workspaceFiles/readBytes): a
 * `metadata` field carrying the `{ok, value}` envelope plus an attachments
 * manifest, and one binary part per attachment to plant at its path — the
 * vendored result codecs only accept a real Uint8Array, which JSON cannot
 * carry.
 */
async function parseBinaryResult(form: FormData): Promise<RpcResult> {
  const metadata = form.get('metadata')
  if (typeof metadata !== 'string') {
    return { ok: false, error: { code: 'gateway/internal', message: 'binary response without metadata', details: {} } }
  }
  const envelope = JSON.parse(metadata) as {
    ok: boolean
    value?: unknown
    error?: { code: string; message: string; details: Record<string, unknown> }
    attachments?: { codec: 'bytes'; part: string; path: (string | number)[] }[]
  }
  if (!envelope.ok) {
    return { ok: false, error: envelope.error ?? { code: 'gateway/internal', message: 'binary response failed', details: {} } }
  }
  const root: { value: unknown } = { value: envelope.value }
  for (const attachment of envelope.attachments ?? []) {
    const part = form.get(attachment.part)
    if (!(part instanceof Blob)) {
      return { ok: false, error: { code: 'gateway/internal', message: `binary response lacks part ${attachment.part}`, details: {} } }
    }
    // Plant the bytes at the manifest's path; the envelope carries null at
    // the leaf (the bridge's own contract, both ends ours).
    let parent: Record<string, unknown> | unknown[] = root
    let key: string | number = 'value'
    for (const segment of attachment.path) {
      const container = (parent as Record<string, unknown>)[key]
      if (typeof container !== 'object' || container === null) {
        return { ok: false, error: { code: 'gateway/internal', message: 'binary response path mismatch', details: {} } }
      }
      parent = container as Record<string, unknown> | unknown[]
      key = segment
    }
    ;(parent as Record<string, unknown>)[key] = new Uint8Array(await part.arrayBuffer())
  }
  return { ok: true, value: root.value }
}

function offlineBadge(): void {
  const badge = document.createElement('div')
  badge.textContent = '离线演示模式 — 未连接桌面桥 (127.0.0.1:1421)'
  badge.style.cssText = 'position:fixed;bottom:8px;left:8px;z-index:99999;padding:4px 10px;background:#5a4a17;color:#ffd98a;font:11px/1.4 system-ui;border-radius:6px;pointer-events:none'
  document.body.append(badge)
}

/** The physical-carrier members of the vendored ClientTransportHooks. */
export interface Carrier {
  rpc: ClientConnectionRpc
  /** WebSocket mux base; absent when `rpc.open` carries the streams. */
  streamBaseUrl?: string
}

/** Probe the bridge before the client boots; fall back to the stubs. */
export async function initTransport(): Promise<Carrier> {
  if (await bridgeReady()) return { rpc: bridgeRpc(BRIDGE), streamBaseUrl: `${BRIDGE}/` }
  offlineBadge()
  return { rpc: fakeRpc }
}
