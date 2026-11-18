/**
 * Preset center: the Settings modal's「Presets · 预设」section.
 *
 * Reads the runtime's preset-center snapshot over the bridge
 * (`agentPresets/describe` → `GET /v1/presets`, implemented in
 * `crates/cli/src/preset_center.rs`) and edits it one op per gesture
 * (`agentPresets/apply` → `POST /v1/presets/apply`): tier cards, the diy
 * deviation banner with per-key reset, grouped dials with source badges,
 * the features matrix, and a read-only view of the raw config.toml tree
 * (unknown keys included, secrets masked server-side).
 *
 * Everything is save-on-interact with optimistic update and rollback; edits
 * land in the user's config.toml through toml_edit surgical writes, so
 * comments and unknown keys survive. Changes take effect on the next app
 * launch (the running serve process keeps its startup fold) — the banner
 * says so whenever `restartPending` is set.
 *
 * A static module loaded through the vendored module table: `create`
 * receives the loader's `load`, so React is the client's own instance.
 */

type Load = (spec: string) => unknown

interface SlotsService {
  inject: (slot: string, register: () => unknown) => unknown
  register: (definition: Record<string, unknown>, component: (props: any) => unknown) => unknown
}

interface LocaleService {
  register: (ns: string, locale: string, dict: Record<string, string>) => unknown
  bind?: (ns: string) => (key: string, params?: Record<string, string | number>) => string
}

interface PluginContext {
  slots: SlotsService
  locale: LocaleService
  effect?: (fn: () => unknown, label: string) => unknown
}

// --- wire types (mirror crates/cli/src/preset_center.rs, camelCase serde) ---

interface Baseline {
  tier: string
  value: unknown
}

interface KeyState {
  path: string
  group: 'session' | 'safety' | 'tools' | 'memory' | 'features' | 'general'
  presetGoverned: boolean
  readOnly: boolean
  secret: boolean
  kind: 'enum' | 'bool' | 'string' | 'secret' | 'int' | 'string-list' | 'string-map' | 'table'
  options?: string[] | null
  labelEn: string
  labelZh: string
  descEn: string
  descZh: string
  value: unknown
  set: boolean
  source: 'explicit-file' | 'explicit-other' | 'baseline' | 'default'
  baselines: Baseline[]
  resettable: boolean
}

interface Snapshot {
  tiers: { name: string; description: string; source: string }[]
  selection: {
    selected: string
    selectedSource: string
    effective: string
    deviated: string[]
  }
  keys: KeyState[]
  fileTree: Record<string, unknown>
  restartPending: boolean
  warnings: string[]
}

type Op =
  | { op: 'selectPreset'; name: string }
  | { op: 'set'; path: string; value: unknown }
  | { op: 'unset'; path: string }

interface RpcResult {
  ok: boolean
  value?: Snapshot
  error?: { code: string; message: string }
}

interface RpcLike {
  call: (channel: string, endpoint: string, payload?: unknown, signal?: AbortSignal) => Promise<RpcResult>
}

type Translate = (key: string, params?: Record<string, string | number>) => string

const NS = 'codesmith-preset-center'

const EN: Record<string, string> = {
  nav: 'Presets',
  loading: 'Loading presets…',
  error: 'Could not load the preset center',
  retry: 'Retry',
  restartNotice: 'Edits are written to ~/.codesmith/config.toml and take effect the next time the app launches.',
  restartPending: 'Pending changes — restart the app to apply',
  tiersTitle: 'Tier',
  effective: 'Effective',
  diyBanner: '{count} dials deviate from the {tier} baseline — the effective tier is diy.',
  resetAll: 'Reset all to baseline',
  reset: 'Reset to baseline',
  resettableHint: 'Set outside config.toml (env / flags / project) — edit it there',
  sourceBaseline: 'baseline',
  sourceExplicitFile: 'custom',
  sourceExplicitOther: 'external',
  sourceDefault: 'default',
  groupSession: 'Session & model',
  groupSafety: 'Safety & network',
  groupTools: 'Tools',
  groupMemory: 'Memory & context',
  groupFeatures: 'Feature flags',
  groupGeneral: 'General',
  readOnlyNote: 'Read-only here — see {where}',
  providersWhere: 'the Models page',
  secretSet: 'A value is set',
  secretUnset: 'No value set',
  secretClear: 'Clear',
  listHint: 'one entry per line',
  mapHint: 'one key = value per line',
  fileTreeTitle: 'config.toml (read-only)',
  fileTreeHint: 'Everything else in the file, including keys this page does not catalog. Secrets are masked.',
  refused: 'Change refused',
  tierSourceBuiltin: 'built-in',
  tierSourceUser: 'user',
  tierSourceProject: 'project',
  layering: 'Baselines fill only unset keys; explicit config always wins. Tiers: simple < middle (default) < all < experiment.',
}

const ZH: Record<string, string> = {
  nav: '预设',
  loading: '正在加载预设…',
  error: '无法加载预设中心',
  retry: '重试',
  restartNotice: '更改写入 ~/.codesmith/config.toml，下次启动应用后生效。',
  restartPending: '有待生效的更改——重启应用后应用',
  tiersTitle: '档位',
  effective: '有效档位',
  diyBanner: '{count} 项拨盘偏离 {tier} 基线——有效档位为 diy。',
  resetAll: '全部恢复基线',
  reset: '恢复基线',
  resettableHint: '由 config.toml 之外设置（环境变量 / 启动参数 / 项目覆盖）——请到那里修改',
  sourceBaseline: '基线',
  sourceExplicitFile: '自定义',
  sourceExplicitOther: '外部',
  sourceDefault: '默认',
  groupSession: '会话与模型',
  groupSafety: '安全与网络',
  groupTools: '工具',
  groupMemory: '记忆与上下文',
  groupFeatures: '功能开关',
  groupGeneral: '通用',
  readOnlyNote: '此处只读——见 {where}',
  providersWhere: 'Models 页',
  secretSet: '已设置值',
  secretUnset: '未设置',
  secretClear: '清除',
  listHint: '每行一项',
  mapHint: '每行一个 key = value',
  fileTreeTitle: 'config.toml（只读）',
  fileTreeHint: '文件中的其余内容，包括本页未收录的键。密钥已脱敏。',
  refused: '更改被拒绝',
  tierSourceBuiltin: '内置',
  tierSourceUser: '用户',
  tierSourceProject: '项目',
  layering: '基线只填充未设置的键；显式配置永远优先。档位：simple < middle（默认）< all < experiment。',
}

const CSS = `
.cds-pc-section { display: flex; flex-direction: column; gap: 14px; padding: 4px 2px 12px; font-size: 13px; line-height: 19px; color: var(--dsw-alias-label-primary); }
.cds-pc-notice { color: var(--dsw-alias-label-tertiary); font-size: 12px; line-height: 17px; margin: 0; }
.cds-pc-pending { display: inline-flex; align-items: center; gap: 6px; padding: 3px 9px; border-radius: var(--dsw-radius-sm); background: var(--dsw-alias-fill-warning-soft, rgba(255, 197, 61, 0.14)); color: var(--dsw-alias-label-warning, #9a6b00); font-size: 12px; }
.cds-pc-error { display: flex; flex-direction: column; gap: 8px; align-items: flex-start; color: var(--dsw-alias-label-danger, #d5484f); }
.cds-pc-error-retry { padding: 3px 10px; border: 1px solid var(--dsw-alias-label-tertiary); border-radius: var(--dsw-radius-sm); background: transparent; color: var(--dsw-alias-label-primary); cursor: pointer; font-size: 12px; }
.cds-pc-tiers { display: flex; flex-wrap: wrap; gap: 8px; }
.cds-pc-tier { display: flex; flex-direction: column; gap: 3px; min-width: 128px; padding: 8px 10px; border: 1px solid var(--dsw-alias-line-2, rgba(128,128,128,.4)); border-radius: var(--dsw-radius-md, 8px); background: transparent; color: var(--dsw-alias-label-primary); text-align: left; cursor: pointer; }
.cds-pc-tier:hover, .cds-pc-tier:focus-visible { border-color: var(--dsw-alias-label-secondary); }
.cds-pc-tier[data-active='true'] { border-color: var(--dsw-alias-accent, #4c8dff); background: var(--dsw-alias-fill-l1); }
.cds-pc-tier-name { font-weight: 600; font-size: 13px; }
.cds-pc-tier-desc { color: var(--dsw-alias-label-tertiary); font-size: 11px; line-height: 15px; }
.cds-pc-tier-source { color: var(--dsw-alias-label-tertiary); font-size: 10px; text-transform: uppercase; letter-spacing: .04em; }
.cds-pc-banner { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; padding: 8px 10px; border-radius: var(--dsw-radius-sm); background: var(--dsw-alias-fill-l1); font-size: 12px; }
.cds-pc-banner-chip { padding: 1px 7px; border: 0; border-radius: 999px; background: var(--dsw-alias-fill-l2, rgba(128,128,128,.22)); color: var(--dsw-alias-label-secondary); font-size: 11px; cursor: pointer; }
.cds-pc-banner-reset { margin-left: auto; padding: 3px 10px; border: 1px solid var(--dsw-alias-line-2, rgba(128,128,128,.4)); border-radius: var(--dsw-radius-sm); background: transparent; color: var(--dsw-alias-label-primary); cursor: pointer; font-size: 12px; }
.cds-pc-group { display: flex; flex-direction: column; gap: 6px; }
.cds-pc-group-title { margin: 8px 0 0; font-size: 12px; font-weight: 600; color: var(--dsw-alias-label-secondary); }
.cds-pc-row { display: grid; grid-template-columns: minmax(180px, 1fr) minmax(200px, 1.4fr); gap: 10px; align-items: center; padding: 7px 8px; border-radius: var(--dsw-radius-sm); }
.cds-pc-row:hover { background: var(--dsw-alias-fill-l1); }
.cds-pc-row[data-deviated='true'] { background: var(--dsw-alias-fill-l1); box-shadow: inset 2px 0 0 var(--dsw-alias-label-warning, #9a6b00); }
.cds-pc-row-label { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.cds-pc-row-name { display: flex; align-items: center; gap: 6px; font-weight: 500; }
.cds-pc-row-path { color: var(--dsw-alias-label-tertiary); font-size: 10px; font-family: var(--dsw-alias-font-mono, monospace); overflow: hidden; text-overflow: ellipsis; }
.cds-pc-row-desc { color: var(--dsw-alias-label-tertiary); font-size: 11px; line-height: 15px; }
.cds-pc-badge { flex: none; padding: 0 5px; border-radius: 4px; background: var(--dsw-alias-fill-l2, rgba(128,128,128,.22)); color: var(--dsw-alias-label-tertiary); font-size: 10px; line-height: 15px; }
.cds-pc-badge[data-source='explicit-file'] { color: var(--dsw-alias-label-secondary); }
.cds-pc-control { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; min-width: 0; }
.cds-pc-seg { display: inline-flex; flex-wrap: wrap; gap: 2px; padding: 2px; border-radius: var(--dsw-radius-sm); background: var(--dsw-alias-fill-l1); }
.cds-pc-seg button { padding: 2px 8px; border: 0; border-radius: 4px; background: transparent; color: var(--dsw-alias-label-secondary); font-size: 12px; cursor: pointer; }
.cds-pc-seg button[data-on='true'] { background: var(--dsw-alias-accent, #4c8dff); color: #fff; }
.cds-pc-switch { width: 34px; height: 19px; padding: 0; border: 0; border-radius: 999px; background: var(--dsw-alias-fill-l2, rgba(128,128,128,.35)); position: relative; cursor: pointer; }
.cds-pc-switch[data-on='true'] { background: var(--dsw-alias-accent, #4c8dff); }
.cds-pc-switch::after { content: ''; position: absolute; top: 2px; left: 2px; width: 15px; height: 15px; border-radius: 50%; background: #fff; transition: transform 120ms; }
.cds-pc-switch[data-on='true']::after { transform: translateX(15px); }
.cds-pc-input, .cds-pc-text { padding: 3px 8px; border: 1px solid var(--dsw-alias-line-2, rgba(128,128,128,.4)); border-radius: var(--dsw-radius-sm); background: transparent; color: var(--dsw-alias-label-primary); font-size: 12px; min-width: 0; flex: 1; }
.cds-pc-text { font-family: var(--dsw-alias-font-mono, monospace); font-size: 11px; line-height: 16px; resize: vertical; }
.cds-pc-btn { padding: 3px 9px; border: 1px solid var(--dsw-alias-line-2, rgba(128,128,128,.4)); border-radius: var(--dsw-radius-sm); background: transparent; color: var(--dsw-alias-label-secondary); cursor: pointer; font-size: 11px; }
.cds-pc-btn:hover, .cds-pc-btn:focus-visible { color: var(--dsw-alias-label-primary); border-color: var(--dsw-alias-label-secondary); }
.cds-pc-readonly { color: var(--dsw-alias-label-tertiary); font-size: 12px; font-style: italic; }
.cds-pc-tree { margin: 0; padding: 10px; border-radius: var(--dsw-radius-sm); background: var(--dsw-alias-fill-l1); color: var(--dsw-alias-label-secondary); font-family: var(--dsw-alias-font-mono, monospace); font-size: 11px; line-height: 16px; max-height: 260px; overflow: auto; white-space: pre; }
.cds-pc-toast { position: sticky; bottom: 0; padding: 8px 10px; border-radius: var(--dsw-radius-sm); background: var(--dsw-alias-fill-danger-soft, rgba(213,72,79,.12)); color: var(--dsw-alias-label-danger, #d5484f); font-size: 12px; }
.cds-pc-warn { color: var(--dsw-alias-label-warning, #9a6b00); font-size: 12px; margin: 0; }
`

function installStyles(): void {
  if (document.querySelector('style[data-codesmith-preset-center]') !== null) return
  const style = document.createElement('style')
  style.dataset.codesmithPresetCenter = 'true'
  style.textContent = CSS
  document.head.appendChild(style)
}

function isZh(): boolean {
  const language = String(
    (globalThis as { navigator?: { language?: string } }).navigator?.language ?? 'en',
  )
  return language.toLowerCase().startsWith('zh')
}

/** Render `key = value` lines for the string-map editor. */
function mapToText(value: unknown): string {
  if (value === null || typeof value !== 'object') return ''
  return Object.entries(value as Record<string, unknown>)
    .filter(([, v]) => typeof v === 'string')
    .map(([k, v]) => `${k} = ${String(v)}`)
    .join('\n')
}

function textToMap(text: string): Record<string, string> | null {
  const out: Record<string, string> = {}
  for (const line of text.split('\n')) {
    const trimmed = line.trim()
    if (trimmed === '') continue
    const at = trimmed.indexOf('=')
    if (at <= 0) return null
    out[trimmed.slice(0, at).trim()] = trimmed.slice(at + 1).trim()
  }
  return out
}

function listToText(value: unknown): string {
  return Array.isArray(value) ? value.map(String).join('\n') : ''
}

function textToList(text: string): string[] {
  return text
    .split('\n')
    .map(line => line.trim())
    .filter(line => line !== '')
}

/** Deep clone with one path replaced — the optimistic-update primitive. */
function withValue(snapshot: Snapshot, path: string, value: unknown): Snapshot {
  const keys = snapshot.keys.map(key =>
    key.path === path
      ? { ...key, value, set: value !== null, source: 'explicit-file' as const, resettable: !key.readOnly }
      : key,
  )
  return { ...snapshot, keys, restartPending: true }
}

/** Deep clone with one path removed — the optimistic reset. */
function withoutValue(snapshot: Snapshot, path: string): Snapshot {
  const keys = snapshot.keys.map(key =>
    key.path === path ? { ...key, value: null, set: false, resettable: false } : key,
  )
  return { ...snapshot, keys, restartPending: true }
}

/** Create the plugin body with the loader's shared module table. */
export function create(load: Load): { inject: string[]; apply: (ctx: PluginContext) => void } {
  return {
    inject: ['slots', 'locale'],
    apply(ctx) {
      const React = load('react') as any
      const { useState, useEffect, useCallback, useMemo } = React
      installStyles()

      const transport = (
        globalThis as { __CODESMITH_TRANSPORT__?: { rpc?: RpcLike } }
      ).__CODESMITH_TRANSPORT__
      const rpc = transport?.rpc
      const describe = async (): Promise<RpcResult> =>
        rpc === undefined
          ? { ok: false, error: { code: 'offline', message: 'bridge transport unavailable' } }
          : rpc.call('unary', 'agentPresets/describe', { args: {} })
      const applyOps = async (ops: Op[]): Promise<RpcResult> =>
        rpc === undefined
          ? { ok: false, error: { code: 'offline', message: 'bridge transport unavailable' } }
          : rpc.call('unary', 'agentPresets/apply', { args: { ops } })

      const effect = (ctx as { effect?: PluginContext['effect'] }).effect
      const registerLocale = (locale: string, dict: Record<string, string>): void => {
        if (typeof effect === 'function') {
          effect.call(ctx, () => ctx.locale.register(NS, locale, dict), `preset-center: ${locale}`)
        } else {
          ctx.locale.register(NS, locale, dict)
        }
      }
      registerLocale('en', EN)
      registerLocale('zh', ZH)
      const bound =
        typeof ctx.locale.bind === 'function'
          ? ctx.locale.bind(NS)
          : (key: string) => key
      const t: Translate = (key, params) => {
        const text = bound(key, params)
        return text === key ? EN[key] ?? key : text
      }

      function Switch(props: { on: boolean; disabled?: boolean; onChange: (next: boolean) => void; label: string }) {
        return React.createElement('button', {
          type: 'button',
          role: 'switch',
          'aria-checked': props.on,
          'aria-label': props.label,
          disabled: props.disabled,
          className: 'cds-pc-switch',
          'data-on': String(props.on),
          onClick: () => props.onChange(!props.on),
        })
      }

      function EnumControl(props: { entry: KeyState; onChange: (value: string) => void }) {
        const current = typeof props.entry.value === 'string' ? props.entry.value : ''
        return React.createElement(
          'span',
          { className: 'cds-pc-seg', role: 'radiogroup' },
          (props.entry.options ?? []).map(option =>
            React.createElement(
              'button',
              {
                key: option,
                type: 'button',
                role: 'radio',
                'aria-checked': option === current,
                'data-on': String(option === current),
                onClick: () => props.onChange(option),
              },
              option,
            ),
          ),
        )
      }

      function TextControl(props: { entry: KeyState; commit: (value: string) => void; placeholder?: string }) {
        const [draft, setDraft] = useState(null as string | null)
        const initial = typeof props.entry.value === 'string' ? props.entry.value : ''
        const value = draft ?? initial
        return React.createElement('input', {
          className: 'cds-pc-input',
          type: props.entry.kind === 'secret' ? 'password' : 'text',
          value,
          placeholder: props.entry.secret
            ? props.entry.set
              ? t('secretSet')
              : t('secretUnset')
            : props.placeholder,
          autoComplete: 'off',
          spellCheck: false,
          onChange: (event: { target: { value: string } }) => setDraft(event.target.value),
          onBlur: () => {
            if (draft !== null && draft !== initial) props.commit(draft)
            setDraft(null)
          },
          onKeyDown: (event: { key: string; target: HTMLInputElement }) => {
            if (event.key === 'Enter') event.target.blur()
          },
        })
      }

      function LinesControl(props: { value: unknown; hint: string; commit: (value: unknown) => void; parse: (text: string) => unknown; invalid: string }) {
        const [draft, setDraft] = useState(null as string | null)
        const initial =
          props.value !== null && typeof props.value === 'object'
            ? Array.isArray(props.value)
              ? listToText(props.value)
              : mapToText(props.value)
            : ''
        const value = draft ?? initial
        const [invalid, setInvalid] = useState(false)
        return React.createElement(
          'div',
          { style: { display: 'flex', flexDirection: 'column', gap: 3, flex: 1, minWidth: 0 } },
          React.createElement('textarea', {
            className: 'cds-pc-text',
            rows: 3,
            value,
            spellCheck: false,
            onChange: (event: { target: { value: string } }) => {
              setInvalid(false)
              setDraft(event.target.value)
            },
            onBlur: () => {
              if (draft !== null && draft !== initial) {
                const parsed = props.parse(draft)
                if (parsed === null) setInvalid(true)
                else props.commit(parsed)
              }
              setDraft(null)
            },
          }),
          React.createElement(
            'span',
            { className: 'cds-pc-row-desc' },
            invalid ? props.invalid : props.hint,
          ),
        )
      }

      function KeyRow(props: { entry: KeyState; deviated: boolean; onSet: (path: string, value: unknown) => void; onUnset: (path: string) => void }) {
        const entry = props.entry
        const zh = isZh()
        const label = zh ? entry.labelZh : entry.labelEn
        const desc = zh ? entry.descZh : entry.descEn
        // Deviation shading rides the server's deviation list — the same
        // list the diy banner's chips enumerate (file value vs the selected
        // tier's baseline), so a row is shaded exactly when its chip shows.
        const deviated = props.deviated

        const control = (() => {
          if (entry.readOnly || entry.kind === 'table') {
            return React.createElement(
              'span',
              { className: 'cds-pc-readonly' },
              entry.path === 'providers' || entry.path === 'rhai_loop'
                ? t('readOnlyNote', { where: entry.path === 'providers' ? t('providersWhere') : 'docs/MODS.md' })
                : t('sourceDefault'),
            )
          }
          switch (entry.kind) {
            case 'bool':
              return React.createElement(Switch, {
                on: entry.value === true,
                label,
                onChange: (next: boolean) => props.onSet(entry.path, next),
              })
            case 'enum':
              return React.createElement(EnumControl, {
                entry,
                onChange: (value: string) => props.onSet(entry.path, value),
              })
            case 'int': {
              const initial = typeof entry.value === 'number' ? String(entry.value) : ''
              return React.createElement(TextControl, {
                key: `int-${entry.set}`,
                entry: { ...entry, kind: 'string', value: initial },
                commit: (value: string) => {
                  const parsed = Number.parseInt(value, 10)
                  if (Number.isFinite(parsed)) props.onSet(entry.path, parsed)
                },
              })
            }
            case 'string':
            case 'secret':
              return React.createElement('div', { className: 'cds-pc-control' },
                React.createElement(TextControl, {
                  key: `text-${entry.set}`,
                  entry,
                  commit: (value: string) => {
                    if (value === '') props.onUnset(entry.path)
                    else props.onSet(entry.path, value)
                  },
                }),
                entry.secret && entry.set
                  ? React.createElement('button', {
                      type: 'button',
                      className: 'cds-pc-btn',
                      onClick: () => props.onUnset(entry.path),
                    }, t('secretClear'))
                  : null,
              )
            case 'string-list':
              return React.createElement(LinesControl, {
                value: entry.value,
                hint: t('listHint'),
                invalid: t('listHint'),
                parse: (text: string) => textToList(text),
                commit: (value: unknown) => {
                  if ((value as string[]).length === 0) props.onUnset(entry.path)
                  else props.onSet(entry.path, value)
                },
              })
            case 'string-map':
              return React.createElement(LinesControl, {
                value: entry.value,
                hint: t('mapHint'),
                invalid: t('mapHint'),
                parse: (text: string) => textToMap(text),
                commit: (value: unknown) => {
                  if (Object.keys(value as Record<string, string>).length === 0) props.onUnset(entry.path)
                  else props.onSet(entry.path, value)
                },
              })
            default:
              return null
          }
        })()

        return React.createElement(
          'div',
          { className: 'cds-pc-row', 'data-deviated': String(deviated), id: `cds-pc-key-${entry.path.replace(/\./g, '-')}` },
          React.createElement(
            'div',
            { className: 'cds-pc-row-label' },
            React.createElement(
              'span',
              { className: 'cds-pc-row-name' },
              label,
              React.createElement(
                'span',
                { className: 'cds-pc-badge', 'data-source': entry.source },
                entry.source === 'baseline'
                  ? t('sourceBaseline')
                  : entry.source === 'explicit-file'
                    ? t('sourceExplicitFile')
                    : entry.source === 'explicit-other'
                      ? t('sourceExplicitOther')
                      : t('sourceDefault'),
              ),
            ),
            React.createElement('span', { className: 'cds-pc-row-path' }, entry.path),
            React.createElement('span', { className: 'cds-pc-row-desc' },
              desc,
              entry.source === 'explicit-other' ? ` — ${t('resettableHint')}` : '',
              entry.resettable && entry.source === 'explicit-file'
                ? React.createElement('button', {
                    type: 'button',
                    className: 'cds-pc-btn',
                    style: { marginLeft: 6 },
                    onClick: () => props.onUnset(entry.path),
                  }, t('reset'))
                : null,
            ),
          ),
          React.createElement('div', { className: 'cds-pc-control' }, control),
        )
      }

      function Section() {
        const [snap, setSnap] = useState(null as Snapshot | null)
        const [error, setError] = useState(null as string | null)
        const [toast, setToast] = useState(null as string | null)

        const refresh = useCallback(async () => {
          setError(null)
          const result = await describe()
          if (result.ok && result.value !== undefined) setSnap(result.value)
          else setError(result.error?.message ?? 'unavailable')
        }, [])

        useEffect(() => { void refresh() }, [refresh])

        const run = useCallback(
          async (ops: Op[], optimistic: Snapshot | null) => {
            const previous = snap
            if (optimistic !== null && previous !== null) setSnap(optimistic)
            const result = await applyOps(ops)
            if (result.ok && result.value !== undefined) {
              setSnap(result.value)
              setToast(null)
            } else {
              if (previous !== null) setSnap(previous)
              setToast(`${t('refused')}: ${result.error?.message ?? result.error?.code ?? 'unknown'}`)
            }
          },
          [snap],
        )

        const onSet = useCallback(
          (path: string, value: unknown) => {
            if (snap === null) return
            void run([{ op: 'set', path, value }], withValue(snap, path, value))
          },
          [snap, run],
        )
        const onUnset = useCallback(
          (path: string) => {
            if (snap === null) return
            void run([{ op: 'unset', path }], withoutValue(snap, path))
          },
          [snap, run],
        )
        const onTier = useCallback(
          (name: string) => {
            if (snap === null) return
            void run(
              [{ op: 'selectPreset', name }],
              // A tier gesture usually moves the next launch off the running
              // fold; the fresh snapshot corrects a same-tier re-click back.
              { ...snap, selection: { ...snap.selection, selected: name }, restartPending: true },
            )
          },
          [snap, run],
        )
        const onResetAll = useCallback(() => {
          if (snap === null) return
          const ops: Op[] = snap.selection.deviated
            .filter((path: string) => {
              const key = snap.keys.find((entry: KeyState) => entry.path === path)
              return key !== undefined && key.resettable
            })
            .map((path: string) => ({ op: 'unset' as const, path }))
          if (ops.length === 0) return
          let optimistic = snap
          for (const op of ops) optimistic = withoutValue(optimistic, (op as { path: string }).path)
          void run(ops, { ...optimistic, selection: { ...optimistic.selection, deviated: [] } })
        }, [snap, run])

        if (error !== null) {
          return React.createElement(
            'div',
            { className: 'cds-pc-section' },
            React.createElement('div', { className: 'cds-pc-error' },
              React.createElement('span', null, `${t('error')}: ${error}`),
              React.createElement('button', { type: 'button', className: 'cds-pc-error-retry', onClick: () => void refresh() }, t('retry')),
            ),
          )
        }
        if (snap === null) {
          return React.createElement('div', { className: 'cds-pc-section' }, React.createElement('p', { className: 'cds-pc-notice' }, t('loading')))
        }

        const zh = isZh()
        const deviatedPaths = new Set(snap.selection.deviated)
        const groups: { id: string; title: string }[] = [
          { id: 'session', title: t('groupSession') },
          { id: 'safety', title: t('groupSafety') },
          { id: 'tools', title: t('groupTools') },
          { id: 'memory', title: t('groupMemory') },
          { id: 'features', title: t('groupFeatures') },
          { id: 'general', title: t('groupGeneral') },
        ]
        // `useState` rides an `any` React — narrow at the use site.
        const catalogPaths: string[] = (snap.keys as KeyState[]).map(key => key.path)
        const knownRoots = new Set(catalogPaths.map(path => path.split('.')[0]))
        const extraTree: Record<string, unknown> = {}
        for (const [root, value] of Object.entries(snap.fileTree ?? {})) {
          if (!knownRoots.has(root)) extraTree[root] = value
        }

        return React.createElement(
          'div',
          { className: 'cds-pc-section' },
          React.createElement('p', { className: 'cds-pc-notice' },
            t('restartNotice'),
            snap.restartPending
              ? React.createElement('span', { className: 'cds-pc-pending', style: { marginLeft: 8 } }, t('restartPending'))
              : null,
          ),
          snap.warnings.map((warning: string, index: number) =>
            React.createElement('p', { key: `warn-${index}`, className: 'cds-pc-warn' }, `⚠ ${warning}`),
          ),
          React.createElement(
            'div',
            { className: 'cds-pc-tiers', role: 'radiogroup', 'aria-label': t('tiersTitle') },
            snap.tiers.map((tier: { name: string; description: string; source: string }) =>
              React.createElement(
                'button',
                {
                  key: tier.name,
                  type: 'button',
                  role: 'radio',
                  'aria-checked': tier.name === snap.selection.selected,
                  className: 'cds-pc-tier',
                  'data-active': String(tier.name === snap.selection.selected),
                  onClick: () => onTier(tier.name),
                },
                React.createElement('span', { className: 'cds-pc-tier-name' }, tier.name),
                React.createElement('span', { className: 'cds-pc-tier-desc' }, tier.description),
                React.createElement(
                  'span',
                  { className: 'cds-pc-tier-source' },
                  tier.source === 'builtin' ? t('tierSourceBuiltin') : tier.source === 'user' ? t('tierSourceUser') : t('tierSourceProject'),
                ),
              ),
            ),
          ),
          snap.selection.deviated.length > 0
            ? React.createElement(
                'div',
                { className: 'cds-pc-banner' },
                React.createElement('span', null, t('diyBanner', { count: snap.selection.deviated.length, tier: snap.selection.selected })),
                snap.selection.deviated.map((path: string) =>
                  React.createElement('button', {
                    key: path,
                    type: 'button',
                    className: 'cds-pc-banner-chip',
                    title: path,
                    onClick: () => {
                      document.getElementById(`cds-pc-key-${path.replace(/\./g, '-')}`)?.scrollIntoView({ block: 'center' })
                    },
                  }, path),
                ),
                React.createElement('button', { type: 'button', className: 'cds-pc-banner-reset', onClick: onResetAll }, t('resetAll')),
              )
            : null,
          ...groups.map(group =>
            React.createElement(
              'div',
              { key: group.id, className: 'cds-pc-group' },
              React.createElement('h4', { className: 'cds-pc-group-title' }, group.title),
              ...snap.keys
                .filter((key: KeyState) => key.group === group.id)
                .map((key: KeyState) => React.createElement(KeyRow, { key: key.path, entry: key, deviated: deviatedPaths.has(key.path), onSet, onUnset })),
            ),
          ),
          Object.keys(extraTree).length > 0
            ? React.createElement(
                'div',
                { className: 'cds-pc-group' },
                React.createElement('h4', { className: 'cds-pc-group-title' }, t('fileTreeTitle')),
                React.createElement('p', { className: 'cds-pc-notice' }, t('fileTreeHint')),
                React.createElement('pre', { className: 'cds-pc-tree' }, JSON.stringify(extraTree, null, 2)),
              )
            : null,
          React.createElement('p', { className: 'cds-pc-notice' }, t('layering')),
          toast !== null ? React.createElement('div', { className: 'cds-pc-toast', role: 'alert' }, toast) : null,
        )
      }

      ctx.slots.inject('settings.section', () =>
        ctx.slots.register(
          {
            name: 'settings.section',
            id: 'presets',
            order: 20,
            label: () => t('nav'),
          },
          Section,
        ),
      )
    },
  }
}
