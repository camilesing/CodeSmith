/**
 * Subagent catalog: the conversation header's child-session switcher.
 *
 * The bridge projects the runtime's durable child records (`origin =
 * "subagent"` threads) as the vendored client's `subagentCatalog` value. This
 * first-party plugin lists the parent session's direct children and opens one
 * through `uiWorkspace.openSession(address)` — the vendored controller then
 * follows the child's own journal. A one-shot child's composer is read-only
 * by the vendored input gate; a continuable child routes `subagents/prompt`
 * to the bridge (which maps it to `agent_send_input`).
 *
 * The coordinator ticket board stays a separate header action: it is a
 * durable work board, not a conversation index, and the upstream catalog
 * vocabulary has no ticket/failure states to carry it. Both borrow the
 * client's React and primitives through the loader table.
 */

type Load = (spec: string) => unknown

interface SlotsService {
  inject: (slot: string, register: () => unknown) => unknown
  register: (definition: Record<string, unknown>, component: (props: any) => unknown) => unknown
}

interface LocaleService {
  register: (ns: string, locale: string, dict: Record<string, string>) => unknown
}

interface UiWorkspaceService {
  openSession: (
    target:
      | string
      | {
          parentSessionId: string
          childSessionId: string
          mode: 'one-shot' | 'continuable' | 'external' | 'unknown'
        },
  ) => void
}

interface SessionsService {
  subagentAddress: (id: string) =>
    | { parentSessionId: string; childSessionId: string; mode: string }
    | undefined
  list: {
    getSnapshot: () => {
      byId: Record<string, { displayTitle?: string } | undefined>
    }
  }
}

interface PluginContext {
  slots: SlotsService
  locale: LocaleService
  uiWorkspace: UiWorkspaceService
  sessions: SessionsService
}

interface CatalogEntry {
  id: string
  createdAt: number
  mode: 'one-shot' | 'continuable' | 'external' | 'unknown'
  label?: string
}

interface SubagentTiming {
  settledMs: number
  active?: { since: number; through: number }
  lastTurnCompleted?: boolean
}

interface CatalogProps {
  sessionId: string
  useProjection: (key: string) => CatalogEntry[] | SubagentTiming | undefined
  t: (key: string, params?: Record<string, string | number>) => string
}

interface LineageProps {
  lineageSessionId?: string
  displayTitle?: string
  t?: (key: string) => string
}

const NS = 'codesmith-subagent-catalog'

const EN: Record<string, string> = {
  trigger: 'Subagents',
  count: '{count}',
  open: 'Open subagent conversation',
  'mode.continuable': 'Continuable',
  'mode.one-shot': 'One-shot',
  'mode.external': 'External',
  'mode.unknown': 'Child',
  empty: 'No subagents yet',
  parent: 'Parent',
  backToParent: 'Back to parent conversation',
}

const ZH: Record<string, string> = {
  trigger: '子代理',
  count: '{count}',
  open: '打开子代理对话',
  'mode.continuable': '可续写',
  'mode.one-shot': '一次性',
  'mode.external': '外部',
  'mode.unknown': '子会话',
  empty: '暂无子代理',
  parent: '父会话',
  backToParent: '返回父会话',
}

const CSS = `
.cds-subagent-trigger {
  display: inline-flex; align-items: center; gap: 4px; min-height: 28px;
  padding: 3px 6px; border: 0; border-radius: var(--dsw-radius-sm);
  background: transparent; color: var(--dsw-alias-label-tertiary);
  cursor: pointer; font-size: 12px; line-height: 18px;
}
.cds-subagent-trigger:focus-visible { color: var(--dsw-alias-label-secondary); background: var(--dsw-alias-interactive-bg-hover); }
@media (hover: hover) {
  .cds-subagent-trigger:hover { color: var(--dsw-alias-label-secondary); background: var(--dsw-alias-interactive-bg-hover); }
}
.cds-subagent-count { white-space: nowrap; }
.cds-subagent-panel {
  position: fixed;
  width: 320px; max-width: min(320px, calc(100vw - 24px));
  padding: 0; overflow: hidden; z-index: 100;
}
.cds-subagent-header {
  display: flex; align-items: baseline; gap: 6px; margin: 0;
  padding: 8px 12px 6px; font-size: 12px; font-weight: 500;
  color: var(--dsw-alias-label-primary);
}
.cds-subagent-summary { color: var(--dsw-alias-label-tertiary); font-size: 11px; }
.cds-subagent-list {
  list-style: none; margin: 0; padding: 4px;
  max-height: min(360px, calc(100vh - 140px)); overflow: auto;
}
.cds-subagent-row {
  display: flex; align-items: center; gap: 7px; width: 100%;
  padding: 7px 6px; border: 0; border-radius: var(--dsw-radius-sm);
  background: transparent; color: var(--dsw-alias-label-primary);
  text-align: left; font-size: 13px; line-height: 18px; cursor: pointer;
}
.cds-subagent-row:focus-visible { background: var(--dsw-alias-fill-l1); }
@media (hover: hover) {
  .cds-subagent-row:hover { background: var(--dsw-alias-fill-l1); }
}
.cds-subagent-label { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.cds-subagent-mode { flex: none; color: var(--dsw-alias-label-tertiary); font-size: 11px; }
.cds-subagent-empty { margin: 0; padding: 10px 12px; color: var(--dsw-alias-label-tertiary); font-size: 12px; }
.cds-subagent-back {
  display: inline-flex; align-items: center; gap: 4px; max-width: 100%;
  padding: 1px 4px; border: 0; border-radius: var(--dsw-radius-sm);
  background: transparent; color: var(--dsw-alias-label-secondary);
  font: inherit; cursor: pointer;
}
.cds-subagent-back:focus-visible { background: var(--dsw-alias-fill-l1); }
@media (hover: hover) {
  .cds-subagent-back:hover { background: var(--dsw-alias-fill-l1); }
}
.cds-subagent-back-label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
`

function installStyles(): void {
  if (document.querySelector('style[data-codesmith-subagent-catalog]') !== null) return
  const style = document.createElement('style')
  style.dataset.codesmithSubagentCatalog = 'true'
  style.textContent = CSS
  document.head.appendChild(style)
}

/** Create the plugin body with the loader's shared module table. */
export function create(load: Load): { inject: string[]; apply: (ctx: PluginContext) => void } {
  return {
    inject: ['slots', 'locale', 'uiWorkspace', 'sessions'],
    apply(ctx) {
      const React = load('react') as any
      const primitives = load('@codesmith/client-ui-primitives') as any
      const {
        MenuSurface,
        StateDot,
        Tooltip,
        IconAgentPresetOutlineRegular,
        useAnchoredPosition,
      } = primitives

      installStyles()

      function modeLabel(entry: CatalogEntry, t: CatalogProps['t']): string {
        return t(`mode.${entry.mode}`)
      }

      function Catalog(props: CatalogProps) {
        const catalog = (props.useProjection('subagentCatalog') as CatalogEntry[] | undefined) ?? []
        const entries = React.useMemo(
          () => catalog.filter((entry: CatalogEntry) => entry.mode !== 'external'),
          [catalog],
        )
        const [open, setOpen] = React.useState(false)
        const triggerRef = React.useRef(null)
        const panelRef = React.useRef(null)
        const position = useAnchoredPosition({
          open,
          anchorRef: triggerRef,
          panelRef,
          side: 'bottom',
          align: 'start',
          gap: 6,
          margin: 12,
        })

        React.useEffect(() => {
          setOpen(false)
        }, [props.sessionId])

        React.useEffect(() => {
          if (!open) return
          const onPointerDown = (event: PointerEvent) => {
            const target = event.target as Node
            if (!triggerRef.current?.contains(target) && !panelRef.current?.contains(target)) {
              setOpen(false)
            }
          }
          const onKeyDown = (event: KeyboardEvent) => {
            if (event.key !== 'Escape') return
            setOpen(false)
            triggerRef.current?.focus()
          }
          document.addEventListener('pointerdown', onPointerDown)
          document.addEventListener('keydown', onKeyDown)
          return () => {
            document.removeEventListener('pointerdown', onPointerDown)
            document.removeEventListener('keydown', onKeyDown)
          }
        }, [open])

        const t = typeof props.t === 'function' ? props.t : (key: string): string => key
        if (entries.length === 0) return null

        const summary = t('count', { count: entries.length })
        const label = `${t('trigger')} · ${summary}`
        const trigger = React.createElement(
          Tooltip,
          { label, side: 'bottom', portal: true, delayMs: 500 },
          React.createElement(
            'button',
            {
              ref: triggerRef,
              type: 'button',
              className: 'cds-subagent-trigger',
              'aria-label': label,
              'aria-haspopup': 'dialog',
              'aria-expanded': open,
              onClick: () => setOpen((value: boolean) => !value),
            },
            React.createElement(IconAgentPresetOutlineRegular, { size: 14 }),
            React.createElement('span', { className: 'cds-subagent-count' }, summary),
          ),
        )

        const panel = open
          ? React.createElement(
              MenuSurface,
              {
                ref: panelRef,
                role: 'dialog',
                'aria-label': t('trigger'),
                className: 'cds-subagent-panel',
                style: position ?? { visibility: 'hidden' },
              },
              React.createElement(
                'h3',
                { className: 'cds-subagent-header' },
                React.createElement('span', null, t('trigger')),
                React.createElement('span', { className: 'cds-subagent-summary' }, summary),
              ),
              React.createElement(
                'ul',
                { className: 'cds-subagent-list' },
                entries.map((entry: CatalogEntry) =>
                  React.createElement(
                    'li',
                    { key: entry.id },
                    React.createElement(
                      'button',
                      {
                        type: 'button',
                        className: 'cds-subagent-row',
                        'aria-label': `${t('open')}: ${entry.label ?? entry.id}`,
                        onClick: () => {
                          setOpen(false)
                          ctx.uiWorkspace.openSession({
                            parentSessionId: props.sessionId,
                            childSessionId: entry.id,
                            mode: entry.mode,
                          })
                        },
                      },
                      React.createElement(StateDot, {
                        state: entry.mode === 'continuable' ? 'ongoing' : 'done',
                        size: 10,
                      }),
                      React.createElement(
                        'span',
                        { className: 'cds-subagent-label' },
                        entry.label ?? entry.id,
                      ),
                      React.createElement(
                        'span',
                        { className: 'cds-subagent-mode' },
                        modeLabel(entry, t),
                      ),
                    ),
                  ),
                ),
              ),
            )
          : null

        return React.createElement(React.Fragment, null, trigger, panel)
      }

      /** Child header lineage: a back control when the crumb is a subagent session. */
      function ParentLink(props: LineageProps) {
        const t = typeof props.t === 'function'
          ? props.t
          : (key: string): string =>
              key === 'parent' ? 'Parent' : 'Back to parent conversation'
        const sessionId = props.lineageSessionId
        const address = sessionId === undefined ? undefined : ctx.sessions.subagentAddress(sessionId)
        if (address === undefined) {
          return React.createElement('span', null, props.displayTitle ?? '')
        }
        const parent = ctx.sessions.list.getSnapshot().byId[address.parentSessionId]
        const parentTitle = parent?.displayTitle ?? t('parent')
        return React.createElement(
          'button',
          {
            type: 'button',
            className: 'cds-subagent-back',
            'aria-label': t('backToParent'),
            title: t('backToParent'),
            onClick: () => ctx.uiWorkspace.openSession(address.parentSessionId),
          },
          React.createElement('span', { 'aria-hidden': true }, '←'),
          React.createElement('span', { className: 'cds-subagent-back-label' }, parentTitle),
        )
      }

      const effect = (ctx as { effect?: (fn: () => unknown, label: string) => unknown }).effect
      if (typeof effect === 'function') {
        effect.call(ctx, () => ctx.locale.register(NS, 'en', EN), 'codesmith-subagent-catalog: en')
        effect.call(ctx, () => ctx.locale.register(NS, 'zh', ZH), 'codesmith-subagent-catalog: zh')
      } else {
        ctx.locale.register(NS, 'en', EN)
        ctx.locale.register(NS, 'zh', ZH)
      }
      ctx.slots.inject('conversation.session.header.actions', () =>
        ctx.slots.register(
          {
            name: 'conversation.session.header.actions',
            id: 'codesmith-subagent-catalog',
            order: 14,
            locale: NS,
          },
          Catalog,
        ),
      )
      // The child breadcrumb gets a back control to the immediate parent;
      // ordinary sessions keep the normal title through the slot fallback.
      ctx.slots.inject('conversation.session.header.lineage', () =>
        ctx.slots.register(
          {
            name: 'conversation.session.header.lineage',
            id: 'codesmith-subagent-parent',
            locale: NS,
          },
          ParentLink,
        ),
      )
    },
  }
}