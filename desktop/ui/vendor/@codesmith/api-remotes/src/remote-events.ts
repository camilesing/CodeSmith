/**
 * The one home of this application's forwarded-Host-event allowlist. Both
 * compiler faces list this file, so the Host forwarding loop and the consumer
 * `ctx.remote.$on` key face read one declaration instead of two copies that
 * could drift; `./types.ts` derives the type projection from it and stays
 * type-only.
 */

import type {} from '@codesmith/api-session-controller/remote-events'
import type { SessionId } from '@codesmith/session/types'
import type {} from '@codesmith/codesmith-account/types'
import type {} from '@codesmith/permission-presets/types'
import type {} from '@codesmith/plugin-manager/types'
import type {} from '@codesmith/plugin-market/types'
import type {} from '@codesmith/schedule/client'
import type { TypertForwardableEventEntry } from '@codesmith/typert-protocol'
import type {} from '@codesmith/web/types'

/**
 * Host events this application forwards without renaming. The explicit mode is
 * both the Host dispatch strategy and the legal key set of `ctx.remote.$on`.
 * An `emit` entry declares readers of the resources its payload carries, or
 * none to touch the built-in `global` resource; it reaches a non-owner Peer
 * only when access control allows it. Each notification names one resource
 * identity or none, and collection changes travel on follow streams instead.
 */
export const API_REMOTE_FORWARDED_EVENTS = [
  { event: 'agent-preset/selected', mode: 'emit' },
  { event: 'approval/request', mode: 'waterfall' },
  { event: 'api-session/activity', mode: 'emit', resources: { session: (sessionId): SessionId => sessionId } },
  { event: 'api-session/added', mode: 'emit', resources: { session: (summary): SessionId => summary.sessionId } },
  { event: 'api-session/error', mode: 'emit', resources: { session: (sessionId): SessionId => sessionId } },
  { event: 'api-session/removed', mode: 'emit', resources: { session: (sessionId): SessionId => sessionId } },
  { event: 'api-session/status', mode: 'emit', resources: { session: (sessionId): SessionId => sessionId } },
  { event: 'commands/change', mode: 'emit' },
  { event: 'codesmith-account/session-expired', mode: 'emit' },
  { event: 'codesmith-account/model-sign-in-required', mode: 'emit' },
  { event: 'credentials/record-updated', mode: 'emit' },
  { event: 'credentials/reference-updated', mode: 'emit' },
  { event: 'goal/activation-changed', mode: 'emit' },
  { event: 'cordis/request-run', mode: 'emit' },
  { event: 'cordis/request-run-resolved', mode: 'emit' },
  { event: 'cordis/dynamic-package', mode: 'emit' },
  { event: 'cordis/dynamic-retract', mode: 'emit' },
  { event: 'cordis/inspect-query', mode: 'emit' },
  { event: 'cordis/inspect-query-resolved', mode: 'emit' },
  { event: 'llm/adapters-updated', mode: 'emit' },
  { event: 'llm/models-updated', mode: 'emit' },
  { event: 'permission-presets/catalog-changed', mode: 'emit' },
  { event: 'plugin-manager/changed', mode: 'emit' },
  { event: 'plugin-manager/install-log', mode: 'emit' },
  { event: 'plugin-manager/install-state', mode: 'emit' },
  { event: 'plugin-manager/update-state', mode: 'emit' },
  { event: 'plugin-market/changed', mode: 'emit' },
  { event: 'settings/document-updated', mode: 'emit' },
  { event: 'schedule/changed', mode: 'emit' },
  { event: 'user-questions/request', mode: 'waterfall' },
  { event: 'web/providers-changed', mode: 'emit' },
] as const satisfies readonly TypertForwardableEventEntry[]
