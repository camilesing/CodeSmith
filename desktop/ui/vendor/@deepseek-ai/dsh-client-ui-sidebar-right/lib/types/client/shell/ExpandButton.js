import { jsx as _jsx } from "react/jsx-runtime";
import clsx from 'clsx';
import { Button, IconPanelLeftOutlineRegular, isMobileDevice, Tooltip } from '@deepseek-ai/dsh-client-ui-primitives';
import css from './ExpandButton.module.css';
/** Opens a collapsed panel; when expanded, hides and disables on mobile devices and renders nothing on desktop devices. */
export function ExpandButton({ sessionId, useStore, actions, t, useShortcuts }) {
    // A session with no surface yet is collapsed: the panel seat materializes the
    // surface on its own mount, and until then there is nothing expanded.
    const shortcut = useShortcuts(entries => entries.find(entry => entry.id === 'sidebar.right.toggle'));
    const expanded = useStore(state => state.bySession[sessionId]?.layout.expanded ?? false);
    if (expanded && !isMobileDevice())
        return null;
    return (_jsx(Tooltip, { label: t('chrome.expand'), shortcutKeys: shortcut?.keys, side: "bottom", delayMs: 500, disabled: expanded, children: _jsx(Button, { size: "sm", className: clsx(css.button, expanded && css.expanded), disabled: expanded, "aria-label": t('chrome.expandAria'), "aria-keyshortcuts": shortcut?.aria, "data-sidebar-right-expand": true, onClick: () => { actions.setExpanded(sessionId, true); }, children: _jsx(IconPanelLeftOutlineRegular, { className: css.icon }) }) }));
}
//# sourceMappingURL=ExpandButton.js.map