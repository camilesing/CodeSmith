import { jsx as _jsx } from "react/jsx-runtime";
/**
 * The navigator's show/hide control, at the end of a document preview's
 * header through its Factory; the `files` page's tree is always
 * shown and has none. It writes the pane's choice, which every file tab of
 * the pane reads, and registers itself with the face, which hands it the
 * keyboard when Escape closes the navigator.
 */
import { useCallback } from 'react';
import { IconFolderCloseRegular, IconFolderOpenRegular, Tooltip } from '@codesmith/client-ui-primitives';
import css from './FilesBody.module.css';
/**
 * Show or hide the pane's navigator.
 * @param props - the tab, the pane records, the face's registration, and copy.
 * @returns the toggle, or nothing without a workspace.
 */
export function NavigatorToggle({ useTabInfo, sessionId, useSessions, useStore, actions, toggleDrawn, t }) {
    const { panel } = useTabInfo();
    const cwd = useSessions(sessions => sessions.byId[sessionId]?.cwd);
    const open = useStore(store => store.byPane[panel.id]?.open) === true;
    // Stable, so the control registers once per drawing rather than on every render.
    const toggleRef = useCallback((button) => { toggleDrawn(panel.id, button); }, [toggleDrawn, panel.id]);
    if (cwd === undefined)
        return null;
    const label = t(open ? 'navigator.hide' : 'navigator.show');
    return (_jsx(Tooltip, { label: label, side: "bottom", delayMs: 500, children: _jsx("button", { ref: toggleRef, type: "button", className: css.tool, "aria-label": label, "aria-expanded": open, "data-files-navigator-toggle": true, onClick: () => { actions.shown(panel.id, !open); }, children: open ? _jsx(IconFolderOpenRegular, {}) : _jsx(IconFolderCloseRegular, {}) }) }));
}
//# sourceMappingURL=NavigatorToggle.js.map