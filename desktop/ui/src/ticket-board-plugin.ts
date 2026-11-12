/**
 * Coordinator ticket board: the desktop's shared-task header action.
 *
 * The bridge projects CodeSmith's durable coordinator tickets as the vendored
 * client's `agentTeam` projection (`desktop/src-tauri/src/host/tickets.rs`).
 * This first-party plugin renders that projection in the conversation header,
 * imitating the upstream client's shared-task panel: a compact trigger with
 * per-state counts, a popover of read-only rows, and one expanded detail
 * (description + owner) at a time.
 *
 * It is a static module loaded through the vendored module table: `create`
 * receives the loader's `load` function, so React and the primitive
 * components are the client's own instances rather than a second bundle.
 */

type Load = (spec: string) => unknown

interface SlotsService {
  inject: (slot: string, register: () => unknown) => unknown
  register: (definition: Record<string, unknown>, component: (props: any) => unknown) => unknown
}

interface LocaleService {
  register: (ns: string, locale: string, dict: Record<string, string>) => unknown
}

interface PluginContext {
  slots: SlotsService
  locale: LocaleService
}

interface TeamTask {
  id: string
  subject: string
  description?: string
  status: 'pending' | 'in_progress' | 'completed' | 'deleted'
  ownerId?: string
  ownerName?: string
  ready?: boolean
}

interface AgentTeam {
  enabled?: boolean
  tasks?: TeamTask[]
  failure?: string
}

interface BoardProps {
  sessionId: string
  useProjection: (key: string) => AgentTeam | undefined
  t: (key: string, params?: Record<string, string | number>) => string
}

const NS = 'codesmith-ticket-board'

const EN: Record<string, string> = {
  trigger: 'Shared tasks',
  completed: '{count} done',
  active: '{count} active',
  pending: '{count} pending',
  owner: 'Owner',
  unowned: 'Unowned',
  'status.completed': 'Completed',
  'status.active': 'In progress',
  'status.pending': 'Pending',
  'status.blocked': 'Blocked',
  expand: 'Show task details',
  collapse: 'Hide task details',
}

const ZH: Record<string, string> = {
  trigger: '共享任务',
  completed: '{count} 已完成',
  active: '{count} 进行中',
  pending: '{count} 待处理',
  owner: '负责人',
  unowned: '未指派',
  'status.completed': '已完成',
  'status.active': '进行中',
  'status.pending': '待处理',
  'status.blocked': '受阻',
  expand: '展开任务详情',
  collapse: '收起任务详情',
}

const CSS = `
.cds-ticket-trigger {
  display: inline-flex; align-items: center; gap: 4px; min-height: 28px;
  padding: 3px 6px; border: 0; border-radius: var(--dsw-radius-sm);
  background: transparent; color: var(--dsw-alias-label-tertiary);
  cursor: pointer; font-size: 12px; line-height: 18px;
}
.cds-ticket-trigger:focus-visible { color: var(--dsw-alias-label-secondary); background: var(--dsw-alias-interactive-bg-hover); }
@media (hover: hover) {
  .cds-ticket-trigger:hover { color: var(--dsw-alias-label-secondary); background: var(--dsw-alias-interactive-bg-hover); }
}
.cds-ticket-count { white-space: nowrap; }
.cds-ticket-panel {
  position: fixed;
  width: 360px; max-width: min(360px, calc(100vw - 24px));
  padding: 0; overflow: hidden; z-index: 100;
}
.cds-ticket-header {
  display: flex; align-items: baseline; gap: 6px; margin: 0;
  padding: 8px 12px 6px; font-size: 12px; font-weight: 500;
  color: var(--dsw-alias-label-primary);
}
.cds-ticket-summary { color: var(--dsw-alias-label-tertiary); font-size: 11px; }
.cds-ticket-list {
  list-style: none; margin: 0; padding: 4px;
  max-height: min(420px, calc(100vh - 140px)); overflow: auto;
}
.cds-ticket-row {
  display: flex; align-items: center; gap: 7px; width: 100%;
  padding: 7px 6px; border: 0; border-radius: var(--dsw-radius-sm);
  background: transparent; color: var(--dsw-alias-label-primary);
  text-align: left; font-size: 13px; line-height: 18px; cursor: pointer;
}
.cds-ticket-row:focus-visible { background: var(--dsw-alias-fill-l1); }
@media (hover: hover) {
  .cds-ticket-row:hover { background: var(--dsw-alias-fill-l1); }
}
.cds-ticket-subject { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.cds-ticket-chevron { flex: none; color: var(--dsw-alias-label-tertiary); transition: transform 120ms; }
.cds-ticket-row[aria-expanded="true"] .cds-ticket-chevron { transform: rotate(180deg); }
.cds-ticket-details { padding: 0 6px 7px 23px; }
.cds-ticket-description {
  margin: 0 0 4px; color: var(--dsw-alias-label-secondary);
  font-size: 12px; line-height: 18px; white-space: pre-wrap; overflow-wrap: anywhere;
}
.cds-ticket-owner { margin: 0; color: var(--dsw-alias-label-tertiary); font-size: 12px; line-height: 18px; }
.cds-ticket-owner-value { color: var(--dsw-alias-label-secondary); }
.cds-ticket-sr {
  position: absolute; width: 1px; height: 1px; overflow: hidden;
  clip: rect(0 0 0 0); white-space: nowrap;
}
`

function installStyles(): void {
  if (document.querySelector('style[data-codesmith-ticket-board]') !== null) return
  const style = document.createElement('style')
  style.dataset.codesmithTicketBoard = 'true'
  style.textContent = CSS
  document.head.appendChild(style)
}

/** Create the plugin body with the loader's shared module table. */
export function create(load: Load): { inject: string[]; apply: (ctx: PluginContext) => void } {
  return {
    inject: ['slots', 'locale'],
    apply(ctx) {
      const React = load('react') as any
      const primitives = load('@codesmith/client-ui-primitives') as any
      const {
        MenuSurface,
        StateDot,
        Tooltip,
        IconChecklistOutlineRegular,
        IconChevronDownOutlineRegular,
        useAnchoredPosition,
      } = primitives

      installStyles()

      function dotState(task: TeamTask): string {
        if (task.status === 'completed') return 'done'
        if (task.status === 'pending') return task.ready === false ? 'warning' : 'idle'
        return 'ongoing'
      }

      function statusLabel(task: TeamTask, t: BoardProps['t']): string {
        if (task.status === 'completed') return t('status.completed')
        if (task.status === 'pending') return task.ready === false ? t('status.blocked') : t('status.pending')
        return t('status.active')
      }

      function Board(props: BoardProps) {
        const team = props.useProjection('agentTeam')
        const tasks: TeamTask[] = React.useMemo(
          () => (team?.tasks ?? []).filter((task: TeamTask) => task.status !== 'deleted'),
          [team],
        )
        const [open, setOpen] = React.useState(false)
        const [expanded, setExpanded] = React.useState<string | null>(null)
        const triggerRef = React.useRef<HTMLButtonElement>(null)
        const panelRef = React.useRef<HTMLDivElement>(null)
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
          setExpanded(null)
        }, [props.sessionId])

        React.useEffect(() => {
          if (!open) return
          const onPointerDown = (event: PointerEvent) => {
            const target = event.target as Node
            if (!triggerRef.current?.contains(target) && !panelRef.current?.contains(target)) {
              setOpen(false)
              setExpanded(null)
            }
          }
          const onKeyDown = (event: KeyboardEvent) => {
            if (event.key !== 'Escape') return
            setOpen(false)
            setExpanded(null)
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
        if (!team || !team.enabled || tasks.length === 0) return null

        const completed = tasks.filter(task => task.status === 'completed').length
        const active = tasks.filter(task => task.status === 'in_progress').length
        const pending = tasks.length - completed - active
        const parts = [
          completed > 0 ? t('completed', { count: completed }) : '',
          active > 0 ? t('active', { count: active }) : '',
          pending > 0 ? t('pending', { count: pending }) : '',
        ].filter(Boolean)
        const summary = parts.join(' · ')
        const label = summary ? `${t('trigger')} · ${summary}` : t('trigger')

        const trigger = React.createElement(
          Tooltip,
          { label, side: 'bottom', portal: true, delayMs: 500 },
          React.createElement(
            'button',
            {
              ref: triggerRef,
              type: 'button',
              className: 'cds-ticket-trigger',
              'aria-label': label,
              'aria-haspopup': 'dialog',
              'aria-expanded': open,
              onClick: () => {
                setOpen((value: boolean) => !value)
                if (open) setExpanded(null)
              },
            },
            React.createElement(IconChecklistOutlineRegular, { size: 14 }),
            React.createElement('span', { className: 'cds-ticket-count' }, summary),
          ),
        )

        const panel = open
          ? React.createElement(
              MenuSurface,
              {
                ref: panelRef,
                role: 'dialog',
                'aria-label': t('trigger'),
                className: 'cds-ticket-panel',
                style: position ?? { visibility: 'hidden' },
              },
              React.createElement(
                'h3',
                { className: 'cds-ticket-header' },
                React.createElement('span', null, t('trigger')),
                React.createElement('span', { className: 'cds-ticket-summary' }, summary),
              ),
              React.createElement(
                'ul',
                { className: 'cds-ticket-list' },
                tasks.map((task: TeamTask) => {
                  const isOpen = expanded === task.id
                  const owner = task.ownerName ?? task.ownerId ?? t('unowned')
                  return React.createElement(
                    'li',
                    { key: task.id },
                    React.createElement(
                      'button',
                      {
                        type: 'button',
                        className: 'cds-ticket-row',
                        'aria-expanded': isOpen,
                        'aria-label': isOpen ? t('collapse') : t('expand'),
                        onClick: () => setExpanded(isOpen ? null : task.id),
                      },
                      React.createElement(StateDot, { state: dotState(task), size: 10 }),
                      React.createElement('span', { className: 'cds-ticket-subject' }, task.subject),
                      React.createElement('span', { className: 'cds-ticket-sr' }, statusLabel(task, t)),
                      React.createElement(IconChevronDownOutlineRegular, {
                        size: 12,
                        className: 'cds-ticket-chevron',
                      }),
                    ),
                    isOpen
                      ? React.createElement(
                          'div',
                          { className: 'cds-ticket-details' },
                          task.description
                            ? React.createElement('p', { className: 'cds-ticket-description' }, task.description)
                            : null,
                          React.createElement(
                            'p',
                            { className: 'cds-ticket-owner' },
                            `${t('owner')}: `,
                            React.createElement('span', { className: 'cds-ticket-owner-value' }, owner),
                          ),
                        )
                      : null,
                  )
                }),
              ),
            )
          : null

        return React.createElement(React.Fragment, null, trigger, panel)
      }

      const effect = (ctx as { effect?: (fn: () => unknown, label: string) => unknown }).effect
      if (typeof effect === 'function') {
        effect.call(ctx, () => ctx.locale.register(NS, 'en', EN), 'codesmith-ticket-board: en')
        effect.call(ctx, () => ctx.locale.register(NS, 'zh', ZH), 'codesmith-ticket-board: zh')
      } else {
        ctx.locale.register(NS, 'en', EN)
        ctx.locale.register(NS, 'zh', ZH)
      }
      ctx.slots.inject('conversation.session.header.actions', () =>
        ctx.slots.register(
          {
            name: 'conversation.session.header.actions',
            id: 'codesmith-ticket-board',
            order: 15,
            locale: NS,
          },
          Board,
        ),
      )
    },
  }
}