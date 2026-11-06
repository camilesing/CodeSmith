import { jsx as _jsx, Fragment as _Fragment } from "react/jsx-runtime";
/** Root-scoped controller for the right Sidebar's Session content. */
import { useLayoutEffect } from 'react';
import css from './SidebarRight.module.css';
function SessionView({ view, visible, SessionProvider, renderSlot, mountView, width, viewportWidth, canShow }) {
    useLayoutEffect(() => mountView(view.reference), [mountView, view.reference]);
    const active = visible && view.selected;
    return _jsx("div", { className: css.session, hidden: !active, "data-sidebar-right-session": view.sessionId, children: _jsx(SessionProvider, { session: view.reference, children: renderSlot('rightbar.session', { width, viewportWidth, canShow, active, retainTab: view.retainTab }) }) });
}
/**
 * Keep independent Session subtrees and hide those outside the selected Conversation.
 * @param props - frame geometry, view targets and the authorized Session renderer.
 * @returns the foreground and retained background Sidebars.
 */
export function RightbarRoot({ usePanelInfo, useViews, ...props }) {
    const visible = usePanelInfo(info => info.activePanelId === null);
    const views = useViews(value => value);
    return _jsx(_Fragment, { children: views.map(view => _jsx(SessionView, { ...props, view: view, visible: visible }, view.sessionId)) });
}
//# sourceMappingURL=RightbarRoot.js.map