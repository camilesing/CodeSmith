import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
/** Shared Web renderer for Client menu contributions. */
import { Menu, Toast } from '@deepseek-ai/dsh-client-ui-primitives';
/**
 * @param actions - plain presentation.
 * @param catalog - effective user shortcuts.
 * @param renderSlot - owner-authorized keyed icon renderer.
 * @returns Existing Menu entries.
 */
export function menuEntries(actions, catalog, renderSlot) {
    return actions.flatMap((action) => {
        const shortcut = catalog.find(entry => entry.id === action.commandId);
        const item = { id: action.id, label: action.label,
            ...(shortcut === undefined ? {} : { shortcut }),
            ...(action.disabled === undefined ? {} : { disabled: action.disabled }),
            ...(action.danger === undefined ? {} : { danger: action.danger }),
            ...(action.icon === undefined ? {} : {
                icon: renderSlot('contextMenu.icon', { icon: action.icon }, { entryKey: action.icon.key }),
            }),
            ...(action.children === undefined ? {} : { submenu: menuEntries(action.children, catalog, renderSlot) }),
        };
        return action.separatorBefore ? [{ type: 'separator', id: `${action.id}:separator` }, item] : [item];
    });
}
/** @param props - registered state, target-specific legacy rows, and actions. @returns one menu and persistent feedback. */
export function MenuOverlay({ useStore, actions, renderSlot, close, select, returnFocus, useCatalog }) {
    const catalog = useCatalog(value => value);
    const { target, point, items, notice, occurrence, closing, replacing } = useStore(s => s);
    return _jsxs(_Fragment, { children: [_jsx(Menu, { open: target !== null, anchor: null, portal: true, autoFocus: true, contextual: true, returnFocus: returnFocus, exiting: closing ? replacing ? 'replace' : 'dismiss' : undefined, getAnchorRect: () => new DOMRect(point.x, point.y, 0, 0), items: menuEntries(items.filter(item => !item.afterContent), catalog, renderSlot), footer: menuEntries(items.filter(item => item.afterContent), catalog, renderSlot), onClose: close, onSelect: select, children: target !== null && renderSlot('contextMenu.content', { target, dismiss: close }, { entryKey: target.kind }) }, occurrence), notice !== null && _jsx(Toast, { text: notice.text, ...notice.action === undefined ? {} : { actions: [notice.action] }, ...notice.success ? { tone: 'success' } : {}, onDone: () => { actions.notify(null); } }, notice.id)] });
}
//# sourceMappingURL=View.js.map