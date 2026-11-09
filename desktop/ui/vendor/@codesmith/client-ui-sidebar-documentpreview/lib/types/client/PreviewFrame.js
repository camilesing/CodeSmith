import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/**
 * The text preview's layout: an optional banner, the path header, and the
 * content row, with the workspace navigator Factories in fixed positions.
 *
 * Every preview state renders this one Component, so the navigator, its
 * header toggle, and the file handoff keep their DOM nodes, focus, and scroll position while the
 * preview moves between unsupported, loading, and text. The two Factory
 * positions are memoized Components whose props are only the tab hook and
 * `renderFactorySlot`; a preview-only update, such as a recorded scroll,
 * does not re-render them.
 */
import { memo, useCallback } from 'react';
import { PathLabel } from '@codesmith/client-ui-primitives';
import css from './TextPreview.module.css';
/** The workspace navigator after the preview's content. */
const NavigatorSeat = memo(function NavigatorSeat({ useTabInfo, renderFactorySlot }) {
    const { tab } = useTabInfo();
    const onOpen = useCallback((address) => {
        tab.actions.openResource(address, { replaceTab: true });
    }, [tab.actions]);
    return renderFactorySlot('sidebar.files.navigator', { useTabInfo, page: false, onOpen });
});
/** The navigator's toggle at the header's end. */
const NavigatorToggleSeat = memo(function NavigatorToggleSeat({ useTabInfo, renderFactorySlot }) {
    return renderFactorySlot('sidebar.files.navigator.toggle', { useTabInfo });
});
/**
 * The text preview's layout for every preview state.
 * @param props - the header and content parts, the navigator inputs, and the root's data attributes.
 * @returns the preview root.
 */
export function PreviewFrame({ path, pathTrigger, banner, tools, handoff, children, useTabInfo, renderFactorySlot, ...data }) {
    return (_jsxs("div", { className: css.preview, ...data, children: [banner, _jsxs("div", { className: css.header, children: [_jsx(PathLabel, { path: path, className: css.path, "data-textpreview-path": true, ...pathTrigger }), tools, handoff, _jsx(NavigatorToggleSeat, { useTabInfo: useTabInfo, renderFactorySlot: renderFactorySlot })] }), _jsxs("div", { className: css.content, "data-textpreview-content": true, children: [children, _jsx(NavigatorSeat, { useTabInfo: useTabInfo, renderFactorySlot: renderFactorySlot })] })] }));
}
//# sourceMappingURL=PreviewFrame.js.map