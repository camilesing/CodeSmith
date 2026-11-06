/**
 * M0 fake RPC carrier for the DSH connection plugin: the in-memory stub
 * table from DSH's own test fixtures
 * (packages/test-support/client-runtime/src/assembly/remote-default-responses.ts)
 * plus a `$events` ready frame, so the vendored client boots to its empty
 * state with zero backend. M1 replaces the carrier body with Tauri IPC to
 * the dsh_host translation layer — this file is the only seam.
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

export const fakeRpc: ClientConnectionRpc = {
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
