import { createSnapshotStore } from '@codesmith/client-store';
import { childPath, locateFile, relativeFileOf } from "./store.js";
import { RemoteError } from '@codesmith/typert-protocol';
import { DirectoryNode } from "./directory-node.js";
import { FILL_BELOW, FULLSCREEN_QUERY } from "./navigator-layout.js";
/**
 * The box an element is laid out in: its nearest ancestor that generates one,
 * passing over wrappers drawn with `display: contents`.
 * @param element - the laid-out element.
 * @returns that ancestor, or `null` for a detached element.
 */
function layoutParent(element) {
    let parent = element.parentElement;
    while (parent !== null && getComputedStyle(parent).display === 'contents')
        parent = parent.parentElement;
    return parent;
}
/**
 * Bind directory observation to the Remote stream supervisor.
 * @param remote - Client Remote with workspace file streams.
 * @returns a watcher that awaits stream disposal when its node ends.
 */
export function createWatch(remote) {
    return async function* (sessionId, path, signal) {
        const aborted = () => signal.aborted;
        if (aborted())
            return;
        const stream = remote.$stream({
            name: `directory ${path}`,
            open: lifetime => remote.workspaceFiles.changes(sessionId, path, lifetime),
            ended: () => new Error(`Directory watch ended: ${path}`),
        });
        const abort = () => { void stream.dispose(); };
        signal.addEventListener('abort', abort, { once: true });
        try {
            for await (const item of stream) {
                if (aborted())
                    return;
                if (item.value.kind === 'ready')
                    item.accept();
                yield item.value.kind;
            }
        }
        finally {
            signal.removeEventListener('abort', abort);
            await stream.dispose();
        }
    };
}
/**
 * Bind the listing to one Remote face.
 * @param remote - the Client Remote face carrying the `workspaceFiles` namespace.
 * @returns the listing the tree's face performs.
 */
export function createList(remote) {
    return async (sessionId, path, signal) => {
        const result = await remote.workspaceFiles.list(sessionId, path, signal);
        if (!result.ok)
            return result;
        return { ok: true, value: { entries: result.value.entries, truncated: result.value.truncated } };
    };
}
/**
 * Observe whether any registered tab type can open an address.
 * @param registry - the right Sidebar's tab type registry.
 * @returns a source whose snapshot is a predicate, replaced whenever registrations change.
 */
export function canOpenOf(registry) {
    let seen = registry.entries();
    let check = (address) => registry.candidates(address).length > 0;
    return {
        getSnapshot: () => {
            const entries = registry.entries();
            if (entries !== seen) {
                seen = entries;
                check = address => registry.candidates(address).length > 0;
            }
            return check;
        },
        subscribe: listener => registry.subscribe(listener),
    };
}
/** `focus` asked for the tree's keyboard focus. */
const REQUESTED = {};
/**
 * Bind the tree's face to one directory listing.
 * @param list - the bound `workspaceFiles.list` call.
 * @param watch - target-scoped directory observation.
 * @param canOpen - whether some tab type can open an address.
 * @param navigationOf - the attached tab's navigation; subscriptions end with this plugin or their disposer.
 * @returns the face factory: session, its store instance, and where to place the face beside tabs in, face out.
 */
export function filesFace(list, watch, canOpen, navigationOf) {
    return (sessionId, store, place) => {
        const { actions } = store;
        const panes = new Map();
        const record = (paneId) => {
            let pane = panes.get(paneId);
            if (pane === undefined) {
                pane = {
                    lifetime: undefined, mounts: 0, tabs: new Set(), holds: 0, root: undefined, scrollTop: 0,
                    revealed: undefined, revealing: undefined, drawn: undefined, current: undefined, scrollToCurrent: false,
                    focus: undefined, show: false,
                    width: createSnapshotStore(undefined),
                    navigator: undefined, closable: false, unobserve: undefined, toggle: undefined,
                };
                panes.set(paneId, pane);
            }
            return pane;
        };
        /** Each open tab's latest pane. */
        const homes = new Map();
        const forget = (paneId, pane) => {
            pane.lifetime?.abort();
            void pane.root?.close();
            pane.root = undefined;
            pane.unobserve?.();
            panes.delete(paneId);
            actions.forget(paneId);
        };
        // A tab replaced in place goes before its successor mounts, in the same task.
        const settle = (paneId, pane) => {
            if (pane.mounts > 0 || pane.tabs.size > 0)
                return;
            queueMicrotask(() => {
                if (pane.mounts === 0 && pane.tabs.size === 0 && panes.get(paneId) === pane)
                    forget(paneId, pane);
            });
        };
        const leave = (tabId, home) => {
            home.pane.tabs.delete(tabId);
            settle(home.paneId, home.pane);
        };
        const load = async (paneId, path, signal) => {
            const aborted = () => signal.aborted;
            // A closed node has nothing left to write, and its pane's tree may be gone.
            if (aborted())
                return undefined;
            actions.loading(paneId, path);
            const result = await list(sessionId, path, signal);
            if (aborted())
                return undefined;
            if (result.ok)
                actions.loaded(paneId, path, result.value);
            else
                actions.failed(paneId, path, result.error);
            return result.ok ? result.value : undefined;
        };
        // Store writes after which a reveal takes one step, rather than one per write.
        let batching = false;
        const batch = (write) => {
            batching = true;
            try {
                write();
            }
            finally {
                batching = false;
            }
        };
        // A kept level is listed again only once its directory's watch is ready;
        // until that listing settles, a reveal must not take its entries as current.
        const relisting = (paneId, paths) => {
            for (const path of paths) {
                if (store.getSnapshot().byPane[paneId]?.tree?.levels[path]?.kind === 'ready')
                    actions.loading(paneId, path);
            }
        };
        // A record made by a drawing whose navigator never attached, because its
        // tab had closed, has nothing else to end it.
        const drop = (paneId) => {
            const pane = panes.get(paneId);
            if (pane !== undefined && pane.lifetime === undefined)
                settle(paneId, pane);
        };
        // `manual` is the reader's toggle; a reveal's own expansions pass `false`.
        const expand = (paneId, parentPath, path, expanded, manual) => {
            const pane = panes.get(paneId);
            const root = pane?.root;
            if (pane === undefined || root === undefined)
                return;
            const parent = root.find(parentPath);
            if (parent === undefined && !expanded.includes(parentPath))
                return;
            const collapsing = expanded.includes(path);
            // A reader who collapses a directory ends the pane's reveal, so it neither
            // expands that directory again nor replays when the tree is drawn again.
            if (manual && collapsing && pane.revealing !== undefined) {
                pane.revealed = pane.revealing;
                pane.revealing = undefined;
            }
            const next = collapsing ? expanded.filter(value => value !== path) : [...expanded, path];
            root.setExpanded(next);
            if (collapsing)
                void parent?.collapse(path);
            else
                parent?.expand(path, next);
            batch(() => {
                actions.toggled(paneId, path);
                if (!collapsing)
                    relisting(paneId, next.filter(value => value === path || value.startsWith(childPath(path, ''))));
            });
            reveal();
        };
        const focusTree = (pane) => {
            const handoff = {};
            pane.focus = handoff;
            // The dock keeps a newly shown tab's host inert until its own layout
            // effect, later in this drawing's commit, and the tab's body may draw the
            // tree again before the task ends; focus moves once the commit ends, to
            // the drawing in place then.
            queueMicrotask(() => {
                if (pane.focus !== handoff)
                    return;
                pane.focus = undefined;
                const target = pane.current ?? pane.drawn?.body;
                target?.focus();
            });
        };
        // One step of a reveal: expand the next collapsed ancestor, wait while a
        // level on the way lists, or finish. Every store change takes the next step;
        // a kept tree without its listing waits for the hold that lists it again.
        const step = (paneId, pane) => {
            const address = pane.revealing;
            const tree = store.getSnapshot().byPane[paneId]?.tree;
            if (address === undefined || tree === undefined || pane.root === undefined)
                return;
            const path = relativeFileOf(address, sessionId, tree.root);
            const location = path === undefined ? undefined : locateFile(tree, path);
            const next = location?.ancestors.find(ancestor => !tree.expanded.includes(ancestor.path));
            if (next !== undefined) {
                expand(paneId, next.parent, next.path, tree.expanded, false);
                return;
            }
            if (location?.pending === true)
                return;
            pane.revealing = undefined;
            pane.revealed = address;
            if (location?.current === undefined)
                return;
            if (pane.current === undefined)
                pane.scrollToCurrent = true;
            else
                pane.current.scrollIntoView({ block: 'nearest', inline: 'nearest' });
        };
        const reveal = () => {
            for (const [paneId, pane] of panes)
                if (pane.revealing !== undefined)
                    step(paneId, pane);
        };
        store.subscribe(() => { if (!batching)
            reveal(); });
        const open = (paneId, pane, signal, root) => {
            if (pane.root?.path === root)
                return;
            void pane.root?.close();
            const tree = store.getSnapshot().byPane[paneId]?.tree;
            const kept = tree?.root === root ? tree : undefined;
            if (kept === undefined) {
                actions.start(paneId, root);
                pane.scrollTop = 0;
                pane.revealed = undefined;
                pane.revealing = pane.drawn?.address;
            }
            // The node's own lifetime ends with the pane, the last hold, or a later root replacing it.
            const node = new DirectoryNode(root, (path, lifetime) => load(paneId, path, lifetime), (path, lifetime) => watch(sessionId, path, lifetime), (path, error) => {
                if (pane.root === node)
                    actions.failed(paneId, path, new RemoteError('gateway/internal', error instanceof Error ? error.message : String(error), {}));
            }, signal, kept?.expanded);
            if (kept !== undefined)
                node.setAutomatic(kept.autoRefresh);
            pane.root = node;
            node.open();
            if (kept !== undefined)
                batch(() => { relisting(paneId, kept.expanded); });
            step(paneId, pane);
        };
        // A navigation belongs to its target tab, not the tree that initiated it.
        const answerNavigation = (home) => {
            if (home.mounts === 0)
                return;
            const navigation = home.navigation.getSnapshot();
            if (navigation.revision <= home.revision)
                return;
            const { pane, paneId } = home;
            if (store.getSnapshot().byPane[paneId]?.open !== true) {
                home.revision = navigation.revision;
                return;
            }
            const navigator = pane.navigator;
            if (navigator === undefined || pane.drawn?.address !== navigation.address)
                return;
            const row = layoutParent(navigator);
            if (row === null)
                return;
            home.revision = navigation.revision;
            const window = row.ownerDocument.defaultView;
            if (pane.closable && (row.getBoundingClientRect().width < FILL_BELOW
                || (typeof window?.matchMedia === 'function' && window.matchMedia(FULLSCREEN_QUERY).matches)))
                face.hide(paneId);
        };
        const answerDrawn = (pane) => {
            for (const home of homes.values())
                if (home.pane === pane)
                    answerNavigation(home);
        };
        const face = {
            attach(paneId, tabId, tabSignal) {
                if (tabSignal.aborted) {
                    drop(paneId);
                    return () => { };
                }
                const pane = record(paneId);
                pane.lifetime ??= new AbortController();
                if (pane.show) {
                    pane.show = false;
                    actions.shown(paneId, true);
                }
                pane.mounts += 1;
                let home = homes.get(tabId);
                if (home === undefined) {
                    const first = { paneId, pane, mounts: 0, revision: 0, navigation: navigationOf(sessionId, tabId) };
                    home = first;
                    homes.set(tabId, first);
                    const unsubscribe = first.navigation.subscribe(() => { answerNavigation(first); });
                    tabSignal.addEventListener('abort', () => {
                        unsubscribe();
                        homes.delete(tabId);
                        leave(tabId, first);
                    }, { once: true });
                }
                else if (home.pane !== pane) {
                    leave(tabId, home);
                    home.paneId = paneId;
                    home.pane = pane;
                }
                home.mounts += 1;
                pane.tabs.add(tabId);
                answerNavigation(home);
                let attached = true;
                return () => {
                    if (!attached)
                        return;
                    attached = false;
                    pane.mounts -= 1;
                    home.mounts -= 1;
                    settle(paneId, pane);
                };
            },
            placeBeside: tabId => place(tabId, face),
            hold(paneId, root) {
                const pane = panes.get(paneId);
                if (pane?.lifetime === undefined)
                    return () => { };
                pane.holds += 1;
                open(paneId, pane, pane.lifetime.signal, root);
                let held = true;
                return () => {
                    if (!held)
                        return;
                    held = false;
                    pane.holds -= 1;
                    // A tab switch draws the next tree in the same commit, which holds it again first.
                    queueMicrotask(() => {
                        if (pane.holds > 0)
                            return;
                        void pane.root?.close();
                        pane.root = undefined;
                    });
                };
            },
            refresh: (paneId) => { void panes.get(paneId)?.root?.refreshTree(); },
            setAutoRefresh: (paneId, enabled) => {
                actions.autoRefresh(paneId, enabled);
                panes.get(paneId)?.root?.setAutomatic(enabled);
            },
            toggle: (paneId, parentPath, path, expanded) => { expand(paneId, parentPath, path, expanded, true); },
            scrolled: (paneId, scrollTop) => {
                const pane = panes.get(paneId);
                if (pane?.lifetime !== undefined)
                    pane.scrollTop = scrollTop;
            },
            hide(paneId) {
                const pane = panes.get(paneId);
                // The store keeps records only for attached panes.
                if (pane?.lifetime === undefined)
                    return;
                const navigator = pane.navigator;
                const held = pane.focus !== undefined || navigator?.contains(navigator.ownerDocument.activeElement) === true;
                pane.focus = undefined;
                actions.shown(paneId, false);
                // The tab an open lands on draws its own toggle in the commit React
                // flushes in a microtask queued before this one, so this focus finds it.
                if (held)
                    queueMicrotask(() => { panes.get(paneId)?.toggle?.focus(); });
            },
            focus(paneId) {
                // A tab that just moved draws in its new pane before that drawing's
                // effects attach it, so its record, made by the drawing, is enough; the
                // store records the navigator shown once the pane attaches.
                const pane = panes.get(paneId);
                if (pane === undefined)
                    return;
                if (pane.lifetime === undefined)
                    pane.show = true;
                else
                    actions.shown(paneId, true);
                if (pane.drawn === undefined)
                    pane.focus = REQUESTED;
                else
                    focusTree(pane);
            },
            bodyDrawn(paneId, address, body) {
                if (body === null) {
                    // A drawing goes after its pane may have been forgotten; that leaves nothing to record.
                    const pane = panes.get(paneId);
                    if (pane === undefined)
                        return;
                    const gone = pane.drawn?.body;
                    pane.drawn = undefined;
                    pane.revealing = undefined;
                    pane.scrollToCurrent = false;
                    drop(paneId);
                    if (gone?.contains(gone.ownerDocument.activeElement) !== true)
                        return;
                    // Only a drawing in the same commit, the next tab's, takes the keyboard over.
                    const handoff = {};
                    pane.focus = handoff;
                    queueMicrotask(() => { if (pane.focus === handoff)
                        pane.focus = undefined; });
                    return;
                }
                const pane = record(paneId);
                pane.drawn = { body, address };
                body.scrollTop = pane.scrollTop;
                if (pane.focus !== undefined)
                    focusTree(pane);
                pane.revealing = pane.revealed === address ? undefined : address;
                step(paneId, pane);
            },
            currentDrawn(paneId, row) {
                if (row === null) {
                    const pane = panes.get(paneId);
                    if (pane !== undefined)
                        pane.current = undefined;
                    drop(paneId);
                    return;
                }
                const pane = record(paneId);
                pane.current = row;
                if (!pane.scrollToCurrent)
                    return;
                pane.scrollToCurrent = false;
                row.scrollIntoView({ block: 'nearest', inline: 'nearest' });
            },
            navigatorDrawn(paneId, navigator, closable) {
                const pane = navigator === null ? panes.get(paneId) : record(paneId);
                if (pane === undefined)
                    return;
                pane.unobserve?.();
                pane.unobserve = undefined;
                pane.navigator = navigator ?? undefined;
                pane.closable = closable;
                if (navigator === null) {
                    drop(paneId);
                    return;
                }
                const row = layoutParent(navigator);
                if (row === null)
                    return;
                const document = row.ownerDocument;
                // The resize edge while it holds the keyboard. A width at which the
                // tree fills the row removes it, and the fullscreen viewport hides it,
                // which blurs it before the row reports its new width; either way the
                // keyboard moves to the tree.
                let edge;
                const handOff = () => {
                    const held = edge;
                    if (held === undefined)
                        return;
                    const active = document.activeElement;
                    if (active !== held && active !== document.body && active !== null) {
                        edge = undefined;
                        return;
                    }
                    // An edge still drawn keeps the mark only while it still holds the keyboard.
                    if (held.isConnected && getComputedStyle(held).display !== 'none') {
                        if (active !== held)
                            edge = undefined;
                        return;
                    }
                    edge = undefined;
                    focusTree(pane);
                };
                const focusin = (event) => {
                    edge = event.target instanceof Element && event.target.hasAttribute('data-files-navigator-resize') ? event.target : undefined;
                };
                const focusout = (event) => {
                    if (event.relatedTarget === null)
                        queueMicrotask(handOff);
                    else
                        edge = undefined;
                };
                const viewport = closable && typeof matchMedia === 'function' ? matchMedia(FULLSCREEN_QUERY) : undefined;
                let filling = false;
                const hidden = (element) => getComputedStyle(element).visibility === 'hidden';
                // Covered content that a fill's `visibility: hidden` blurred to nowhere,
                // until the next measurement or until the reader moves the keyboard or
                // the pointer, or the window loses focus. The browser may blur it in a
                // task before the observer or the media query reports the fill.
                let covered;
                const release = (event) => {
                    const target = event.target;
                    covered = event.relatedTarget === null && target instanceof Element && !navigator.contains(target) && hidden(target)
                        ? target
                        : undefined;
                };
                const forget = () => { covered = undefined; };
                const measure = () => {
                    // Layout can blur content that the fullscreen stylesheet hides.
                    const active = document.activeElement;
                    const blurred = covered;
                    covered = undefined;
                    const width = row.getBoundingClientRect().width;
                    const fills = closable && (width < FILL_BELOW || viewport?.matches === true);
                    const entering = fills && !filling;
                    filling = fills;
                    pane.width.set(width);
                    answerDrawn(pane);
                    const inContent = active !== null && row.contains(active) && !navigator.contains(active);
                    const blurredByFill = blurred !== undefined && (active === null || active === document.body)
                        && row.contains(blurred) && hidden(blurred);
                    if (entering && store.getSnapshot().byPane[paneId]?.open === true
                        && (inContent || blurredByFill) && pane.focus === undefined)
                        focusTree(pane);
                    // React removes the edge in the commit it flushes in a microtask
                    // queued before this one, so the check sees the new drawing.
                    if (edge !== undefined)
                        queueMicrotask(handOff);
                };
                // On the document, so the row's own handlers see the key first and can keep it.
                const close = (event) => {
                    if (event.key !== 'Escape' || event.defaultPrevented || event.isComposing
                        || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey)
                        return;
                    if (!(event.target instanceof Node) || !row.contains(event.target))
                        return;
                    event.preventDefault();
                    pane.toggle?.focus();
                    actions.shown(paneId, false);
                };
                measure();
                const observer = new ResizeObserver(measure);
                observer.observe(row);
                viewport?.addEventListener('change', measure);
                if (closable) {
                    document.addEventListener('keydown', close);
                    row.addEventListener('focusout', release);
                    document.addEventListener('focusin', forget, true);
                    document.addEventListener('pointerdown', forget, true);
                    window.addEventListener('blur', forget);
                }
                navigator.addEventListener('focusin', focusin);
                navigator.addEventListener('focusout', focusout);
                pane.unobserve = () => {
                    observer.disconnect();
                    viewport?.removeEventListener('change', measure);
                    document.removeEventListener('keydown', close);
                    row.removeEventListener('focusout', release);
                    document.removeEventListener('focusin', forget, true);
                    document.removeEventListener('pointerdown', forget, true);
                    window.removeEventListener('blur', forget);
                    navigator.removeEventListener('focusin', focusin);
                    navigator.removeEventListener('focusout', focusout);
                };
            },
            toggleDrawn(paneId, toggle) {
                if (toggle !== null) {
                    record(paneId).toggle = toggle;
                    return;
                }
                const pane = panes.get(paneId);
                if (pane !== undefined)
                    pane.toggle = undefined;
                drop(paneId);
            },
            hooks: { canOpen },
            keyedHooks: { rowWidth: paneId => record(paneId).width },
        };
        return face;
    };
}
//# sourceMappingURL=face.js.map