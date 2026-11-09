import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { IconCloseFillRegular, IconRefreshOutlineRegular, IconWarningOutlineRegular, Tooltip, } from '@codesmith/client-ui-primitives';
import { AttachmentRail } from "../AttachmentRail.js";
import { DropOverlay } from "../DropOverlay.js";
import { FileCard } from "../FileCard.js";
import { ImageLightbox } from '@codesmith/client-ui-primitives';
import { attachmentRailLabels, dropOverlayLabels, fileCardLabels, lightboxLabels } from "./labels.js";
import { installDocumentDropEvents } from "./drop-events.js";
import css from './ComposerAttachments.module.css';
/** Draft image previews, pending-file cards, drop target, and original-image preview. */
export function ComposerAttachments({ attachments, canAcceptDrop, onAddFiles, onRemoveAttachment, uploads, onRetryFile, uploadFailureText, dropLimits, t, readFileDrag, }) {
    const [preview, setPreview] = useState(null);
    const [dragActive, setDragActive] = useState(false);
    const dragDepth = useRef(0);
    const closePreview = useCallback(() => { setPreview(null); }, []);
    useEffect(() => {
        if (preview !== null && !attachments.some(attachment => attachment.id === preview.id))
            setPreview(null);
    }, [attachments, preview]);
    useEffect(() => {
        return installDocumentDropEvents(canAcceptDrop, onAddFiles, dragDepth, setDragActive, readFileDrag);
    }, [canAcceptDrop, onAddFiles, readFileDrag]);
    const railItems = useMemo(() => attachments.map(attachment => ({
        id: attachment.id,
        attachment,
    })), [attachments]);
    return (_jsxs(_Fragment, { children: [dragActive && (_jsx(DropOverlay, { disabled: !canAcceptDrop, labels: dropOverlayLabels(t, canAcceptDrop, dropLimits) })), railItems.length > 0 && (_jsx("div", { className: css.rail, children: _jsx(AttachmentRail, { items: railItems, labels: attachmentRailLabels(t), renderItem: (item) => {
                        const attachment = item.attachment;
                        if (attachment.kind === 'file') {
                            const upload = uploads[attachment.id];
                            return (_jsx(FileCard, { name: attachment.file.name || t('file.label'), bytes: attachment.file.size, state: upload === undefined || upload.status === 'uploading'
                                    ? 'uploading'
                                    : upload.status === 'ready' ? 'ready' : 'error', ...upload?.status === 'uploading' && upload.total !== undefined && upload.total > 0
                                    ? { progress: upload.loaded / upload.total }
                                    : {}, labels: fileCardLabels(t, attachment.file.name), onRemove: () => { onRemoveAttachment(attachment.id); }, onRetry: () => { onRetryFile(attachment.id); } }));
                        }
                        // An uploading image shows its progress; a failed upload turns the thumbnail into its retry
                        // control, except one the Host refused, which explains why and leaves removal as the way out.
                        // The refusal is the only explanation of its warning, so a tap or click also pins it.
                        const upload = uploads[attachment.id];
                        const name = attachment.file.name || t('image.pending');
                        const refusal = upload?.status === 'error' && upload.reason !== undefined
                            ? uploadFailureText(upload.reason)
                            : undefined;
                        return (_jsxs("div", { className: css.imageItem, children: [refusal !== undefined
                                    ? (_jsx(Tooltip, { label: refusal, side: "top", delayMs: 300, portal: true, openOnClick: true, children: _jsxs("button", { type: "button", className: `${css.thumbnail} ${css.thumbnailFailed} ${css.thumbnailRefused}`, "aria-label": refusal, children: [_jsx("img", { src: attachment.previewUrl, alt: "" }), _jsx("span", { className: css.failureGlyph, "aria-hidden": true, children: _jsx(IconWarningOutlineRegular, { size: 16 }) })] }) }))
                                    : upload?.status === 'error'
                                        ? (_jsx(Tooltip, { label: t('file.uploadFailed'), side: "top", delayMs: 300, portal: true, children: _jsxs("button", { type: "button", className: `${css.thumbnail} ${css.thumbnailFailed}`, "aria-label": t('file.retry', { name }), onClick: () => { onRetryFile(attachment.id); }, children: [_jsx("img", { src: attachment.previewUrl, alt: "" }), _jsx("span", { className: css.failureGlyph, "aria-hidden": true, children: _jsx(IconRefreshOutlineRegular, { size: 16 }) })] }) }))
                                        : (_jsx("button", { type: "button", className: css.thumbnail, title: t('image.openOriginal'), "aria-busy": upload?.status === 'uploading', onClick: () => { setPreview(attachment); }, children: _jsx("img", { src: attachment.previewUrl, alt: name }) })), upload?.status === 'uploading' && (_jsx("span", { className: css.progressTrack, "aria-hidden": true, children: _jsx("span", { className: css.progressBar, style: upload.total === undefined || upload.total <= 0
                                            ? undefined
                                            : { width: `${String(Math.min(1, upload.loaded / upload.total) * 100)}%` } }) })), _jsx("button", { type: "button", className: css.remove, "aria-label": t('image.remove', { name }), onClick: () => { onRemoveAttachment(attachment.id); }, children: _jsx(IconCloseFillRegular, { size: 12 }) })] }));
                    } }) })), preview !== null && (_jsx(ImageLightbox, { src: preview.previewUrl, alt: preview.file.name || t('image.original'), labels: lightboxLabels(t), onClose: closePreview }))] }));
}
//# sourceMappingURL=ComposerAttachments.js.map