/**
 * Host-path drops: the shell half of the drag-drop bridge.
 *
 * The vendored composer cites dropped files and folders as `@path` references
 * when the shell answers `__CODESMITH_HOST_PATHS__.pathFor(file)` (upstream's
 * Electron preload). A webview File carries no host path, so the Tauri shell
 * describes each drop to `window.__codesmithHostDrop(payload)` (an eval call
 * from the DragDrop window event — see desktop/src-tauri/src/host/host_drop.rs)
 * and this module turns the description into the composer's own paste intake:
 * a File per member, its path in the bridge's WeakMap, and a duck-typed paste
 * event whose clipboardData the vendored keymap already knows how to read
 * (files, directory entries, empty text).
 *
 * The bridge object must exist before the client boots — the composer's
 * slot injector captures it once at mount time.
 */

/** One member of a `__codesmithHostDrop` payload. */
interface DroppedEntry {
  readonly path: string
  readonly name: string
  readonly isDirectory: boolean
  /** Present only when the shell staged the bytes of an in-cap image. */
  readonly mediaType?: string
  readonly bytesId?: string
}

interface HostDropPayload {
  readonly files: readonly DroppedEntry[]
}

/** The bridge the vendored composer reads (apply.ts `hostPathBridge`). */
interface HostPathBridge {
  pathFor(file: File): string
}

/** The shell-installed entry the Tauri DragDrop handler evals into. */
type HostDropHandler = (payload: HostDropPayload) => void

/** Every delivered File's host path; the bridge answers from here. */
const pathByFile = new WeakMap<File, string>()

/** Every element under `root`, piercing closed shadow roots. */
function deepElements(root: ParentNode): HTMLElement[] {
  const out: HTMLElement[] = []
  const walk = (scope: ParentNode): void => {
    for (const el of scope.querySelectorAll('*')) {
      if (el instanceof HTMLElement) {
        out.push(el)
        if (el.shadowRoot !== null) walk(el.shadowRoot)
      }
    }
  }
  walk(root)
  return out
}

/** The visible session composer's editable, or null when none accepts files. */
function composerInput(): HTMLElement | null {
  for (const el of deepElements(document)) {
    if (!el.hasAttribute('data-composer-input')) continue
    if (el.getAttribute('contenteditable') !== 'true') continue
    if (el.getClientRects().length === 0) continue
    return el
  }
  return null
}

/**
 * Deliver one drop: build each member's File (fetching staged image bytes),
 * remember its host path, and synthesize the composer's paste intake.
 * Unfetchable or unstaged bytes degrade that member to a path citation —
 * a citation never fails because its bytes could not travel.
 */
async function deliverDrop(payload: HostDropPayload, bridge: string): Promise<void> {
  const target = composerInput()
  if (target === null || payload.files.length === 0) return
  const items: {
    kind: 'file'
    getAsFile: () => File
    webkitGetAsEntry: () => { isDirectory: boolean } | null
  }[] = []
  for (const entry of payload.files) {
    let bytes: Blob | null = null
    if (entry.bytesId !== undefined) {
      try {
        const response = await fetch(`${bridge}/api/host/dropBytes/${encodeURIComponent(entry.bytesId)}`)
        if (response.ok) bytes = await response.blob()
      } catch {
        bytes = null // a spent id or a dead bridge degrades to a citation
      }
    }
    // Empty mediaType when the bytes did not travel: the composer must cite
    // the path, not push empty image bytes into its upload pipeline.
    const file = new File([bytes ?? new Blob()], entry.name, {
      type: bytes !== null ? entry.mediaType ?? '' : '',
    })
    pathByFile.set(file, entry.path)
    items.push({
      kind: 'file',
      getAsFile: () => file,
      webkitGetAsEntry: () => (entry.isDirectory ? { isDirectory: true } : null),
    })
  }
  const event = new KeyboardEvent('paste', { cancelable: true })
  Object.defineProperty(event, 'clipboardData', {
    value: { items, getData: () => '' },
  })
  target.focus({ preventScroll: true })
  target.dispatchEvent(event)
}

/**
 * Publish the drag gesture's hover state as the document `dragenter`/
 * `dragleave` the vendored attachment view listens for (its overlay's own
 * signal — the DOM never sees native drag events under wry). The data
 * transfer duck-types a files-only drag; a Drop itself still arrives through
 * `__codesmithHostDrop`'s paste intake, so the drop path also clears the
 * hover (wry emits no Leave after a Drop).
 */
function dragEvent(type: 'dragenter' | 'dragleave' | 'dragend'): DragEvent {
  const event = new Event(type, { cancelable: true, bubbles: true }) as DragEvent
  Object.defineProperty(event, 'dataTransfer', {
    value: { types: ['Files'], items: [], files: [] },
  })
  return event
}

/** The shell-installed hover announcer (see `host_drop::hover_script`). */
type HostDragHoverHandler = (active: boolean) => void

function installHoverBridge(): void {
  const globals = globalThis as { __codesmithHostDragHover?: HostDragHoverHandler }
  globals.__codesmithHostDragHover = active => {
    document.dispatchEvent(dragEvent(active ? 'dragenter' : 'dragleave'))
    if (!active) document.dispatchEvent(dragEvent('dragend'))
  }
}

/**
 * Install `__CODESMITH_HOST_PATHS__`, `__codesmithHostDrop` and
 * `__codesmithHostDragHover` before the client boots. `bridgeBase` is the
 * loopback the staged bytes fetch from.
 */
export function installHostDropBridge(bridgeBase: string): void {
  const globals = globalThis as {
    __CODESMITH_HOST_PATHS__?: HostPathBridge
    __codesmithHostDrop?: HostDropHandler
  }
  globals.__CODESMITH_HOST_PATHS__ = { pathFor: file => pathByFile.get(file) ?? '' }
  globals.__codesmithHostDrop = payload => {
    void deliverDrop(payload, bridgeBase)
    // wry emits no Leave after a Drop: clear the hover the Enter announced.
    ;(globalThis as { __codesmithHostDragHover?: HostDragHoverHandler }).__codesmithHostDragHover?.(false)
  }
  installHoverBridge()
}
