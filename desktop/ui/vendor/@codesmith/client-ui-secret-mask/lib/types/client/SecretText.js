import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/** Masked credential presentation with a per-value reveal control in interactive text. */
import { useState } from 'react';
import { MASK } from "./credential-masks.js";
import css from './SecretText.module.css';
const EYE_CONTOUR = 'M1.5 8C2.9 5.25 5.25 3.5 8 3.5C10.75 3.5 13.1 5.25 14.5 8C13.1 10.75 10.75 12.5 8 12.5C5.25 12.5 2.9 10.75 1.5 8Z';
const EYE_PATH = `${EYE_CONTOUR}M10 8C10 9.1 9.1 10 8 10C6.9 10 6 9.1 6 8C6 6.9 6.9 6 8 6C9.1 6 10 6.9 10 8Z`;
/** The struck-through glyph drops the pupil so it stays distinct at text size. */
const EYE_OFF_PATH = `${EYE_CONTOUR}M2.5 2.5L13.5 13.5`;
const MASK_RUNS = new RegExp(`(\\${MASK}+)`, 'u');
function EyeGlyph({ path }) {
    return (_jsx("svg", { className: css.icon, viewBox: "0 0 16 16", fill: "none", stroke: "currentColor", strokeWidth: 1.3, strokeLinecap: "round", "aria-hidden": "true", children: _jsx("path", { d: path }) }));
}
/**
 * Split the masked form into kept text and mask runs. Mask runs keep their
 * characters for copying and assistive technology but draw centered marks,
 * because a font's own asterisk sits near the cap height.
 */
function MaskedText({ masked }) {
    let offset = 0;
    return masked.split(MASK_RUNS).map((part) => {
        const key = offset;
        offset += part.length;
        if (part === '')
            return null;
        return part.startsWith(MASK)
            ? _jsx("span", { className: css.mask, children: part }, key)
            : _jsx("span", { children: part }, key);
    });
}
/**
 * Draw one credential masked; interactive text adds a control that reveals or hides this value.
 * Every mount, and every different original value at the same position, starts masked.
 * @param props - masked node, original spelling, interaction mode, and locale seat.
 * @returns an inline masked run with an optional reveal control.
 */
export function SecretText({ node, raw, interactive, t }) {
    // Holding the revealed value instead of a flag re-masks a different credential that reuses this element.
    const [revealedRaw, setRevealedRaw] = useState();
    const revealed = interactive && revealedRaw === raw;
    return (_jsxs("span", { className: css.secret, "data-secret": "", children: [_jsx("span", { className: css.value, children: revealed ? raw : _jsx(MaskedText, { masked: node.data.masked }) }), interactive && (_jsx("button", { type: "button", className: css.toggle, "aria-pressed": revealed, "aria-label": t(revealed ? 'secret.hide' : 'secret.reveal'), onClick: () => { setRevealedRaw(revealed ? undefined : raw); }, children: _jsx(EyeGlyph, { path: revealed ? EYE_OFF_PATH : EYE_PATH }) }))] }));
}
//# sourceMappingURL=SecretText.js.map