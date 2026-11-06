import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
import clsx from 'clsx';
import { ReferenceIconRegular } from "./ReferenceIcon.js";
import css from './user-text.module.css';
import markdownCss from './markdown/MarkdownText.module.css';
/**
 * Render an uninterpreted inline text run.
 * @param props - text retained verbatim from the caller's document.
 * @returns one inline run without Markdown or reference recognition.
 */
export function UserTextRun({ text }) {
    return _jsx("span", { className: css.plainRun, children: text });
}
/**
 * Render a reference label or its explicitly supplied activation action.
 * Text selection and repeated pointer clicks do not activate the reference.
 * @param props - resolved visual fields and optional action from the owning renderer.
 * @returns an inline label, or a keyboard-accessible button when activation is supplied.
 */
export function ReferenceChip({ appearance, label, title, onActivate, contextMenuProps, }) {
    const slash = appearance === 'skill' || appearance === 'command';
    const contents = _jsxs(_Fragment, { children: [!slash && _jsx(ReferenceIconRegular, { kind: appearance, size: 16, className: css.refIcon }), label] });
    const className = clsx(css.refChip, slash && css.slashChip);
    return onActivate === undefined
        ? _jsx("span", { ...contextMenuProps, className: className, "data-ref-chip": appearance, title: title, children: contents })
        : _jsx("button", { type: "button", ...contextMenuProps, className: clsx(className, markdownCss.fileMention), "data-ref-chip": appearance, title: title, onClick: (event) => {
                if (event.detail > 1 || (event.detail !== 0 && event.currentTarget.ownerDocument.getSelection()?.isCollapsed === false))
                    return;
                onActivate();
            }, children: contents });
}
//# sourceMappingURL=user-text.js.map