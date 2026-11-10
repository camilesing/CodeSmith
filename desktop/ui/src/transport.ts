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

// Offline mirrors of the desktop bridge's General-settings namespaces. The
// vendored pages validate a namespace against its schemastery envelope, so
// the envelopes are the ones host::settings serves (same field ids).
// The appearance, input-behavior and transcript-view namespaces (the
// General/Settings rows the shell edits). The envelopes are byte-identical
// to `host::settings`'s THEME_SCHEMA / CONVERSATION_SCHEMA / CHAT_SCHEMA —
// the vendored pages validate a namespace against its schemastery envelope,
// so the field ids and defaults must track those constants.
const OFFLINE_THEME_SCHEMA = `{"uid":29,"refs":{"2":{"type":"const","meta":{"required":true},"value":"light"},"4":{"type":"const","meta":{"required":true},"value":"dark"},"6":{"type":"const","meta":{"required":true},"value":"system"},"7":{"type":"union","meta":{"default":"system"},"list":[2,4,6]},"12":{"type":"number","meta":{"step":1,"min":10,"max":22,"default":14}},"17":{"type":"number","meta":{"step":1,"min":10,"max":16,"default":11}},"22":{"type":"number","meta":{"step":1,"min":10,"max":20,"default":13}},"24":{"type":"string","meta":{"default":""}},"26":{"type":"string","meta":{"default":""}},"28":{"type":"string","meta":{"default":""}},"29":{"type":"object","meta":{"default":{}},"dict":{"preference":7,"fontSize":12,"codeFontSize":17,"terminalFontSize":22,"textFontFamily":24,"codeFontFamily":26,"terminalFontFamily":28}}}}`
const OFFLINE_CONVERSATION_SCHEMA = `{"uid":27,"refs":{"23":{"type":"const","meta":{"required":true},"value":"queue"},"25":{"type":"const","meta":{"required":true},"value":"steer"},"26":{"type":"union","meta":{"default":"queue"},"list":[23,25]},"27":{"type":"object","meta":{"default":{}},"dict":{"busyEnter":26}}}}`
const OFFLINE_CHAT_SCHEMA = `{"uid":68,"refs":{"32":{"type":"const","meta":{"required":true},"value":"completion"},"34":{"type":"const","meta":{"required":true},"value":"next-input"},"35":{"type":"union","meta":{"default":"completion"},"list":[32,34]},"38":{"type":"const","meta":{"required":true},"value":"all"},"40":{"type":"const","meta":{"required":true},"value":"final"},"41":{"type":"union","meta":{"default":"all"},"list":[38,40]},"44":{"type":"const","meta":{"required":true},"value":"sidebar"},"46":{"type":"const","meta":{"required":true},"value":"new-tab"},"47":{"type":"union","meta":{"default":"sidebar"},"list":[44,46]},"50":{"type":"const","meta":{"required":true},"value":"compact"},"52":{"type":"const","meta":{"required":true},"value":"detailed"},"53":{"type":"union","meta":{"default":"detailed"},"list":[50,52]},"56":{"type":"const","meta":{"required":true},"value":"compact"},"58":{"type":"const","meta":{"required":true},"value":"standard"},"60":{"type":"const","meta":{"required":true},"value":"detailed"},"62":{"type":"const","meta":{"required":true},"value":"verbose"},"64":{"type":"const","meta":{"required":true},"value":"normal"},"66":{"type":"const","meta":{"required":true},"value":"expanded"},"67":{"type":"union","meta":{"loose":true},"list":[56,58,60,62,64,66]},"68":{"type":"object","meta":{"default":{}},"dict":{"collapseTiming":35,"foldedReplies":41,"linkOpening":47,"performanceUsage":53,"transcriptView":67}}}}`

const OFFLINE_SETTINGS_SCHEMAS: Record<string, unknown> = {
  'ui-settings-general': {
    uid: 2,
    refs: {
      1: { type: 'string', meta: { volatile: true } },
      2: { type: 'object', meta: { default: {} }, dict: { welcomeNoticeVersion: 1 } },
    },
  },
  locale: {
    uid: 3,
    refs: {
      2: { type: 'string', meta: { pattern: { source: '^[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*$', flags: 'u' }, required: false } },
      3: { type: 'object', meta: { default: {} }, dict: { preference: 2 } },
    },
  },
  'ui-settings': {
    uid: 6,
    refs: {
      5: { type: 'boolean', meta: { default: true } },
      6: { type: 'object', meta: { default: {} }, dict: { enabled: 5 } },
    },
  },
  permission: {
    uid: 9,
    refs: {
      8: { type: 'string', meta: { volatile: true } },
      9: { type: 'object', meta: { default: {} }, dict: { defaultPreset: 8 } },
    },
  },
  // Appearance, input behavior and transcript view: the shell-owned rows the
  // Settings pages edit. Parsed from the same envelopes the bridge serves.
  'ui-theme': JSON.parse(OFFLINE_THEME_SCHEMA),
  'ui-conversation': JSON.parse(OFFLINE_CONVERSATION_SCHEMA),
  'ui-chat': JSON.parse(OFFLINE_CHAT_SCHEMA),
}

const OFFLINE_SETTINGS: Record<string, { value: Record<string, unknown>; revision: number }> = {
  // Upstream's current copy version: treating it as already acknowledged
  // keeps the preview notice from blocking the offline page.
  'ui-settings-general': { value: { welcomeNoticeVersion: '2026-09-28.1' }, revision: 0 },
  locale: { value: {}, revision: 0 },
  'ui-settings': { value: { enabled: true }, revision: 0 },
  permission: { value: { defaultPreset: 'agent' }, revision: 0 },
  'ui-theme': {
    value: {
      preference: 'system',
      fontSize: 14,
      codeFontSize: 11,
      terminalFontSize: 13,
      textFontFamily: '',
      codeFontFamily: '',
      terminalFontFamily: '',
    },
    revision: 0,
  },
  'ui-conversation': { value: { busyEnter: 'queue' }, revision: 0 },
  'ui-chat': {
    value: {
      transcriptView: 'detailed',
      collapseTiming: 'completion',
      foldedReplies: 'all',
      linkOpening: 'sidebar',
      performanceUsage: 'detailed',
    },
    revision: 0,
  },
}

function offlineSettingsView(ns: string): unknown {
  const state = OFFLINE_SETTINGS[ns]
  return {
    autoGenerate: true,
    ns,
    schema: OFFLINE_SETTINGS_SCHEMAS[ns],
    // The client freezes the views it receives (its stores are snapshot
    // stores), so the preview hands out a COPY — a later write must never
    // hit a frozen object owned by the page.
    value: structuredClone(state.value),
    applies: 'live',
    secrets: [],
    revision: state.revision,
  }
}

/** Apply the vendored pages' `{op:'set'|'unset', path, value}` ops in memory. */
function offlineSettingsMutate(payload: unknown): RpcResult {
  const args = (payload as { args?: Record<string, unknown> })?.args ?? {}
  const ns = typeof args.ns === 'string' ? args.ns : ''
  const state = OFFLINE_SETTINGS[ns]
  if (state === undefined) {
    return { ok: false, error: { code: 'settings/rejected', message: `offline preview does not serve ${ns}`, details: {} } }
  }
  // `settings/update` carries a flat patch instead of ops.
  const patch = args.patch
  if (typeof patch === 'object' && patch !== null && !Array.isArray(patch)) {
    Object.assign(state.value, patch)
  }
  const ops = Array.isArray(args.ops) ? args.ops : []
  for (const op of ops as { op?: string; path?: unknown[]; value?: unknown }[]) {
    const key = Array.isArray(op.path) && typeof op.path[0] === 'string' ? op.path[0] : undefined
    if (key === undefined) continue
    if (op.op === 'set') state.value[key] = op.value
    if (op.op === 'unset') delete state.value[key]
  }
  state.revision += 1
  return ok(offlineSettingsView(ns))
}

function offlineCatalog(): RpcResult {
  // The bridge localizes these CodeSmith-specific labels from
  // `locale.preference`; the in-memory write is the offline equivalent, so a
  // language picked on the offline Settings page applies on the next fetch.
  const preference = OFFLINE_SETTINGS.locale.value.preference
  const zh = typeof preference === 'string' && preference.toLowerCase().startsWith('zh')
  const options = [
    { value: 'agent', name: zh ? '智能体' : 'Agent' },
    { value: 'plan', name: zh ? '计划' : 'Plan' },
    { value: 'danger-full-access', name: zh ? '完全权限' : 'Full access' },
    { value: 'coordinator', name: zh ? '协调者' : 'Coordinator' },
  ]
  const defaultPreset = typeof OFFLINE_SETTINGS.permission.value.defaultPreset === 'string'
    ? OFFLINE_SETTINGS.permission.value.defaultPreset
    : 'agent'
  return ok({ options, defaultOptions: options, defaultPreset })
}

const UNARY: Record<string, (payload?: unknown) => RpcResult> = {
  'session/list': () => ok({ items: [] }),
  'workspace/initializeDefault': () => ok(undefined),
  'settings/describe': () => ok({
    writable: true,
    hasDocument: false,
    namespaces: Object.keys(OFFLINE_SETTINGS).map(offlineSettingsView),
  }),
  'settings/mutate': payload => offlineSettingsMutate(payload),
  'settings/update': payload => offlineSettingsMutate(payload),
  'session/events': () => ok({ cursor: -1, records: [], hasMore: false }),
  'fileReferences/list': () => ok([]),
  'sessionReferenceResolver/candidates': () => ok([]),
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
  'permissionPresets/catalog': () => offlineCatalog(),
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
  async call(_channel, endpoint, payload, signal) {
    if (signal?.aborted) return { ok: false, error: { code: 'gateway/cancelled', message: 'aborted', details: {} } }
    const handler = UNARY[endpoint]
    return handler ? handler(payload) : unavailable(endpoint)
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
