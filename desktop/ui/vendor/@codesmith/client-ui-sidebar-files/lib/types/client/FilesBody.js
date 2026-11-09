import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/**
 * The `files` page: a file browser with no file chosen yet.
 *
 * Its header spans the page: the workspace root's path, the tree's reload and
 * the directory actions, in the order a preview's header ends with its own
 * reload and file actions. The pane's navigator fills the page below it and
 * stays shown, so the page draws no toggle; picking a file leaves this page as
 * it is and opens the file where the right Sidebar places it by default
 * (`FileNavigator`).
 */
import { useCallback, useEffect } from 'react';
import { IconPauseOutlineRegular, IconPlayOutlineRegular, IconRefreshOutlineRegular, PathLabel, Tooltip, } from '@codesmith/client-ui-primitives';
import css from './FilesBody.module.css';
/**
 * The `files` page body.
 * @param props - composed slot props.
 * @returns the header above the pane's navigator, or the workspace line.
 */
export function FilesBody(props) {
    const { useTabInfo, sessionId, useSessions, useStore, refresh, setAutoRefresh, renderSlot, renderFactorySlot, t, useContextMenu } = props;
    const { tab, panel } = useTabInfo();
    const { getTriggerProps } = useContextMenu();
    const onOpen = useCallback((address) => {
        if (panel.isFloating)
            tab.actions.openResource(address);
        else
            tab.actions.openResource(address, { paneId: panel.id });
    }, [tab.actions, panel.id, panel.isFloating]);
    useEffect(() => tab.actions.bindCommands({ refresh: () => { refresh(panel.id); } }), [tab.actions, panel.id, refresh]);
    const cwd = useSessions(sessions => sessions.byId[sessionId]?.cwd);
    // The setting belongs to a listed tree, so there is nothing to set before one.
    const autoRefresh = useStore((store) => {
        const tree = store.byPane[panel.id]?.tree;
        return tree?.root === cwd ? tree?.autoRefresh : undefined;
    });
    if (cwd === undefined) {
        return (_jsx("div", { className: css.status, "data-files-state": "no-workspace", children: _jsx("p", { className: css.statusLine, children: t('noWorkspace') }) }));
    }
    const automatic = autoRefresh ?? true;
    const navigator = renderFactorySlot('sidebar.files.navigator', { useTabInfo, page: true, onOpen });
    return (_jsxs("div", { className: css.page, "data-files-page": true, children: [_jsxs("div", { className: css.header, children: [_jsx(PathLabel, { path: cwd, className: css.path, "data-files-path": true, ...getTriggerProps({ kind: 'directory', value: { sessionId, path: cwd } }) }), _jsx("span", { hidden: true, children: _jsx("button", { type: "button", className: css.tool, "aria-label": t('autoRefresh'), "aria-pressed": automatic, disabled: autoRefresh === undefined, "data-files-auto-refresh": true, title: t(automatic ? 'autoRefresh.disable' : 'autoRefresh.enable'), onClick: () => { setAutoRefresh(panel.id, !automatic); }, children: automatic ? _jsx(IconPauseOutlineRegular, {}) : _jsx(IconPlayOutlineRegular, {}) }) }), _jsx(Tooltip, { label: t('reload'), shortcutKeys: tab.refreshShortcut?.keys, side: "bottom", delayMs: 500, children: _jsx("button", { type: "button", className: css.tool, "aria-label": t('reload'), "aria-keyshortcuts": tab.refreshShortcut?.aria, "data-files-reload": true, onClick: () => { refresh(panel.id); }, children: _jsx(IconRefreshOutlineRegular, {}) }) }), renderSlot('sidebar.right.tab.files.actions', { absolutePath: cwd })] }), _jsx("div", { className: css.content, "data-files-content": true, children: navigator })] }));
}
//# sourceMappingURL=FilesBody.js.map