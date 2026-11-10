import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
/** File navigation feedback belongs to the individual inline reference that requested it. */
import { useState } from 'react';
import { Button, Modal, ReferenceChip } from '@codesmith/client-ui-primitives';
/**
 * Render a file chip and contain synchronous navigation failures without hiding the reference.
 * @param props - JSON node, interaction policy, Session-authorized navigation, and locale seat.
 * @returns an inline chip and, after a navigation failure, its body-portaled dialog.
 */
export function FileReference({ node, raw, interactive, openFile, resolveFilePath, sessionId, useContextMenu, t }) {
    const { getTriggerProps } = useContextMenu();
    const [failedPath, setFailedPath] = useState(null);
    const { path, label } = node.data;
    const close = () => { setFailedPath(null); };
    const open = () => {
        try {
            openFile(path);
        }
        catch {
            setFailedPath(path);
            return;
        }
        setFailedPath(null);
    };
    return _jsxs(_Fragment, { children: [_jsx(ReferenceChip, { appearance: "file", label: label, title: raw, contextMenuProps: getTriggerProps(interactive ? () => ({ kind: 'file', value: { sessionId, path: resolveFilePath(path) } }) : undefined), ...interactive ? { onActivate: open } : {} }), interactive && failedPath === path && _jsx(Modal, { open: true, onClose: close, closeLabel: t('fileOpen.close'), title: t('fileOpen.title'), description: t('fileOpen.failed', { path }), footer: _jsxs(_Fragment, { children: [_jsx(Button, { variant: "outline", onClick: close, children: t('fileOpen.close') }), _jsx(Button, { variant: "primary", onClick: open, children: t('fileOpen.retry') })] }) })] });
}
/**
 * Render a directory as a label, even when the containing text permits file navigation.
 * @param props - resolved directory data and the original reference spelling.
 * @returns an inline non-interactive chip.
 */
export function FolderReference({ node, raw }) {
    return _jsx(ReferenceChip, { appearance: "folder", label: node.data.label, title: raw });
}
//# sourceMappingURL=FileReference.js.map