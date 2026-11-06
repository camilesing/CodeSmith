import { jsx as _jsx } from "react/jsx-runtime";
/** @param props - clicked tab and already retained view. @returns existing plugin menu rows. */
export function TabMenuContent({ target, dismiss, useViews, SessionProvider, renderSlot }) {
    const views = useViews(value => value);
    if (target.kind !== 'tab')
        return null;
    const view = views.find(view => view.sessionId === target.value.sessionId);
    const tab = target.value.tab;
    if (view === undefined)
        return null;
    return _jsx(SessionProvider, { session: view.reference, children: renderSlot('sidebar.right.tab.menu.item', { tab, dismiss }) });
}
//# sourceMappingURL=TabMenuContent.js.map