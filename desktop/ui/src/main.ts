/**
 * CodeSmith Desktop entry: install the transport globals the vendored client
 * expects, then run the stock AppWebEntry boot (module loader from the
 * bootstrap script in index.html, boot graph, activation audit, mount).
 */
import { AppWebEntry } from '@codesmith/client-web'
import * as brandModule from './brand-plugin'
import { buildBootGraph } from './boot-graph.generated'
import { initTransport, rpc } from './transport'

const g = globalThis as {
  __CODESMITH_BOOT__?: unknown
  __CODESMITH_TRANSPORT__?: unknown
  __CODESMITH_REMOTE_ACCESS__?: unknown
}

// First-party plugin rows ride the module queue: the graph lists them, the
// factory is our statically imported namespace (never fetched over a URL).
;(g as { __ModuleLoader__?: { load: (r: { id: string; factory: () => unknown }) => void } })
  .__ModuleLoader__?.load?.({ id: '@codesmith/desktop-brand', factory: () => brandModule })

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

// Carrier-only transport swap: `rpc` replaces HTTP entirely; `ownsHost`
// marks the loopback so the connection reports isLoopback (tauri://localhost
// on Windows would not qualify by hostname alone).
g.__CODESMITH_TRANSPORT__ = { ownsHost: true, rpc, loadBundle }
// Access baseline: without it every permission check answers false.
g.__CODESMITH_REMOTE_ACCESS__ = { type: 'baseline', owner: true }

const container = document.querySelector<HTMLElement>('#root')
if (container === null) throw new Error('codesmith desktop: #root mount point missing')
initTransport().finally(() => {
  new AppWebEntry(container).run(reason => {
    console.error('codesmith desktop: boot failed', reason)
    const report = document.createElement('pre')
    report.style.cssText = 'position:fixed;inset:auto 12px 12px 12px;z-index:9999;margin:0;padding:12px;background:#2b1d1d;color:#ff9c9c;font:12px/1.5 monospace;white-space:pre-wrap;border-radius:8px'
    report.textContent = `codesmith desktop: boot failed\n${reason instanceof Error ? `${reason.message}\n${reason.stack ?? ''}` : String(reason)}`
    document.body.append(report)
  })
})
