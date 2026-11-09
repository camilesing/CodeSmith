import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { resolveWorkspacePath } from '@codesmith/util-workspace-path';
import { FileTypeIcon, fileExtension } from '@codesmith/client-ui-primitives';
import { basename } from "./turn-deliverables.js";
import css from './Deliverables.module.css';
function cardDescription(description, fallback) {
    const trimmed = description?.replace(/\s*(?:\([^()]*\)|（[^（）]*）)\s*$/u, '').trim();
    return trimmed === undefined || trimmed === '' ? fallback : trimmed;
}
/**
 * Render independent file actions without nesting buttons inside a clickable card.
 * @param props - durable file metadata, Sidebar preview, Host capabilities, gesture status, and localized copy.
 * @returns the file card and its anchored action menu.
 */
export function PresentedFileCard({ file, cwd, phase, host, onPreview, contextMenuProps, actions, t }) {
    const reveal = host?.fileManager ?? 'directory';
    const name = basename(file.path);
    const metadata = fileExtension(name).toUpperCase() || t('presented.file');
    const status = phase === undefined
        ? cardDescription(file.description, metadata)
        : t(reveal === 'directory' && phase === 'revealing' ? 'presented.directoryOpening'
            : reveal === 'directory' && phase === 'revealError' ? 'presented.directoryError' : `presented.${phase}`);
    return _jsxs("div", { className: css.file, "data-presented-file": true, children: [_jsx("button", { type: "button", className: css.cardPreview, title: resolveWorkspacePath(cwd, file.path), "aria-label": t('presented.previewCard', { name: file.path }), onClick: onPreview, ...contextMenuProps }), _jsx("span", { className: css.fileIcon, children: _jsx(FileTypeIcon, { path: file.path, size: 20 }) }), _jsxs("div", { className: css.fileBody, children: [_jsxs("div", { className: css.details, children: [_jsx("span", { className: css.fileName, children: name }), _jsxs("span", { className: css.description, "data-presented-description": true, role: phase === undefined ? undefined : 'status', "data-error": phase === 'error' || phase === 'revealError' || phase === 'nativeUnavailable' ? true : undefined, children: [_jsx("span", { className: css.secondaryText, children: status }), _jsx("span", { className: css.previewHint, children: t('presented.preview') })] })] }), _jsx("div", { className: css.actions, children: actions })] })] });
}
//# sourceMappingURL=PresentedFileCard.js.map