/**
 * CodeSmith Desktop entry: install the transport globals the vendored client
 * expects, then run the stock AppWebEntry boot (module loader from the
 * bootstrap script in index.html, boot graph, activation audit, mount).
 */
import { AppWebEntry } from '@codesmith/client-web'
import * as brandModule from './brand-plugin'
import { buildBootGraph } from './boot-graph.generated'
import { installHostDropBridge } from './host-drop'
import { create as createTicketBoard } from './ticket-board-plugin'
import { bridgeUrl, initTransport, installApiRouteRedirect } from './transport'

const g = globalThis as {
  __CODESMITH_BOOT__?: unknown
  __CODESMITH_TRANSPORT__?: unknown
  __CODESMITH_REMOTE_ACCESS__?: unknown
  __CODESMITH_FILE_UPLOAD__?: unknown
}

// Attachment uploads post raw bytes to `api/session/uploadFileBinary` on the
// page origin by default (a background Worker); this page's host lives on
// the bridge origin, so install the vendored file-upload hook that carries
// every upload fetch there instead. It must exist before the client boots —
// the FileUploadRuntime service picks its transport once, at construction.
{
  const bridge = bridgeUrl()
  const hook = g as { __CODESMITH_FILE_UPLOAD__?: { fetch: (input: string | URL, init: RequestInit) => Promise<Response> } }
  hook.__CODESMITH_FILE_UPLOAD__ = {
    fetch: (input, init) => fetch(new URL(String(input), `${bridge}/`), init),
  }
  // The drag-drop bridge (host-path citations and staged image bytes) is
  // captured the same way: before boot, because the composer's slot injector
  // reads `__CODESMITH_HOST_PATHS__` once at mount time. The page-relative
  // /api HTTP routes (changes review, present metadata) carry to the bridge
  // origin the same way.
  installHostDropBridge(bridge)
  installApiRouteRedirect(bridge)
}

// Dev affordance for the pixel baseline: `?theme=dark|light` pins the color
// scheme before the vendored theme plugin boots (its dark palette rides
// `prefers-color-scheme` media queries, which a page cannot switch at will).
// No query leaves the real media state alone — the app follows the system.
{
  const requested = new URLSearchParams(globalThis.location?.search).get('theme')
  if (requested === 'dark' || requested === 'light') {
    const nativeMatchMedia = globalThis.matchMedia.bind(globalThis)
    globalThis.matchMedia = (query: string): MediaQueryList =>
      nativeMatchMedia(
        query.replace(/\(prefers-color-scheme:\s*(?:dark|light)\)/g, `(prefers-color-scheme: ${requested})`),
      )
  }
}

// First-party plugin rows ride the module queue: the graph lists them, the
// factory is our statically imported namespace (never fetched over a URL).
// The ticket board borrows the client's own React and primitives through the
// loader's `load` function, so its hooks share the client's React instance.
{
  const loader = g as {
    __ModuleLoader__?: {
      load: (r: { id: string; factory: (load?: (spec: string) => unknown) => unknown }) => void
    }
  }
  loader.__ModuleLoader__?.load?.({ id: '@codesmith/desktop-brand', factory: () => brandModule })
  loader.__ModuleLoader__?.load?.({
    id: '@codesmith/desktop-ticket-board',
    factory: load => createTicketBoard(load ?? (() => {
      throw new Error('codesmith desktop: ticket board requires the client module table')
    })),
  })
}

// The boot graph the loader reads in AppWebEntry.run().
g.__CODESMITH_BOOT__ = buildBootGraph()

// The `??<id>/client.js&rev=` URL form cannot be served by a static server
// (the first `?` starts the query string), so bundles are staged under plain
// `<id>/` paths and this hook rewrites loader URLs onto them. AppWebEntry
// adopts a transport-provided loadBundle automatically.
const resolveBundleUrl = (url: string): string => {
  const marker = '/??'
  const at = url.indexOf(marker)
  if (at >= 0) {
    return `${url.slice(0, at + 1)}${url.slice(at + marker.length).replace(/&rev=[^&]*/, '')}`
  }
  return url.includes('&rev=') ? url.replace(/&rev=[^&]*/, '') : url
}
const loadBundle = (url: string): Promise<void> =>
  new Promise((resolve, reject) => {
    const el = document.createElement('script')
    el.async = true
    el.src = resolveBundleUrl(url)
    el.addEventListener('load', () => { el.remove(); resolve() }, { once: true })
    el.addEventListener('error', () => { el.remove(); reject(new Error(`codesmith desktop: bundle ${url} failed to load`)) }, { once: true })
    document.head.append(el)
  })

// Access baseline: without it every permission check answers false.
g.__CODESMITH_REMOTE_ACCESS__ = { type: 'baseline', owner: true }

const container = document.querySelector<HTMLElement>('#root')
if (container === null) throw new Error('codesmith desktop: #root mount point missing')
initTransport().then(carrier => {
  // Carrier-only transport swap: `rpc` replaces the HTTP unary caller and,
  // without `open`, leaves streams to the vendored WebSocket mux at
  // `streamBaseUrl`; `ownsHost` marks the loopback so the connection reports
  // isLoopback (tauri://localhost on Windows would not qualify by hostname
  // alone).
  g.__CODESMITH_TRANSPORT__ = { ownsHost: true, loadBundle, ...carrier }
  new AppWebEntry(container).run(reason => {
    console.error('codesmith desktop: boot failed', reason)
    const report = document.createElement('pre')
    report.style.cssText = 'position:fixed;inset:auto 12px 12px 12px;z-index:9999;margin:0;padding:12px;background:#2b1d1d;color:#ff9c9c;font:12px/1.5 monospace;white-space:pre-wrap;border-radius:8px'
    report.textContent = `codesmith desktop: boot failed\n${reason instanceof Error ? `${reason.message}\n${reason.stack ?? ''}` : String(reason)}`
    document.body.append(report)
  })
})
