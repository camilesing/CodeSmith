import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/** Floating controls shared by every zoomable document renderer. */
import clsx from 'clsx';
import { forwardRef, useEffect, useImperativeHandle, useState } from 'react';
import { Button } from "./Button.js";
import { IconChevronDownOutlineRegular, IconPlusOutlineRegular } from "./icons/index.js";
import { Menu } from "./Menu.js";
import { Tooltip } from "./Tooltip.js";
import { DOCUMENT_ZOOM } from "./document-zoom.js";
import css from './ZoomControls.module.css';
function steppedZoom(zoom, direction) {
    const step = direction === -1 ? Math.ceil(zoom / DOCUMENT_ZOOM.step) - 1 : Math.floor(zoom / DOCUMENT_ZOOM.step) + 1;
    return Math.min(DOCUMENT_ZOOM.maximum, Math.max(DOCUMENT_ZOOM.minimum, step * DOCUMENT_ZOOM.step));
}
/**
 * Present optional fitting, fixed presets, and incremental controls.
 * @param props - committed `zoom`, optional `fit` row (omitted: no fitting row), localized labels, `visible`, optional
 *   owner `className`, and zoom and activity callbacks.
 * @param ref - `showZoom` displays a transient scale until `zoom` changes.
 * @returns a localized zoom toolbar.
 */
export const ZoomControls = forwardRef(function ZoomControls({ zoom, fit, className, labels, visible, onZoom, onActiveChange, }, ref) {
    const [committed, setCommitted] = useState(zoom);
    const [transient, setTransient] = useState();
    const [open, setOpen] = useState(false);
    const [hovered, setHovered] = useState(false);
    const [focused, setFocused] = useState(false);
    if (committed !== zoom) {
        setCommitted(zoom);
        setTransient(undefined);
    }
    const displayZoom = transient ?? zoom;
    useImperativeHandle(ref, () => ({ showZoom: setTransient }), []);
    useEffect(() => { onActiveChange(open || hovered || focused); }, [focused, hovered, onActiveChange, open]);
    const items = [
        ...(fit === undefined ? [] : [{ id: 'fit', label: fit.label }, { type: 'separator', id: 'fit-scale' }]),
        ...DOCUMENT_ZOOM.presets.map(value => ({ id: String(value), label: labels.value(value * 100) })),
    ];
    const selectedId = fit?.selected === true ? 'fit'
        : DOCUMENT_ZOOM.presets.some(value => value === displayZoom) ? String(displayZoom) : undefined;
    return _jsxs("div", { className: clsx(css.panel, visible && css.visible, className), role: "toolbar", "aria-label": labels.controls, "data-document-zoom-controls": true, "data-document-zoom-visible": visible || undefined, onPointerEnter: (event) => { if (event.pointerType !== 'touch')
            setHovered(true); }, onPointerLeave: () => { setHovered(false); }, onFocusCapture: () => { setFocused(true); }, onBlurCapture: (event) => {
            if (!event.currentTarget.contains(event.relatedTarget))
                setFocused(false);
        }, children: [_jsx(Tooltip, { label: labels.out, side: "top", children: _jsx("span", { className: css.controlAnchor, children: _jsx(Button, { size: "sm", className: css.zoomButton, "aria-label": labels.out, disabled: displayZoom <= DOCUMENT_ZOOM.minimum, onMouseDown: (event) => { event.preventDefault(); }, onClick: () => { onZoom(steppedZoom(displayZoom, -1)); }, children: _jsx("svg", { width: "14", height: "14", viewBox: "0 0 16 16", "aria-hidden": "true", children: _jsx("path", { d: "M2 8h12", fill: "none", stroke: "currentColor", strokeWidth: "1.5" }) }) }) }) }), _jsx(Menu, { open: open, side: "top", align: "start", portal: true, compact: true, listClassName: css.zoomMenu, anchor: _jsxs(Button, { size: "sm", className: css.percent, "aria-label": labels.menu, "aria-expanded": open, onMouseDown: (event) => { event.preventDefault(); }, onClick: () => { setOpen(value => !value); }, children: [_jsx("span", { children: labels.value(Math.round(displayZoom * 100)) }), _jsx(IconChevronDownOutlineRegular, { size: 12 })] }), items: items, selectedId: selectedId, onClose: () => { setOpen(false); }, onSelect: (selected) => {
                    setOpen(false);
                    if (selected === 'fit')
                        fit?.onSelect();
                    else
                        onZoom(Number(selected));
                } }), _jsx(Tooltip, { label: labels.into, side: "top", children: _jsx("span", { className: css.controlAnchor, children: _jsx(Button, { size: "sm", className: css.zoomButton, "aria-label": labels.into, disabled: displayZoom >= DOCUMENT_ZOOM.maximum, onMouseDown: (event) => { event.preventDefault(); }, onClick: () => { onZoom(steppedZoom(displayZoom, 1)); }, children: _jsx(IconPlusOutlineRegular, { size: 14 }) }) }) })] });
});
//# sourceMappingURL=ZoomControls.js.map